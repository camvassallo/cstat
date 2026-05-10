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
use uuid::Uuid;

use crate::AppState;
use crate::routes::predict::predict_projection;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/teams/rankings", get(rankings))
        .route("/api/teams/{id}", get(team_detail))
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

    let (mut schedule, roster, archetype_distribution, available_seasons) = tokio::try_join!(
        queries::get_team_schedule(pool, resolved_id, season),
        queries::get_team_roster(pool, resolved_id, season),
        queries::get_team_archetype_index(pool, resolved_id, season),
        queries::get_team_available_seasons(pool, resolved_id),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    // Project every game using the existing predictor. Inference is fast
    // (sub-ms per call) so doing it inline keeps the team-detail endpoint a
    // single round-trip surface. Failures per-game are silently dropped —
    // the schedule still renders, just without a projection on that row.
    // Sign convention: `projected_margin` is from the *requested team's*
    // perspective (positive = requested team favored), regardless of host.
    //
    // We project completed games too (not just upcoming) so the column is
    // useful in the offseason and on historical browsing. The caveat is
    // that for completed games the projection uses *current* team state,
    // not pre-game state — true pre-game predictions are tracked as a
    // future roadmap item (see "point-in-time historical predictions").
    for entry in schedule.iter_mut() {
        let opp_id = match entry.opponent_id {
            Some(id) => id,
            None => continue,
        };
        let is_neutral = entry.is_neutral.unwrap_or(false);
        let is_conference = entry.is_conference.unwrap_or(false);
        // Sort home/away for the predictor's frame, then flip the sign back
        // to the requested team's perspective if the requested team is
        // visiting. Neutral games predict symmetric to argument order so the
        // sign flip is purely semantic.
        let requested_is_home = entry.is_home.unwrap_or(false);
        let (host_id, visitor_id) = if requested_is_home {
            (resolved_id, opp_id)
        } else {
            (opp_id, resolved_id)
        };
        if let Ok(proj) = predict_projection(
            &state,
            host_id,
            visitor_id,
            season,
            is_neutral,
            is_conference,
        )
        .await
        {
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
        }
    }

    Ok(Json(json!({
        "team": team,
        "schedule": schedule,
        "roster": roster,
        "archetype_distribution": archetype_distribution,
        "available_seasons": available_seasons,
    })))
}
