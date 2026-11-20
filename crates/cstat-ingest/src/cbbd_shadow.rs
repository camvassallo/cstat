//! Shadow ingest of CollegeBasketballData into the `cbbd_*` tables (migration
//! 075), for `source-parity` to measure against the NatStat-fed tables. Nothing
//! the site serves reads what this writes (`docs/natstat_migration_plan.md`
//! §4.2).
//!
//! Dates are Eastern, like every other window in the nightly. CBBD's date
//! filters are UTC, so a window asks for one extra UTC day and keeps the games
//! whose Eastern tip date falls inside it: a 9 PM tip on Nov 18 is 02:00Z on
//! the 19th.
//!
//! `/plays/date` takes a UTC day, keyed on game start time, so a window of
//! Eastern dates reads UTC days `from..=to+1` and keeps the games already in
//! `cbbd_games`. Its `utcOffset` parameter is not used: with `-5` it returned a
//! six-hour slice (2026-11-20), not the Eastern day.
//!
//! Calls per window: one each for games, team box scores and player box
//! scores per 7 days, plus one `/plays/date` per UTC day. A two-day nightly
//! window is six calls.

use crate::cbbd::{CbbdClient, CbbdError, Game, PlayCoverage, PlayersGame, TeamGame};
use chrono::{Duration, NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashSet};
use tracing::{info, warn};

/// Days per range call. CBBD's cap per response is not documented; a week of
/// team rows is ~900, and [`RESULT_CAP_WARN`] flags a response large enough to
/// suspect truncation.
const CHUNK_DAYS: i64 = 7;

/// A range response this long may have been cut off at an undocumented cap.
const RESULT_CAP_WARN: usize = 3000;

/// Cache games and box scores for an hour: a re-run of the same window inside
/// that spends no quota, and the next night's run sees fresh data.
const CACHE_TTL_SECS: i64 = 3600;

#[derive(Debug, Default)]
pub struct ShadowReport {
    pub games: usize,
    pub finals: usize,
    pub team_rows: usize,
    pub player_rows: usize,
    pub play_summaries: usize,
    pub plays: usize,
    /// Days whose `/plays/date` call failed; the rest of the window still ran.
    pub play_days_failed: usize,
    pub calls_remaining: Option<i64>,
}

/// Ingest CBBD's games, box scores and play coverage for Eastern dates
/// `from..=to`. `plays` false skips the `/plays/date` calls (one per UTC day,
/// and the bulk of the bandwidth).
pub async fn run(
    pool: &PgPool,
    client: &CbbdClient,
    from: NaiveDate,
    to: NaiveDate,
    plays: bool,
) -> Result<ShadowReport, ShadowError> {
    let mut report = ShadowReport::default();
    let mut start = from;
    while start <= to {
        let end = (start + Duration::days(CHUNK_DAYS - 1)).min(to);
        ingest_chunk(pool, client, start, end, &mut report).await?;
        start = end + Duration::days(1);
    }
    if plays {
        // A game that tips exactly at midnight UTC (7 PM Eastern) comes back
        // whole in both days' responses, so games are counted once.
        let mut summarized: HashSet<i32> = HashSet::new();
        let mut day = from;
        while day <= to + Duration::days(1) {
            match ingest_plays(pool, client, day).await {
                Ok((games, n)) => {
                    summarized.extend(games);
                    report.plays += n;
                }
                // Out of quota: nothing later in the window can succeed.
                Err(ShadowError::Cbbd(e @ CbbdError::Refused(_))) => return Err(e.into()),
                Err(e) => {
                    warn!(%day, error = %e, "CBBD plays failed for a day; continuing");
                    report.play_days_failed += 1;
                }
            }
            day += Duration::days(1);
        }
        report.play_summaries = summarized.len();
    }
    report.calls_remaining = client.calls_remaining();
    info!(?report, "CBBD shadow ingest done");
    Ok(report)
}

