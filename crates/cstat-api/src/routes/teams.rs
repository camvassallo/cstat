use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries::{self, SortOrder, TeamSortField};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::AppState;
use crate::routes::predict::{ProjectionSummary, predict_projection};

/// Max schedule-game projections in flight at once inside `team_detail`.
///
/// Each completed game's projection runs a full-season `compute_pit_campom`
/// aggregate (its dominant cost), so overlapping them is the win; the bound
/// keeps a single page load from monopolizing the shared connection pool
/// (each projection peaks at ~6 connections during its inner `try_join`).
/// 6 concurrent games overlaps the heavy scans while leaving pool headroom
/// for other endpoints; sqlx queues any acquire overflow, so this can't
/// deadlock even if every game peaks together.
const SCHEDULE_PROJECTION_CONCURRENCY: usize = 6;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/teams/rankings", get(rankings))
        .route("/api/teams/{id}", get(team_detail))
        .route("/api/teams/{id}/coach", get(team_coach))
        .route("/api/teams/{id}/lineups", get(team_lineups))
}

/// `GET /api/teams/{id}/lineups` — the team's top 5-man on-floor lineups
/// (PBP-derived, from `lineup_aggregates`). A dedicated route — like `coach`,
/// it must not wait on `team_detail`'s per-game projection loop, and it's a
/// supplementary panel the frontend fetches in parallel. Returns an empty list
/// (200) for a team-season with no PBP-derived lineups rather than a 404.
async fn team_lineups(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<TeamDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let resolved_id = match queries::resolve_team_id_for_season(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })? {
        Some(rid) => rid,
        None => return Ok(Json(json!({ "season": season, "lineups": [] }))),
    };

    let lineups = queries::get_team_lineups(pool, resolved_id, season, 15)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({ "season": season, "lineups": lineups })))
}

/// `GET /api/teams/{id}/coach` — the coach card for a team-detail page.
///
/// Deliberately a DEDICATED route, NOT folded into `team_detail`: that handler
/// still carries a ~30-game `predict_projection` fan-out (point-in-time CamPom
/// rebuild per completed game) — now bounded-concurrent rather than serial, but
/// still the page's heaviest step. This query is two indexed lookups
/// (`coach_seasons` → `coach_ratings`) and must not wait on that work, so the
/// frontend fetches it in parallel and the card paints immediately. See
/// ROADMAP "API latency — team-detail schedule projections".
///
/// Returns `{ "coach": null }` (200) when coachdict has no entry for the
/// (team, season) — an unmatched team-season is an expected empty state, not an
/// error.
async fn team_coach(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let coach = queries::get_team_coach(&state.db.pool, id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({ "coach": coach })))
}

#[derive(Deserialize)]
struct RankingsParams {
    season: Option<i32>,
    sort: Option<TeamSortField>,
    order: Option<SortOrder>,
}

async fn rankings(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RankingsParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let sort = params.sort.unwrap_or_default();

    let teams = queries::get_team_rankings(&state.db.pool, season, sort, params.order)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({
        "season": season,
        "teams": teams,
    })))
}

#[derive(Deserialize)]
struct TeamDetailParams {
    season: Option<i32>,
}

