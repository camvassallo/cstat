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
        .route("/api/players/compare", get(player_compare))
        .route("/api/players/{id}", get(player_detail))
        .route("/api/players/{id}/archetype", get(player_archetype))
        .route("/api/players/{id}/similar", get(player_similar))
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

    let (season_stats, percentiles, game_log, league_averages, torvik_stats, archetype) =
        tokio::try_join!(
            queries::get_player_season_stats(pool, id, season),
            queries::get_player_percentiles(pool, id, season),
            queries::get_player_game_log(pool, id, season),
            queries::get_league_averages(pool, season),
            queries::get_torvik_stats(pool, id, season),
            queries::get_player_archetype(pool, id, season),
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
        "archetype": archetype,
    })))
}

#[derive(Deserialize)]
struct PlayerArchetypeParams {
    season: Option<i32>,
}

async fn player_archetype(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerArchetypeParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let archetype = queries::get_player_archetype(&state.db.pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    match archetype {
        Some(a) => Ok(Json(json!({
            "season": season,
            "archetype": a,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no archetype assigned for this player/season" })),
        )),
    }
}

#[derive(Deserialize)]
struct PlayerSimilarParams {
    season: Option<i32>,
    k: Option<i64>,
}

async fn player_similar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerSimilarParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.k.unwrap_or(10).clamp(1, 50);
    let players = queries::get_similar_players(&state.db.pool, id, season, limit)
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
    })))
}

#[derive(Deserialize)]
struct PlayerCompareParams {
    ids: String,
    season: Option<i32>,
}

const MAX_COMPARE_PLAYERS: usize = 4;

async fn player_compare(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlayerCompareParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let ids: Vec<Uuid> = params
        .ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Uuid::parse_str)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid uuid in ids: {e}") })),
            )
        })?;

    if ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ids query param is required" })),
        ));
    }
    if ids.len() > MAX_COMPARE_PLAYERS {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("max {MAX_COMPARE_PLAYERS} players per compare request"),
            })),
        ));
    }

    let league_averages = queries::get_league_averages(pool, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    let mut players_data = Vec::with_capacity(ids.len());
    for id in &ids {
        let (player, season_stats, percentiles, game_log, torvik_stats) = tokio::try_join!(
            queries::get_player_by_id(pool, *id, season),
            queries::get_player_season_stats(pool, *id, season),
            queries::get_player_percentiles(pool, *id, season),
            queries::get_player_game_log(pool, *id, season),
            queries::get_torvik_stats(pool, *id, season),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

        let Some(player) = player else { continue };

        players_data.push(json!({
            "player": player,
            "season_stats": season_stats,
            "percentiles": percentiles,
            "game_log": game_log,
            "torvik_stats": torvik_stats,
        }));
    }

    Ok(Json(json!({
        "season": season,
        "league_averages": league_averages,
        "players": players_data,
    })))
}