#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error(transparent)]
    Cbbd(#[from] CbbdError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// The Eastern tip date of a UTC start time.
fn eastern_date(start: chrono::DateTime<Utc>) -> NaiveDate {
    crate::eastern_datetime(start).date()
}

/// A UTC date range that covers Eastern dates `from..=to`. It runs two days
/// past `to` because whether CBBD's `endDateRange` includes its own day is not
/// documented, and a late tip on `to` is already the next UTC day; the extra
/// games are filtered out by Eastern date.
fn utc_range(from: NaiveDate, to: NaiveDate) -> [(&'static str, String); 2] {
    [
        ("startDateRange", from.to_string()),
        ("endDateRange", (to + Duration::days(2)).to_string()),
    ]
}

fn warn_if_capped(path: &str, n: usize) {
    if n >= RESULT_CAP_WARN {
        warn!(
            path,
            rows = n,
            "CBBD response may be truncated; shorten CHUNK_DAYS"
        );
    }
}

async fn ingest_chunk(
    pool: &PgPool,
    client: &CbbdClient,
    from: NaiveDate,
    to: NaiveDate,
    report: &mut ShadowReport,
) -> Result<(), ShadowError> {
    let range = utc_range(from, to);
    let games: Vec<Game> = client.get("/games", &range, Some(CACHE_TTL_SECS)).await?;
    warn_if_capped("/games", games.len());
    // One row per key: a repeated row would fail the bulk upserts below on
    // their primary keys and lose the whole window.
    let mut seen_games = HashSet::new();
    let games: Vec<Game> = games
        .into_iter()
        .filter(|g| (from..=to).contains(&eastern_date(g.start_date)))
        .filter(|g| seen_games.insert(g.id))
        .collect();
    let kept: HashSet<i32> = games.iter().map(|g| g.id).collect();
    upsert_games(pool, &games).await?;
    report.games += games.len();
    report.finals += games
        .iter()
        .filter(|g| g.status.as_deref() == Some("final"))
        .count();
    if games.is_empty() {
        return Ok(());
    }

    let teams: Vec<TeamGame> = client
        .get("/games/teams", &range, Some(CACHE_TTL_SECS))
        .await?;
    warn_if_capped("/games/teams", teams.len());
    let mut seen_teams = HashSet::new();
    let teams: Vec<TeamGame> = teams
        .into_iter()
        .filter(|t| kept.contains(&t.game_id))
        .filter(|t| seen_teams.insert((t.game_id, t.team_id)))
        .collect();
    report.team_rows += replace_team_rows(pool, &teams).await?;

    let players: Vec<PlayersGame> = client
        .get("/games/players", &range, Some(CACHE_TTL_SECS))
        .await?;
    warn_if_capped("/games/players", players.len());
    let players: Vec<PlayersGame> = players
        .into_iter()
        .filter(|p| kept.contains(&p.game_id))
        .collect();
    report.player_rows += replace_player_rows(pool, &players).await?;
    Ok(())
}

async fn upsert_games(pool: &PgPool, games: &[Game]) -> Result<(), sqlx::Error> {
    if games.is_empty() {
        return Ok(());
    }
    let col = |f: fn(&Game) -> Option<i32>| games.iter().map(f).collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO cbbd_games (cbbd_game_id, espn_event_id, season, start_date, status, \
            neutral_site, conference_game, home_team_id, home_team, home_points, \
            home_elo_start, home_elo_end, away_team_id, away_team, away_points, \
            away_elo_start, away_elo_end) \
         SELECT * FROM UNNEST($1::int[], $2::text[], $3::int[], $4::timestamptz[], $5::text[], \
            $6::bool[], $7::bool[], $8::int[], $9::text[], $10::int[], $11::int[], $12::int[], \
            $13::int[], $14::text[], $15::int[], $16::int[], $17::int[]) \
         ON CONFLICT (cbbd_game_id) DO UPDATE SET \
            espn_event_id = EXCLUDED.espn_event_id, season = EXCLUDED.season, \
            start_date = EXCLUDED.start_date, status = EXCLUDED.status, \
            neutral_site = EXCLUDED.neutral_site, conference_game = EXCLUDED.conference_game, \
            home_team_id = EXCLUDED.home_team_id, home_team = EXCLUDED.home_team, \
            home_points = EXCLUDED.home_points, home_elo_start = EXCLUDED.home_elo_start, \
            home_elo_end = EXCLUDED.home_elo_end, away_team_id = EXCLUDED.away_team_id, \
            away_team = EXCLUDED.away_team, away_points = EXCLUDED.away_points, \
            away_elo_start = EXCLUDED.away_elo_start, away_elo_end = EXCLUDED.away_elo_end, \
            fetched_at = now()",
    )
    .bind(games.iter().map(|g| g.id).collect::<Vec<_>>())
    .bind(
        games
            .iter()
            .map(|g| g.source_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind(games.iter().map(|g| g.season).collect::<Vec<_>>())
    .bind(games.iter().map(|g| g.start_date).collect::<Vec<_>>())
    .bind(games.iter().map(|g| g.status.clone()).collect::<Vec<_>>())
    .bind(games.iter().map(|g| g.neutral_site).collect::<Vec<_>>())
    .bind(games.iter().map(|g| g.conference_game).collect::<Vec<_>>())
    .bind(col(|g| g.home_team_id))
    .bind(
        games
            .iter()
            .map(|g| g.home_team.clone())
            .collect::<Vec<_>>(),
    )
    .bind(col(|g| g.home_points))
    .bind(col(|g| g.home_team_elo_start))
    .bind(col(|g| g.home_team_elo_end))
    .bind(col(|g| g.away_team_id))
    .bind(
        games
            .iter()
            .map(|g| g.away_team.clone())
            .collect::<Vec<_>>(),
    )
    .bind(col(|g| g.away_points))
    .bind(col(|g| g.away_team_elo_start))
    .bind(col(|g| g.away_team_elo_end))
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace every team row for the games in `rows`, and stamp `first_box_at`
/// on games seeing their first box score.
async fn replace_team_rows(pool: &PgPool, rows: &[TeamGame]) -> Result<usize, sqlx::Error> {
    if rows.is_empty() {
        return Ok(0);
    }
    let game_ids: Vec<i32> = rows
        .iter()
        .map(|r| r.game_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let s = |f: fn(&TeamGame) -> Option<i32>| rows.iter().map(f).collect::<Vec<_>>();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cbbd_team_game_stats WHERE cbbd_game_id = ANY($1)")
        .bind(&game_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO cbbd_team_game_stats (cbbd_game_id, cbbd_team_id, team, is_home, points, \
            fgm, fga, tpm, tpa, ftm, fta, off_rebounds, def_rebounds, total_rebounds, assists, \
            steals, blocks, turnovers, fouls, possessions) \
         SELECT * FROM UNNEST($1::int[], $2::int[], $3::text[], $4::bool[], $5::int[], \
            $6::int[], $7::int[], $8::int[], $9::int[], $10::int[], $11::int[], $12::int[], \
            $13::int[], $14::int[], $15::int[], $16::int[], $17::int[], $18::int[], $19::int[], \
            $20::int[])",
    )
    .bind(rows.iter().map(|r| r.game_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.team_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.team.clone()).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.is_home).collect::<Vec<_>>())
    .bind(s(|r| r.team_stats.points.total))
    .bind(s(|r| r.team_stats.field_goals.made))
    .bind(s(|r| r.team_stats.field_goals.attempted))
    .bind(s(|r| r.team_stats.three_point_field_goals.made))
    .bind(s(|r| r.team_stats.three_point_field_goals.attempted))
    .bind(s(|r| r.team_stats.free_throws.made))
    .bind(s(|r| r.team_stats.free_throws.attempted))
    .bind(s(|r| r.team_stats.rebounds.offensive))
    .bind(s(|r| r.team_stats.rebounds.defensive))
    .bind(s(|r| r.team_stats.rebounds.total))
    .bind(s(|r| r.team_stats.assists))
    .bind(s(|r| r.team_stats.steals))
    .bind(s(|r| r.team_stats.blocks))
    .bind(s(|r| r.team_stats.turnovers.total))
    .bind(s(|r| r.team_stats.fouls.total))
    .bind(s(|r| r.team_stats.possessions))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE cbbd_games SET first_box_at = now() \
         WHERE cbbd_game_id = ANY($1) AND first_box_at IS NULL",
    )
    .bind(&game_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows.len())
}

/// Replace every player row for the games in `rows`. A player CBBD later drops
/// from a box score is deleted with the rest, not left behind.
async fn replace_player_rows(pool: &PgPool, games: &[PlayersGame]) -> Result<usize, sqlx::Error> {
    let game_ids: Vec<i32> = games
        .iter()
        .map(|g| g.game_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if game_ids.is_empty() {
        return Ok(0);
    }
    // Flatten to one entry per player line, carrying its game's keys. A player
    // listed twice in one game (a feed glitch) keeps the first line, so the
    // insert cannot fail on the primary key.
    let mut seen = HashSet::new();
    let lines: Vec<(&PlayersGame, &crate::cbbd::PlayerLine)> = games
        .iter()
        .flat_map(|g| g.players.iter().map(move |p| (g, p)))
        .filter(|(g, p)| seen.insert((g.game_id, p.athlete_id)))
        .collect();
    let s = |f: fn(&crate::cbbd::PlayerLine) -> Option<i32>| {
        lines.iter().map(|(_, p)| f(p)).collect::<Vec<_>>()
    };
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cbbd_player_game_stats WHERE cbbd_game_id = ANY($1)")
        .bind(&game_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO cbbd_player_game_stats (cbbd_game_id, cbbd_athlete_id, espn_athlete_id, \
            cbbd_team_id, is_home, name, starter, minutes, points, fgm, fga, tpm, tpa, ftm, fta, \
            off_rebounds, def_rebounds, total_rebounds, assists, steals, blocks, turnovers, fouls) \
         SELECT * FROM UNNEST($1::int[], $2::int[], $3::text[], $4::int[], $5::bool[], \
            $6::text[], $7::bool[], $8::int[], $9::int[], $10::int[], $11::int[], $12::int[], \
            $13::int[], $14::int[], $15::int[], $16::int[], $17::int[], $18::int[], $19::int[], \
            $20::int[], $21::int[], $22::int[], $23::int[])",
    )
    .bind(lines.iter().map(|(g, _)| g.game_id).collect::<Vec<_>>())
    .bind(lines.iter().map(|(_, p)| p.athlete_id).collect::<Vec<_>>())
    .bind(
        lines
            .iter()
            .map(|(_, p)| p.athlete_source_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind(lines.iter().map(|(g, _)| g.team_id).collect::<Vec<_>>())
    .bind(lines.iter().map(|(g, _)| g.is_home).collect::<Vec<_>>())
    .bind(
        lines
            .iter()
            .map(|(_, p)| p.name.clone())
            .collect::<Vec<_>>(),
    )
    .bind(lines.iter().map(|(_, p)| p.starter).collect::<Vec<_>>())
    .bind(s(|p| p.minutes))
    .bind(s(|p| p.points))
    .bind(s(|p| p.field_goals.made))
    .bind(s(|p| p.field_goals.attempted))
    .bind(s(|p| p.three_point_field_goals.made))
    .bind(s(|p| p.three_point_field_goals.attempted))
    .bind(s(|p| p.free_throws.made))
    .bind(s(|p| p.free_throws.attempted))
    .bind(s(|p| p.rebounds.offensive))
    .bind(s(|p| p.rebounds.defensive))
    .bind(s(|p| p.rebounds.total))
    .bind(s(|p| p.assists))
    .bind(s(|p| p.steals))
    .bind(s(|p| p.blocks))
    .bind(s(|p| p.turnovers))
    .bind(s(|p| p.fouls))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(lines.len())
}

/// Per-game coverage counts from one day's plays.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Coverage {
    pub(crate) plays: i32,
    pub(crate) shots: i32,
    pub(crate) shots_with_location: i32,
    pub(crate) plays_full_on_floor: i32,
}

fn summarize(plays: &[PlayCoverage]) -> BTreeMap<i32, Coverage> {
    let mut out: BTreeMap<i32, Coverage> = BTreeMap::new();
    for p in plays {
        let c = out.entry(p.game_id).or_default();
        c.plays += 1;
        if p.shooting_play {
            c.shots += 1;
            if p.has_shot_location() {
                c.shots_with_location += 1;
            }
        }
        if p.full_on_floor() {
            c.plays_full_on_floor += 1;
        }
    }
    out
}

/// Fetch one UTC day's plays and write a coverage row per game already in
/// `cbbd_games`. Returns (games summarized, plays read). A game listed in two
/// days' responses is listed whole in each, so writing it twice is harmless.
async fn ingest_plays(
    pool: &PgPool,
    client: &CbbdClient,
    day: NaiveDate,
) -> Result<(Vec<i32>, usize), ShadowError> {
    let query = [("date", day.to_string())];
    let plays: Vec<PlayCoverage> = client.get("/plays/date", &query, None).await?;
    let n = plays.len();
    let by_game = summarize(&plays);
    drop(plays);
    Ok((write_summaries(pool, &by_game).await?, n))
}

/// Upsert one coverage row per game in `by_game` that `cbbd_games` holds, and
/// return those games. Shared with `cbbd_plays`, which reads the same days.
pub(crate) async fn write_summaries(
    pool: &PgPool,
    by_game: &BTreeMap<i32, Coverage>,
) -> Result<Vec<i32>, sqlx::Error> {
    let ids: Vec<i32> = by_game.keys().copied().collect();
    // Only games the box-score pass stored: the foreign key needs them, and a
    // game outside the window's games has nothing to compare against.
    let known: Vec<i32> =
        sqlx::query_scalar("SELECT cbbd_game_id FROM cbbd_games WHERE cbbd_game_id = ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await?;
    let known: HashSet<i32> = known.into_iter().collect();
    let rows: Vec<(&i32, &Coverage)> = by_game
        .iter()
        .filter(|(id, _)| known.contains(id))
        .collect();
    if rows.is_empty() {
        return Ok(vec![]);
    }
    sqlx::query(
        "INSERT INTO cbbd_game_play_summary (cbbd_game_id, plays, shots, shots_with_location, \
            plays_full_on_floor) \
         SELECT * FROM UNNEST($1::int[], $2::int[], $3::int[], $4::int[], $5::int[]) \
         ON CONFLICT (cbbd_game_id) DO UPDATE SET plays = EXCLUDED.plays, \
            shots = EXCLUDED.shots, shots_with_location = EXCLUDED.shots_with_location, \
            plays_full_on_floor = EXCLUDED.plays_full_on_floor, fetched_at = now()",
    )
    .bind(rows.iter().map(|(id, _)| **id).collect::<Vec<_>>())
    .bind(rows.iter().map(|(_, c)| c.plays).collect::<Vec<_>>())
    .bind(rows.iter().map(|(_, c)| c.shots).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|(_, c)| c.shots_with_location)
            .collect::<Vec<_>>(),
    )
    .bind(
        rows.iter()
            .map(|(_, c)| c.plays_full_on_floor)
            .collect::<Vec<_>>(),
    )
    .execute(pool)
    .await?;
    Ok(rows.iter().map(|(id, _)| **id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_tip_belongs_to_its_eastern_date() {
        // A 7:30 PM ET tip on Nov 18 is 00:30Z on the 19th.
        let start = chrono::DateTime::parse_from_rfc3339("2026-11-19T00:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            eastern_date(start),
            NaiveDate::from_ymd_opt(2026, 11, 18).unwrap()
        );
    }

    #[test]
    fn summarize_counts_coverage_per_game() {
        let plays: Vec<PlayCoverage> = serde_json::from_value(serde_json::json!([
            {"gameId": 1, "shootingPlay": true,
             "shotInfo": {"location": {"x": 1, "y": 2}},
             "onFloor": [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}]},
            {"gameId": 1, "shootingPlay": true, "shotInfo": {"location": null},
             "onFloor": [{}, {}]},
            {"gameId": 2, "shootingPlay": false}
        ]))
        .unwrap();
        let s = summarize(&plays);
        assert_eq!(
            s[&1],
            Coverage {
                plays: 2,
                shots: 2,
                shots_with_location: 1,
                plays_full_on_floor: 1
            }
        );
        assert_eq!(
            s[&2],
            Coverage {
                plays: 1,
                ..Default::default()
            }
        );
    }
}
