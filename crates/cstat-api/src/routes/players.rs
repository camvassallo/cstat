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
        .route("/api/players/{id}/progression", get(player_progression))
}

#[derive(Deserialize)]
struct PlayerListParams {
    search: Option<String>,
    team: Option<Uuid>,
    season: Option<i32>,
    sort: Option<PlayerSortField>,
    order: Option<SortOrder>,
    /// Filter to a single archetype class (e.g. "Wizard").
    archetype: Option<String>,
    /// When true and `archetype` is set, also match players whose
    /// `secondary_class` equals the filter — used by the drill-down's
    /// "primary or secondary" toggle.
    include_secondary_archetype: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

// Page size cap. The Players tab loads the entire qualified pool in one
// request (~2-3k players for a typical season) and paginates client-side via
// AG Grid; 5000 leaves headroom for that plus scripted callers without
// letting a single request scrape an entire database scan.
const PLAYER_LIST_MAX_LIMIT: i64 = 5000;

async fn player_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlayerListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.limit.unwrap_or(50).clamp(1, PLAYER_LIST_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0).max(0);
    let sort = params.sort.unwrap_or_default();
    let include_secondary = params.include_secondary_archetype.unwrap_or(false);

    let (players, total) = queries::search_players(
        &state.db.pool,
        params.search.as_deref(),
        params.team,
        season,
        sort,
        params.order,
        params.archetype.as_deref(),
        include_secondary,
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

    // Player UUIDs are season-scoped (natstat_id is the cross-season key), so
    // a URL with last season's UUID + `?season=` switching to this season
    // initially 404s. Resolve via natstat_id and retry. Returned `player.id`
    // tells the frontend to redirect to the canonical URL.
    let resolved_id = match queries::resolve_player_id_for_season(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })? {
        Some(rid) => rid,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "player not found for this season" })),
            ));
        }
    };

    let player = queries::get_player_by_id(pool, resolved_id, season)
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

    let (
        season_stats,
        percentiles,
        game_log,
        league_averages,
        torvik_stats,
        archetype,
        available_seasons,
        trajectory_row,
    ) = tokio::try_join!(
        queries::get_player_season_stats(pool, resolved_id, season),
        queries::get_player_percentiles(pool, resolved_id, season),
        queries::get_player_game_log(pool, resolved_id, season),
        queries::get_league_averages(pool, season),
        queries::get_torvik_stats(pool, resolved_id, season),
        queries::get_player_archetype(pool, resolved_id, season),
        queries::get_player_available_seasons(pool, resolved_id),
        cstat_core::trajectory::fetch_player_trajectory_row(pool, resolved_id, season),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    // Phase 5c trajectory: project next-season CamPom from the qualified
    // prior-season row. Gated on the player passing the QUAL filter AND
    // having a non-null CamPom (the model's most-load-bearing feature) —
    // if either is missing the badge stays null and the frontend hides the
    // section. ONNX inference is ~3ms total (3 models); we run it inline
    // on the request rather than precomputing because the player-detail
    // route is per-player traffic, not batch.
    let trajectory = trajectory_row.and_then(|row| {
        row.campom?;
        let features = cstat_core::trajectory::build_trajectory_features(&row);
        match state.predictor.predict_trajectory(&features) {
            Ok(pred) => Some(json!({
                "base_season": season,
                "target_season": season + 1,
                "projected_mean": pred.mean,
                "projected_lower": pred.lower,
                "projected_upper": pred.upper,
                "prior_campom": row.campom,
            })),
            Err(e) => {
                tracing::warn!(error = ?e, player_id = %resolved_id, "trajectory predict failed");
                None
            }
        }
    });

    Ok(Json(json!({
        "player": player,
        "season_stats": season_stats,
        "percentiles": percentiles,
        "game_log": game_log,
        "league_averages": league_averages,
        "torvik_stats": torvik_stats,
        "archetype": archetype,
        "available_seasons": available_seasons,
        "trajectory": trajectory,
    })))
}

