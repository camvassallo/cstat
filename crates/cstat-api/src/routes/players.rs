use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries::{self, PlayerSortField, SortOrder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/players", get(player_list))
        .route("/api/players/{id}", get(player_detail))
}

#[derive(Deserialize)]
struct PlayerListParams {
    search: Option<String>,
    team: Option<Uuid>,
    season: Option<i32>,
    sort: Option<PlayerSortField>,
    order: Option<SortOrder>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn player_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlayerListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.limit.unwrap_or(50).min(200);
    let offset = params.offset.unwrap_or(0);
    let sort = params.sort.unwrap_or_default();

    let (players, total) = queries::search_players(
        &state.db.pool,
        params.search.as_deref(),
        params.team,
        season,
        sort,
        params.order,
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
        "players": players,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

#[derive(Deserialize)]
struct PlayerDetailParams {
    season: Option<i32>,
}

async fn player_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let player = queries::get_player_by_id(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "player not found" })),
            )
        })?;

    let (season_stats, percentiles, game_log, league_averages, torvik_stats) = tokio::try_join!(
        queries::get_player_season_stats(pool, id, season),
        queries::get_player_percentiles(pool, id, season),
        queries::get_player_game_log(pool, id, season),
        queries::get_league_averages(pool, season),
        queries::get_torvik_stats(pool, id, season),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    Ok(Json(json!({
        "player": player,
        "season_stats": season_stats,
        "percentiles": percentiles,
        "game_log": game_log,
        "league_averages": league_averages,
        "torvik_stats": torvik_stats,
    })))
}
