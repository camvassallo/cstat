use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use chrono::NaiveDate;
use cstat_core::queries;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/games", get(game_list))
}

#[derive(Deserialize)]
struct GameListParams {
    date: Option<NaiveDate>,
    team: Option<Uuid>,
    season: Option<i32>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn game_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GameListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.limit.unwrap_or(50).min(200);
    let offset = params.offset.unwrap_or(0);

    let games = queries::get_games(
        &state.db.pool,
        params.date,
        params.team,
        season,
        limit,
        offset,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    Ok(Json(json!({
        "season": season,
        "games": games,
        "limit": limit,
        "offset": offset,
    })))
}
