//! Team strength as of a cutoff date.
//!
//! The point-in-time feature path reads team stats from `team_season_stats`,
//! which hold how the season ended. For a past game that leaks: end-of-season
//! Elo and adjusted efficiency alone took a walk-forward model from about 54%
//! to 57% against the spread (#274). Record, four factors, roster and form
//! showed nothing measurable on the margin, so those stay season aggregates.
//!
//! Tempo is the exception on the totals side. `sum_adj_tempo` is trained on
//! each team's pre-game possessions per game, and November is played faster
//! than March: scoring a past game with the season's final tempo read the two
//! teams about 1.5 possessions slow together and pulled stored projected totals
//! 2.5 points low (2025, walk-forward: -2.5 with the final tempo, +0.3 with
//! this).
//!
//! Mirrors the pre-game values in `training/features.py`: Elo from NatStat's
//! per-game log, adjusted efficiency from a solve over the games played so far,
//! tempo as `cum_tempo` (the team's own possessions per game, floored at 1).

use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::compute::{RatingAsOf, adjusted_ratings_as_of};

/// Every team's strength through one cutoff.
#[derive(Debug, Clone, Default)]
pub struct PitTeams {
    pub ratings: HashMap<Uuid, RatingAsOf>,
    pub league_avg: f64,
    pub elo: HashMap<Uuid, f64>,
    /// Possessions per game through the cutoff, from the team's own box rows.
    pub tempo: HashMap<Uuid, f64>,
}

/// Build the cutoff's state. Elo is each team's latest rating on or before
/// `as_of`, which carries in from an earlier season before its first game. The
/// season bound matters: the feed has mis-dated rows, and one from a later
/// season must not pass for history.
pub async fn compute_pit_teams(
    pool: &PgPool,
    season: i32,
    as_of: NaiveDate,
) -> Result<PitTeams, sqlx::Error> {
    let (ratings, league_avg) = adjusted_ratings_as_of(pool, season, as_of).await?;
    let elo: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT t.id, latest.elo_after
           FROM teams t
           CROSS JOIN LATERAL (
               SELECT e.elo_after
                 FROM team_elo_games e
                WHERE e.team_code = t.natstat_id AND e.season <= $1 AND e.game_date <= $2
                ORDER BY e.game_date DESC, e.season DESC, e.season_games DESC NULLS LAST
                LIMIT 1
           ) latest
          WHERE t.season = $1",
    )
    .bind(season)
    .bind(as_of)
    .fetch_all(pool)
    .await?;
    // Every box row counts, a non-D1 opponent included, exactly as
    // `compute_cumulative_team_stats` averages them. A row missing a column
    // is skipped there (NaN) and here (NULL).
    let tempo: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT team_id,
                AVG(GREATEST(fga - off_rebounds + turnovers + 0.44 * fta, 1))::float8
           FROM team_game_stats
          WHERE season = $1 AND game_date <= $2
            AND fga IS NOT NULL AND off_rebounds IS NOT NULL
            AND turnovers IS NOT NULL AND fta IS NOT NULL
          GROUP BY team_id",
    )
    .bind(season)
    .bind(as_of)
    .fetch_all(pool)
    .await?;
    Ok(PitTeams {
        ratings,
        league_avg,
        elo: elo.into_iter().collect(),
        tempo: tempo.into_iter().collect(),
    })
}
