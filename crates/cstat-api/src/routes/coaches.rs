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
    /// Minimum scored seasons to qualify. Default 3 — thin tenures shrink
    /// toward 0 and would otherwise top the board on noise. Clamped to ≥1.
    min_seasons: Option<i32>,
    /// Page size. Default 200, clamped to [1, 500].
    limit: Option<i64>,
}

/// `GET /api/coaches` — Coach-Above-Expectation leaderboard, sorted by the
/// headline `cae_shrunk` (raw EB-shrunk) descending. Reads only `coach_ratings`
/// + a latest-team lookup; no team-detail / projection work.
async fn coach_leaderboard(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LeaderboardParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let min_seasons = params.min_seasons.unwrap_or(3).max(1);
    let limit = params.limit.unwrap_or(200).clamp(1, 500);

    let coaches = queries::get_coach_leaderboard(&state.db.pool, min_seasons, limit)
        .await
        .map_err(internal_error)?;

    Ok(Json(json!({
        "min_seasons": min_seasons,
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
    let (rating, seasons) = tokio::try_join!(
        queries::get_coach_rating(pool, id),
        queries::get_coach_seasons(pool, id),
    )
    .map_err(internal_error)?;

    // A coach with no rating AND no scored seasons is effectively unknown.
    if rating.is_none() && seasons.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "coach not found" })),
        ));
    }

    Ok(Json(json!({
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
