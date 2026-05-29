//! Score-ticker endpoint. Returns a paired list of recently-completed games
//! and soonest-upcoming games for the global header strip. Upcoming games
//! get a per-matchup margin + win-probability prediction from the predict
//! pipeline so the tile can show "Duke vs UNC · −3.5 (62% Duke)".
//!
//! Offseason behavior: `upcoming` is empty until next season's schedule is
//! ingested via NatStat. The frontend collapses that half cleanly.

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;
use crate::routes::predict::predict_projection;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/ticker", get(ticker))
}

#[derive(Deserialize)]
struct TickerParams {
    season: Option<i32>,
    /// Number of recent completed games to surface. Defaults to 8; capped at 24.
    past: Option<i64>,
    /// Number of upcoming games to surface. Defaults to 8; capped at 24.
    future: Option<i64>,
}

#[derive(Serialize)]
struct UpcomingTile {
    #[serde(flatten)]
    game: queries::GameResult,
    /// Predicted home-vs-away margin, in points. Positive = home favored.
    predicted_margin: f32,
    /// Probability the home team wins, derived from `predicted_margin`.
    home_win_probability: f64,
    /// Projected integer scores (rounded so they reconcile with margin).
    predicted_home_score: i32,
    predicted_away_score: i32,
}

async fn ticker(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TickerParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let past_limit = params.past.unwrap_or(8).clamp(1, 24);
    let future_limit = params.future.unwrap_or(8).clamp(1, 24);

    let (past, upcoming_raw) = tokio::try_join!(
        queries::get_recent_games(&state.db.pool, season, past_limit),
        queries::get_upcoming_games(&state.db.pool, season, future_limit),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("ticker query failed: {e}") })),
        )
    })?;

    // Per-game prediction for each upcoming row. Sequential — typical N is 8
    // and each call is sub-ms (Postgres feature fetch + ONNX inference), so
    // adding the `futures` dep just for `join_all` isn't worth it. Skip rows
    // with missing team UUIDs; they can't be fed into the predictor. If a
    // single matchup fails to predict (e.g. a team with no prior-season
    // stats early in the year), drop that tile rather than failing the whole
    // ticker response.
    let mut upcoming: Vec<UpcomingTile> = Vec::with_capacity(upcoming_raw.len());
    for g in upcoming_raw {
        let (home_id, away_id) = match (g.home_team_id, g.away_team_id) {
            (Some(h), Some(a)) => (h, a),
            _ => continue,
        };
        let is_conference = g.is_conference.unwrap_or(false);
        if let Ok(proj) = predict_projection(
            &state,
            home_id,
            away_id,
            season,
            g.is_neutral_site,
            is_conference,
            // Ticker shows only upcoming games — `as_of_date = None` is the
            // right cutoff (the only honest cutoff for an unplayed game is
            // "today", which is what `None` already gives).
            None,
        )
        .await
        {
            upcoming.push(UpcomingTile {
                game: g,
                predicted_margin: proj.margin,
                home_win_probability: proj.home_win_prob,
                predicted_home_score: proj.home_score,
                predicted_away_score: proj.away_score,
            });
        }
    }

    Ok(Json(json!({
        "season": season,
        "past": past,
        "upcoming": upcoming,
    })))
}