async fn team_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<TeamDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    // Team UUIDs are season-scoped, so an `id` from season X plus `?season=Y`
    // initially looks like a 404. Before giving up, resolve the team's
    // `natstat_id` (cross-season identifier) and retry for the requested
    // season. The frontend uses the returned `team.id` to redirect to the
    // canonical URL when this fallback fires.
    let resolved_id = match queries::resolve_team_id_for_season(pool, id, season)
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
                Json(json!({ "error": "team not found for this season" })),
            ));
        }
    };

    let team = queries::get_team_by_id(pool, resolved_id, season)
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
                Json(json!({ "error": "team not found" })),
            )
        })?;

    let (mut schedule, roster, archetype_distribution, available_seasons, total_teams) =
        tokio::try_join!(
            queries::get_team_schedule(pool, resolved_id, season),
            queries::get_team_roster(pool, resolved_id, season),
            queries::get_team_archetype_index(pool, resolved_id, season),
            queries::get_team_available_seasons(pool, resolved_id),
            queries::get_season_team_count(pool, season),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    // Project every game using the existing predictor. Each *completed*
    // game rebuilds CamPom v3 from pre-game state via the pit bundle, and
    // that path runs a full-season `compute_pit_campom` aggregate — so a
    // serial loop over a ~30-game schedule was the team-detail page's
    // dominant latency. We fan the per-game projections out concurrently
    // instead, bounded by a semaphore so a single page load can't drain the
    // shared connection pool out from under other endpoints. Results are
    // written back by row index, so schedule order is preserved.
    // Per-game failures are silently dropped — the row still renders, just
    // without a projection.
    //
    // Sign convention: `projected_margin` is from the *requested team's*
    // perspective (positive = requested team favored), regardless of host.
    //
    // Honest projections for completed games: when both teams' scores are
    // populated, we treat the row as historical and pass `as_of_date =
    // game_date - 1 day` to the predictor so it rebuilds CamPom v3 from
    // pre-game state. Upcoming games pass `None` ("today" is the only honest
    // cutoff for an unplayed game). This closes the audit's R3 surface: the
    // column is no longer a leaky "we'd predict X today" on rows where we
    // already know the outcome.
    let sem = Arc::new(Semaphore::new(SCHEDULE_PROJECTION_CONCURRENCY));
    let mut tasks: JoinSet<(
        usize,
        bool,
        Option<chrono::NaiveDate>,
        Option<ProjectionSummary>,
    )> = JoinSet::new();
    for (idx, entry) in schedule.iter().enumerate() {
        let Some(opp_id) = entry.opponent_id else {
            continue;
        };
        let is_neutral = entry.is_neutral.unwrap_or(false);
        let is_conference = entry.is_conference.unwrap_or(false);
        // Sort home/away for the predictor's frame; the sign flip back to the
        // requested team's perspective happens when results land. Neutral
        // games predict symmetric to argument order, so the flip is purely
        // semantic there.
        let requested_is_home = entry.is_home.unwrap_or(false);
        let (host_id, visitor_id) = if requested_is_home {
            (resolved_id, opp_id)
        } else {
            (opp_id, resolved_id)
        };
        let is_played = entry.team_score.is_some() && entry.opponent_score.is_some();
        let as_of_date = if is_played {
            entry.game_date.pred_opt()
        } else {
            None
        };
        let state = Arc::clone(&state);
        let sem = Arc::clone(&sem);
        tasks.spawn(async move {
            // Permit is held for the projection's lifetime and released when
            // the task ends, capping concurrent pit aggregates against the
            // pool. `acquire_owned` only errors if the semaphore is closed,
            // which never happens here.
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let proj = predict_projection(
                &state,
                host_id,
                visitor_id,
                season,
                is_neutral,
                is_conference,
                as_of_date,
            )
            .await
            .ok();
            (idx, requested_is_home, as_of_date, proj)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        // A task panic (JoinError) drops just that row's projection.
        let Ok((idx, requested_is_home, as_of_date, Some(proj))) = joined else {
            continue;
        };
        let entry = &mut schedule[idx];
        let (margin_team, p_team, score_team, score_opp) = if requested_is_home {
            (
                proj.margin as f64,
                proj.home_win_prob,
                proj.home_score,
                proj.away_score,
            )
        } else {
            (
                -proj.margin as f64,
                1.0 - proj.home_win_prob,
                proj.away_score,
                proj.home_score,
            )
        };
        // Round to 1 decimal / 3 decimals to match the rest of the API.
        entry.projected_margin = Some((margin_team * 10.0).round() / 10.0);
        entry.projected_win_prob = Some((p_team * 1000.0).round() / 1000.0);
        entry.projected_score_team = Some(score_team);
        entry.projected_score_opp = Some(score_opp);
        // Honesty label travels with the projection — set ONLY when the
        // predictor succeeds, so a failed prediction can't leave the row
        // labelled "pre-game projection" with a null margin. A completed
        // game whose date can't be decremented (NaiveDate::MIN sentinel)
        // has as_of_date = None and is correctly NOT labelled pre-game.
        entry.is_pre_game_projection = as_of_date.is_some();
    }

    Ok(Json(json!({
        "team": team,
        "schedule": schedule,
        "roster": roster,
        "archetype_distribution": archetype_distribution,
        "available_seasons": available_seasons,
        "total_teams": total_teams,
    })))
}
