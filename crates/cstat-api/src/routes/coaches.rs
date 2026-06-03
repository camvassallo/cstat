use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/coaches", get(coach_leaderboard))
        .route("/api/coaches/{id}", get(coach_detail))
}

#[derive(Deserialize)]
struct LeaderboardParams {
    /// `career` (default) — ranks by career EB-shrunk CAE, `season` scopes the
    /// list. `season` — ranks by the selected year's single-season raw CAE.
    mode: Option<String>,
    /// Minimum scored seasons to qualify (career mode only). Default 3 — thin
    /// tenures shrink toward 0 and would otherwise top the board on noise.
    min_seasons: Option<i32>,
    /// Page size. Default 200, clamped to [1, 1000]. The frontend requests the
    /// full board (career is season-agnostic, ~690 coaches all-time) so the
    /// Blend z-score population and sort cover every qualified coach.
    limit: Option<i64>,
    /// Career mode: scope the list to coaches who coached this season (rating
    /// stays career-aggregated); omit for all-time. Season mode: which year's
    /// single-season board to rank (defaults to the current season).
    season: Option<i32>,
}

/// `GET /api/coaches` — Coach-Above-Expectation leaderboard.
///
/// Career mode (default): sorted by the headline `cae_shrunk` (raw EB-shrunk)
/// descending, reading only `coach_ratings` plus a team lookup; `season` filters
/// the list to that year's coaches without changing the rating. Season mode
/// (`mode=season`): re-ranks by the selected year's single-season `cae_raw` —
/// noisier, framed as a "who overachieved this year" view. The
/// `available_seasons` field carries the CAE coverage so the frontend can
/// constrain the navbar season picker. No team-detail or projection work.
async fn coach_leaderboard(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LeaderboardParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = params.limit.unwrap_or(200).clamp(1, 1000);
    let pool = &state.db.pool;

    if params.mode.as_deref() == Some("season") {
        let season = params.season.unwrap_or_else(crate::default_season);
        let (mut coaches, available_seasons) = tokio::try_join!(
            queries::get_coach_season_leaderboard(pool, season, limit),
            queries::get_coach_cae_seasons(pool),
        )
        .map_err(internal_error)?;

        // Display-only single-season "results + overperformance" lens — z(CAE) +
        // z(AdjEM) over this season's board. Never an input to forecasts. Skip it
        // on a truncated page (a full page may be cut by `limit`, which would
        // z-score a biased slice); the season-scoped frontend is always well
        // under the cap, so it always blends.
        if (coaches.len() as i64) < limit {
            queries::apply_season_blend(&mut coaches);
        }

        return Ok(Json(json!({
            "mode": "season",
            "season": season,
            "available_seasons": available_seasons,
            "coaches": coaches,
        })));
    }

    let min_seasons = params.min_seasons.unwrap_or(3).max(1);
    let (mut coaches, available_seasons) = tokio::try_join!(
        queries::get_coach_leaderboard(pool, min_seasons, limit, params.season),
        queries::get_coach_cae_seasons(pool),
    )
    .map_err(internal_error)?;

    // Display-only "results + overperformance" lens — z(CAE) + z(career AdjEM)
    // over this qualified population. Computed here, never an input to forecasts.
    // Skip on a truncated page: blend z-scores are only meaningful over the
    // COMPLETE board, and a full page may have been cut by `limit`. The frontend
    // requests the whole board (limit 1000 vs ~690 all-time coaches), so it
    // always blends.
    if (coaches.len() as i64) < limit {
        queries::apply_career_blend(&mut coaches);
    }

    Ok(Json(json!({
        "mode": "career",
        "min_seasons": min_seasons,
        "season": params.season,
        "available_seasons": available_seasons,
        "coaches": coaches,
    })))
}

/// `GET /api/coaches/{id}` — one coach's career rating + per-season CAE rows
/// (the sparkline). The season rows carry the stored `actual_adjem` /
/// `projection`, so nothing is re-projected here.
async fn coach_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let pool = &state.db.pool;
    let (name, rating, seasons) = tokio::try_join!(
        queries::get_coach_name(pool, id),
        queries::get_coach_rating(pool, id),
        queries::get_coach_seasons(pool, id),
    )
    .map_err(internal_error)?;

    // Unknown id ⇔ not in `coaches`. (A real coach can legitimately have no
    // rating and only ungraded seasons — that page still renders.)
    if name.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "coach not found" })),
        ));
    }

    Ok(Json(json!({
        "name": name,
        "rating": rating,
        "seasons": seasons,
    })))
}

fn internal_error(e: sqlx::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": format!("query failed: {e}") })),
    )
}
