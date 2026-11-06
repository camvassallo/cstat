use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::compute::FULL_SEASON_GAMES;
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
    /// blowout rates). Defaults per size and to how much of the season has been
    /// played, see [`default_min_minutes`].
    min_minutes: Option<f64>,
    limit: Option<i64>,
    /// Optional: restrict to combos containing this player (season-scoped UUID,
    /// re-resolved to the requested season so a stale cross-season id still
    /// matches).
    player: Option<Uuid>,
    /// Optional: restrict to a single team.
    team: Option<Uuid>,
    /// Ordering: `"minutes"` (most-used) or the default `"adj_net"` (best,
    /// opponent-adjusted). The team-page duo/trio panels pass `minutes`.
    order: Option<String>,
}

/// The smallest share of the full floor a season can drop to, so opening week
/// still filters out units that shared a few possessions.
const EARLY_SEASON_FLOOR_SHARE: f64 = 0.2;

/// Minutes a combo needs to qualify when `games` is what a team has played so
/// far. The floor grows in step with the season, because a fixed one left every
/// tab empty for its first weeks: after two games the most-used unit in the
/// country had 32 minutes against a floor of 50.
///
/// The full floor scales with combo size: a duo/trio pools shared minutes across
/// every 5-man unit it appears in, so it accrues minutes far faster than an exact
/// 5-man and needs a higher bar to filter thin-sample outliers.
///
/// Full floors lowered (was 300/200/100) for the heavily-substituted modern game:
/// at the old 100-min 5-man bar only ~23% of 2026 teams had a single qualifying
/// unit (vs ~70% in 2023-24), so most teams — including Duke, whose top unit
/// lands at 99.1 min — vanished from the ranking entirely. 50 restores ~68% team
/// coverage, comparable to what 2023-24 saw at 100.
fn default_min_minutes(size: i32, games: f64) -> f64 {
    let full = match size {
        2 => 150.0,
        3 => 100.0,
        _ => 50.0,
    };
    let share = (games / FULL_SEASON_GAMES).clamp(EARLY_SEASON_FLOOR_SHARE, 1.0);
    (full * share).round()
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
    let min_minutes = match params.min_minutes {
        Some(floor) => floor,
        None => default_min_minutes(
            size,
            queries::typical_team_games(pool, season)
                .await
                .map_err(internal)?,
        ),
    };
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let order_by_minutes = params.order.as_deref() == Some("minutes");

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

    let lineups = queries::get_lineup_rankings(
        pool,
        season,
        size,
        min_minutes,
        limit,
        player,
        team,
        order_by_minutes,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_week_stops_at_the_early_season_floor() {
        assert_eq!(default_min_minutes(5, 0.0), 10.0);
        assert_eq!(default_min_minutes(5, 2.0), 10.0);
        assert_eq!(default_min_minutes(3, 2.0), 20.0);
        assert_eq!(default_min_minutes(2, 2.0), 30.0);
    }

    #[test]
    fn the_floor_grows_with_games_played() {
        assert_eq!(default_min_minutes(5, 16.0), 25.0);
        assert_eq!(default_min_minutes(3, 16.0), 50.0);
        assert_eq!(default_min_minutes(2, 16.0), 75.0);
    }

    #[test]
    fn a_full_season_keeps_the_full_floor() {
        for games in [FULL_SEASON_GAMES, 36.0] {
            assert_eq!(default_min_minutes(5, games), 50.0);
            assert_eq!(default_min_minutes(3, games), 100.0);
            assert_eq!(default_min_minutes(2, games), 150.0);
        }
    }
}
