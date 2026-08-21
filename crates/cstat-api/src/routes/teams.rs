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
use cstat_core::projection::ProjectionSummary;

use crate::routes::predict::predict_projection;

/// Max LIVE schedule-game projections in flight at once inside `team_detail`.
///
/// Since #266 this bounds only the games `game_projections` doesn't cover —
/// upcoming games, and any completed game the nightly sweep hasn't reached —
/// so on a materialized in-season page it governs a handful of rows rather
/// than the whole schedule. It still matters: each live projection peaks at
/// ~6 connections during its inner `try_join`, so an un-materialized page
/// (a season the sweep has never run for) would otherwise demand ~6× that
/// against a 25-connection pool on its own. sqlx queues any acquire overflow,
/// so this can't deadlock even if every game peaks together.
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

/// Re-frame a stored `game_projections` row from the home team's perspective
/// into the requested team's.
///
/// The table stores one row per game in the home frame; a schedule renders the
/// same game from whichever side the viewer is looking at. Margin negates, the
/// win probability mirrors around 0.5, and the two scores swap — the same
/// transform the live fan-out applies to a `ProjectionSummary`.
fn stored_in_team_frame(
    p: &queries::StoredGameProjection,
    requested_is_home: bool,
) -> (f64, f64, i32, i32) {
    if requested_is_home {
        (
            p.projected_margin,
            p.home_win_prob,
            p.projected_home_score,
            p.projected_away_score,
        )
    } else {
        (
            -p.projected_margin,
            1.0 - p.home_win_prob,
            p.projected_away_score,
            p.projected_home_score,
        )
    }
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

    // Completed games are served from `game_projections`, the table the
    // nightly materializes (#266). Projecting them live is what made this the
    // slowest route on the site: each one routes through the point-in-time
    // feature path, whose first step is a full-season aggregate over
    // `torvik_player_game_stats`, and neutral-site games run it twice for
    // order-invariance — 846 database round-trips for a 40-game schedule,
    // ~3 requests/second, and pool starvation for every other endpoint while
    // it ran. One indexed read replaces the bulk of that.
    //
    // A completed game that ISN'T in the table (played since the last sweep,
    // or skipped because a team had no stats row) falls through to the live
    // path below, so the column never silently empties.
    let stored = queries::get_team_game_projections(pool, resolved_id, season)
        .await
        .unwrap_or_else(|e| {
            // Non-fatal: a failed read costs latency, not correctness — every
            // game falls back to the live projection it used to get.
            tracing::warn!(
                team_id = %resolved_id, season, error = %e,
                "precomputed game projections unavailable; projecting the schedule live"
            );
            Default::default()
        });
    for entry in schedule.iter_mut() {
        let Some(p) = stored.get(&entry.game_id) else {
            continue;
        };
        // Re-check the played predicate against THIS response's schedule rows
        // rather than trusting the row's existence. A game can be completed
        // when the sweep runs and be corrected to postponed before the next
        // one — scores nulled — and the stored row survives until that sweep
        // prunes it. Serving it would put a retroactive "we predicted X"
        // projection, labelled pre-game, on a game that has not been played.
        // Falling through instead hands the row to the live path, which is
        // what it got before this table existed.
        if entry.team_score.is_none() || entry.opponent_score.is_none() {
            continue;
        }
        // Stored rows are in the home team's frame; flip when the requested
        // team was the visitor. Keyed off the stored `home_team_id` rather
        // than the schedule row's `is_home`, so a disagreement between the two
        // can't silently invert a margin.
        let (margin_team, p_team, score_team, score_opp) =
            stored_in_team_frame(p, p.home_team_id == resolved_id);
        entry.projected_margin = Some((margin_team * 10.0).round() / 10.0);
        entry.projected_win_prob = Some((p_team * 1000.0).round() / 1000.0);
        entry.projected_score_team = Some(score_team);
        entry.projected_score_opp = Some(score_opp);
        // Every stored row carries a non-null `as_of_date` by construction —
        // the writer refuses to persist a game it can't date — so a stored
        // projection is always the pre-game one.
        entry.is_pre_game_projection = true;
    }

    // Whatever is left — upcoming games, plus any completed game the sweep
    // hasn't covered — is projected live. The fan-out is bounded by a
    // semaphore so a single page load can't drain the shared connection pool
    // out from under other endpoints. Results are written back by row index,
    // so schedule order is preserved. Per-game failures are silently dropped
    // — the row still renders, just without a projection.
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
        // Already served from `game_projections`.
        if entry.projected_margin.is_some() {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(margin: f64, win: f64, home: i32, away: i32) -> queries::StoredGameProjection {
        queries::StoredGameProjection {
            game_id: Uuid::nil(),
            home_team_id: Uuid::nil(),
            projected_margin: margin,
            home_win_prob: win,
            projected_home_score: home,
            projected_away_score: away,
        }
    }

    #[test]
    fn stored_frame_is_identity_for_the_host() {
        let p = row(6.5, 0.72, 78, 71);
        assert_eq!(stored_in_team_frame(&p, true), (6.5, 0.72, 78, 71));
    }

    #[test]
    fn stored_frame_inverts_for_the_visitor() {
        let p = row(6.5, 0.72, 78, 71);
        let (m, w, team, opp) = stored_in_team_frame(&p, false);
        assert_eq!(m, -6.5);
        assert!((w - 0.28).abs() < 1e-12);
        // The visitor's own score comes first.
        assert_eq!((team, opp), (71, 78));
    }

    #[test]
    fn stored_frame_round_trips() {
        // Reading the same row from both sides must describe one game: the two
        // margins cancel, the two win probabilities sum to 1, and each side's
        // score pair is the other's reversed. A regression here would show up
        // as a team page claiming both teams were favored.
        let p = row(-3.25, 0.38, 64, 67);
        let (m_home, w_home, s_home, s_away) = stored_in_team_frame(&p, true);
        let (m_away, w_away, a_own, a_opp) = stored_in_team_frame(&p, false);
        assert!((m_home + m_away).abs() < 1e-12);
        assert!((w_home + w_away - 1.0).abs() < 1e-12);
        assert_eq!((s_home, s_away), (a_opp, a_own));
    }
}
