use axum::{
    Router,
    extract::{Query, State},
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
    Router::new().route("/api/lineups", get(lineup_rankings))
}

#[derive(Deserialize)]
struct LineupRankingParams {
    season: Option<i32>,
    /// Combination size: 2 (duos), 3 (trios), or 5 (full lineups). Defaults to
    /// 5; any other value is clamped to 5.
    size: Option<i32>,
    /// Minimum shared minutes for a combo to qualify (floors out small-sample
    /// blowout rates). Defaults per size — duos/trios need a higher floor than
    /// 5-man units to be meaningful.
    min_minutes: Option<f64>,
    limit: Option<i64>,
    /// Optional: restrict to combos containing this player (season-scoped UUID,
    /// re-resolved to the requested season so a stale cross-season id still
    /// matches).
    player: Option<Uuid>,
    /// Optional: restrict to a single team.
    team: Option<Uuid>,
}

/// `GET /api/lineups` — cross-team ranking of lineup combinations (duos / trios
/// / 5-man), best opponent-adjusted net (AdjEM) first among combos clearing the
/// minutes floor. Exploded at query time from the prod-resident
/// `lineup_aggregates`. Optional `player` / `team` filters drive the per-player
/// drill-down.
async fn lineup_rankings(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LineupRankingParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    // Only 2 / 3 / 5 are served; anything else is treated as 5-man.
    let size = match params.size {
        Some(2) => 2,
        Some(3) => 3,
        _ => 5,
    };
    // Floor scales with combo size — a duo/trio pools shared minutes across
    // every 5-man unit it appears in, so it accrues minutes far faster than an
    // exact 5-man and needs a higher bar to filter thin-sample outliers.
    let min_minutes = params.min_minutes.unwrap_or(match size {
        2 => 300.0,
        3 => 200.0,
        _ => 100.0,
    });
    let limit = params.limit.unwrap_or(100).clamp(1, 500);

    // Re-resolve an optional player filter to the requested season so a
    // cross-season UUID (e.g. arriving from a player page on a different year)
    // still matches the season's lineups.
    let player = match params.player {
        Some(pid) => queries::resolve_player_id_for_season(pool, pid, season)
            .await
            .map_err(internal)?,
        None => None,
    };
    let team = match params.team {
        Some(tid) => queries::resolve_team_id_for_season(pool, tid, season)
            .await
            .map_err(internal)?,
        None => None,
    };

    let lineups =
        queries::get_lineup_rankings(pool, season, size, min_minutes, limit, player, team)
            .await
            .map_err(internal)?;

    Ok(Json(json!({
        "season": season,
        "size": size,
        "min_minutes": min_minutes,
        "lineups": lineups,
    })))
}

fn internal(e: sqlx::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": format!("query failed: {e}") })),
    )
}
