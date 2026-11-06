//! Team strength as of a cutoff date.
//!
//! The point-in-time feature path reads team stats from `team_season_stats`,
//! which hold how the season ended. For a past game that leaks: end-of-season
//! Elo and adjusted efficiency alone took a walk-forward model from about 54%
//! to 57% against the spread (#274). Record, four factors, roster and form
//! showed nothing measurable, so those stay season aggregates.
//!
//! Mirrors the pre-game values in `training/features.py`: Elo from NatStat's
//! per-game log, adjusted efficiency from a solve over the games played so far.

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
    Ok(PitTeams {
        ratings,
        league_avg,
        elo: elo.into_iter().collect(),
    })
}
