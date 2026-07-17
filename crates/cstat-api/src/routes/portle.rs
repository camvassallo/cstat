use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use chrono::{NaiveDate, Utc};
use cstat_core::queries::{self, PortleMode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/portle/daily", get(daily_puzzle))
}

#[derive(Deserialize)]
struct DailyParams {
    mode: Option<String>,
    season: Option<i32>,
    date: Option<String>,
}

/// Server-authoritative daily answer (issue #181). Pins one player per
/// (mode, season, date) on first request and freezes it, so every client fetches
/// the identical puzzle and it never moves once set. Returns just the stable
/// `natstat_id`; the client resolves it against the player pool it already loaded
/// (the pin is guaranteed to be a member of that pool). `natstat_id` is null when
/// no player is eligible for the requested pool.
async fn daily_puzzle(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DailyParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mode_str = params.mode.as_deref().unwrap_or("p5");
    let mode = PortleMode::parse(mode_str).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown mode: {mode_str}") })),
        )
    })?;
    let season = params.season.unwrap_or_else(crate::default_season);

    // The daily flips at the player's LOCAL midnight (Wordle convention), so the
    // client passes its own calendar date. Two clients on the same local date get
    // the same pin regardless of timezone. Default to the server UTC date only if
    // the client omits it.
    let date = match params.date.as_deref() {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid date (want YYYY-MM-DD): {s}") })),
            )
        })?,
        None => Utc::now().date_naive(),
    };

    let natstat_id = queries::pick_or_pin_daily_puzzle(&state.db.pool, mode, season, date)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("daily puzzle failed: {e}") })),
            )
        })?;

    Ok(Json(json!({
        "mode": mode.as_str(),
        "season": season,
        "date": date.format("%Y-%m-%d").to_string(),
        "natstat_id": natstat_id,
    })))
}