/// Cross-season "career progression" view. Returns one entry per
/// (player_id, season) the human appears in — joined across transfers
/// via `torvik_pid` the same way `get_player_available_seasons` does.
/// Each entry carries the player profile (team name changes year over
/// year for transfers), season_stats, percentiles, torvik_stats, and
/// archetype, so the frontend can render time-series + per-season
/// radars and shot-diet cards without N round-trips. The most-recent
/// season's trajectory projection is included for the header chip.
async fn player_progression(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let pool = &state.db.pool;

    let seasons = queries::get_player_available_seasons(pool, id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("seasons query failed: {e}") })),
            )
        })?;
    if seasons.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "player not found" })),
        ));
    }

    // Fetch each season's payload. Seasons go in order from
    // `get_player_available_seasons` (newest-first); we resolve the
    // season-scoped player_id for each, then fan out the per-season
    // queries via tokio::try_join. Sequential outer loop with parallel
    // inner is a good fit for ~3-6 seasons: ~50ms per season, never
    // hot-pathed enough to need futures::join_all.
    let mut entries: Vec<Value> = Vec::with_capacity(seasons.len());
    for season in &seasons {
        let resolved = queries::resolve_player_id_for_season(pool, id, *season)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("resolve failed for season {season}: {e}") })),
                )
            })?;
        let Some(rid) = resolved else {
            continue;
        };
        let (profile, season_stats, percentiles, torvik_stats, archetype) = tokio::try_join!(
            queries::get_player_by_id(pool, rid, *season),
            queries::get_player_season_stats(pool, rid, *season),
            queries::get_player_percentiles(pool, rid, *season),
            queries::get_torvik_stats(pool, rid, *season),
            queries::get_player_archetype(pool, rid, *season),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({ "error": format!("per-season fetch failed for season {season}: {e}") }),
                ),
            )
        })?;
        let Some(profile) = profile else {
            continue;
        };
        entries.push(json!({
            "season": season,
            "player_id": rid,
            "name": profile.name,
            "team_id": profile.team_id,
            "team_name": profile.team_name,
            "position": profile.position,
            "class_year": profile.class_year,
            "jersey_number": profile.jersey_number,
            "height_inches": profile.height_inches,
            "weight_lbs": profile.weight_lbs,
            "season_stats": season_stats,
            "percentiles": percentiles,
            "torvik_stats": torvik_stats,
            "archetype": archetype,
        }));
    }

    // Trajectory projection — same logic as `player_detail`, anchored
    // to the most-recent season. Surfaces in the page header so users
    // see the projected next-season CamPom alongside their actual
    // career arc on this page (and the link back from the chip on the
    // single-season page).
    let latest_season = seasons[0];
    let latest_resolved = queries::resolve_player_id_for_season(pool, id, latest_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("trajectory resolve failed: {e}") })),
            )
        })?;
    let trajectory = if let Some(rid) = latest_resolved {
        let row = cstat_core::trajectory::fetch_player_trajectory_row(pool, rid, latest_season)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("trajectory query failed: {e}") })),
                )
            })?;
        row.and_then(|row| {
            row.campom?;
            let features = cstat_core::trajectory::build_trajectory_features(&row);
            match state.predictor.predict_trajectory(&features) {
                Ok(pred) => Some(json!({
                    "base_season": latest_season,
                    "target_season": latest_season + 1,
                    "projected_mean": pred.mean,
                    "projected_lower": pred.lower,
                    "projected_upper": pred.upper,
                    "prior_campom": row.campom,
                })),
                Err(e) => {
                    tracing::warn!(error = ?e, player_id = %rid, "trajectory predict failed");
                    None
                }
            }
        })
    } else {
        None
    };

    Ok(Json(json!({
        "available_seasons": seasons,
        "seasons": entries,
        "trajectory": trajectory,
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
        let (player, season_stats, percentiles, game_log, torvik_stats, archetype) =
            tokio::try_join!(
                queries::get_player_by_id(pool, *id, season),
                queries::get_player_season_stats(pool, *id, season),
                queries::get_player_percentiles(pool, *id, season),
                queries::get_player_game_log(pool, *id, season),
                queries::get_torvik_stats(pool, *id, season),
                queries::get_player_archetype(pool, *id, season),
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
            "archetype": archetype,
        }));
    }

    Ok(Json(json!({
        "season": season,
        "league_averages": league_averages,
        "players": players_data,
    })))
}
