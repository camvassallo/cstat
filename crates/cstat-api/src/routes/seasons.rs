use axum::{Router, extract::State, http::StatusCode, response::Json, routing::get};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/seasons", get(list_seasons))
}

/// Seasons present in the DB (newest first), so the frontend doesn't have to
/// hardcode them. Driven off `games` because that's the canonical "season
/// is real" signal — adding a season without games (teams-only) wouldn't
/// show anything useful anyway. Returns `{ seasons: [int], default: int }`
/// where `default` is the newest available season.
async fn list_seasons(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows: Vec<(i32,)> = sqlx::query_as(
        "SELECT DISTINCT season FROM games WHERE season IS NOT NULL ORDER BY season DESC",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("seasons query failed: {e}") })),
        )
    })?;

    let seasons: Vec<i32> = rows.into_iter().map(|(s,)| s).collect();
    let default = seasons.first().copied();

    Ok(Json(json!({
        "seasons": seasons,
        "default": default,
    })))
}
