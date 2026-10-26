//! Full-season schedule refresh (#406).
//!
//! The nightly's `games` step ingests only its date window, so a game NatStat
//! publishes after the initial `games --year` load reached `games` on the day
//! it was played and not before. Bracket events are the sharp case: a
//! conference tournament's semifinal exists in the feed only once the
//! quarterfinal is final (the feed has no "Winner of …" placeholders), so
//! without a daily full-season pull it never appeared on a schedule ahead of
//! tipoff. On 2026-10-26 the 2026-27 schedule held 1,827 of 5,510 published
//! games.
//!
//! The refresh is cheap to fetch (~56 pages) but the per-row upsert in
//! `games.rs` is not cheap to WRITE over the Railway link — a 5,510-game run
//! took more than ten minutes. So this module writes the whole season in one
//! batched statement and skips rows that would not change, which makes an
//! ordinary night a no-op.

use super::games::{GameRow, SkipCounts, parse_game};
use crate::NatStatClient;
use crate::client::NatStatError;
use crate::extract_results;
use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{info, warn};

/// Tables whose rows block deleting a `games` row: every non-cascading
/// foreign key onto `games` except `schedules`, which the prune clears itself
/// (`compute_schedules` rebuilds it from `games` anyway). `game_projections`
/// cascades. A test checks this list against the catalog, so a new table
/// referencing `games` fails the build rather than the nightly.
pub const PRUNE_BLOCKERS: [&str; 7] = [
    "game_forecasts",
    "lineup_stints",
    "natstat_lineup_games",
    "natstat_lineups",
    "play_by_play",
    "player_game_stats",
    "team_game_stats",
];

/// A prune larger than this share of the season's unplayed games is refused.
/// A fetch can report itself complete and still be wrong; a real night loses a
/// handful of cancelled games, never a tenth of the schedule.
const MAX_PRUNE_SHARE: f64 = 0.10;
/// ...with a floor, so a short remaining schedule in March can still lose the
/// few games a bracket reshuffle drops.
const MAX_PRUNE_FLOOR: i64 = 25;

/// What one season's feed returned.
#[derive(Debug, Default)]
pub struct SeasonFetch {
    pub rows: Vec<GameRow>,
    pub skipped: SkipCounts,
    /// Every result the feed reported was received. Only a complete fetch may
    /// prune: a game absent from a truncated pull is not evidence of anything.
    pub complete: bool,
}

/// What [`apply_season_schedule`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScheduleRefresh {
    pub inserted: u64,
    pub updated: u64,
    pub pruned: u64,
    /// Why the prune did not run, or what it declined to delete.
    pub prune_note: Option<String>,
    /// Team-days with two or more future games — upstream noise (24 of them in
    /// the 2027 feed on 2026-10-26), reported rather than repaired.
    pub double_booked: u64,
}

impl ScheduleRefresh {
    pub fn written(&self) -> u64 {
        self.inserted + self.updated + self.pruned
    }
}

/// Fetch every page of a season's `games` feed.
pub async fn fetch_season(
    client: &NatStatClient,
    season: i32,
) -> Result<SeasonFetch, NatStatError> {
    let pages = client
        .get_all_pages("games", Some(&season.to_string()), None)
        .await?;
    let reported = pages
        .first()
        .and_then(|p| NatStatClient::parse_meta(p).results_total);

    let mut fetch = SeasonFetch::default();
    let mut received: u64 = 0;
    for page in &pages {
        for game in extract_results(page) {
            received += 1;
            match parse_game(game) {
                Ok(row) => fetch.rows.push(row),
                Err(reason) => fetch.skipped.record(reason),
            }
        }
    }
    fetch.complete = reported.is_some_and(|total| total > 0 && received >= total);
    if !fetch.complete {
        warn!(
            season,
            received,
            ?reported,
            "schedule fetch incomplete; prune will be skipped"
        );
    }
    Ok(fetch)
}

/// Write a season's schedule: insert new games, update changed ones, and prune
/// unplayed games the feed no longer lists.
///
/// Update rules differ from the per-row `upsert_game` because this touches
/// every game of the season every night, including ones `compute_all` has
/// since corrected:
/// - Scores, status, venue and the rest only ever fill in (`COALESCE`), so a
///   schedule row carrying none cannot blank a completed game.
/// - `game_date` moves only while the game is unplayed — a reschedule under
///   the same id.
/// - `is_neutral_site` is promote-only and `is_conference` fills only when
///   unknown: `compute_all` derives both, and resetting them would rewrite
///   thousands of rows a night for compute to flip back.
/// - A natstat id already stored under another season is left alone (the
///   cross-season duplicate-game case).
///
/// Rows whose effective values would not change are not written at all, so a
/// refresh of an unchanged feed writes zero rows.
pub async fn apply_season_schedule(
    pool: &PgPool,
    season: i32,
    rows: &[GameRow],
    fetch_complete: bool,
    today: NaiveDate,
) -> Result<ScheduleRefresh, sqlx::Error> {
    let mut out = ScheduleRefresh::default();

    let teams = crate::team_ids_by_code_for_season(pool, season).await?;
    let resolve = |code: &Option<String>| code.as_ref().and_then(|c| teams.get(c).copied());

    // The feed repeats a game now and then, and one INSERT ... ON CONFLICT
    // cannot touch the same row twice. Last occurrence wins, like the per-row
    // path that wrote them in order.
    let mut by_id: HashMap<&str, &GameRow> = HashMap::with_capacity(rows.len());
    for r in rows {
        by_id.insert(r.natstat_id.as_str(), r);
    }
    let mut unique: Vec<&GameRow> = by_id.into_values().collect();
    unique.sort_by(|a, b| a.natstat_id.cmp(&b.natstat_id));

    let mut tx = pool.begin().await?;

    if !unique.is_empty() {
        let col =
            |f: fn(&GameRow) -> Option<String>| unique.iter().map(|r| f(r)).collect::<Vec<_>>();
        let int = |f: fn(&GameRow) -> Option<i32>| unique.iter().map(|r| f(r)).collect::<Vec<_>>();
        let inserted: Vec<bool> = sqlx::query_scalar(
            "INSERT INTO games AS g (id, natstat_id, season, game_date, home_team_id, away_team_id,
                 home_score, away_score, is_neutral_site, is_conference, is_postseason, venue,
                 venue_code, overtime, attendance, status,
                 home_half1, home_half2, away_half1, away_half2)
             SELECT gen_random_uuid(), s.natstat_id, $1, s.game_date, s.home_team_id, s.away_team_id,
                    s.home_score, s.away_score, s.is_neutral, s.is_conference, s.is_postseason,
                    s.venue, s.venue_code, s.overtime, s.attendance, s.status,
                    s.home_half1, s.home_half2, s.away_half1, s.away_half2
             FROM UNNEST($2::text[], $3::date[], $4::uuid[], $5::uuid[], $6::int[], $7::int[],
                         $8::bool[], $9::bool[], $10::bool[], $11::text[], $12::text[], $13::text[],
                         $14::int[], $15::text[], $16::int[], $17::int[], $18::int[], $19::int[])
                  AS s(natstat_id, game_date, home_team_id, away_team_id, home_score, away_score,
                       is_neutral, is_conference, is_postseason, venue, venue_code, overtime,
                       attendance, status, home_half1, home_half2, away_half1, away_half2)
             ON CONFLICT (natstat_id) DO UPDATE SET
                 game_date = CASE WHEN g.home_score IS NULL AND g.away_score IS NULL
                                  THEN EXCLUDED.game_date ELSE g.game_date END,
                 home_team_id = COALESCE(EXCLUDED.home_team_id, g.home_team_id),
                 away_team_id = COALESCE(EXCLUDED.away_team_id, g.away_team_id),
                 home_score = COALESCE(EXCLUDED.home_score, g.home_score),
                 away_score = COALESCE(EXCLUDED.away_score, g.away_score),
                 is_neutral_site = g.is_neutral_site OR EXCLUDED.is_neutral_site,
                 is_conference = COALESCE(g.is_conference, EXCLUDED.is_conference),
                 is_postseason = COALESCE(EXCLUDED.is_postseason, g.is_postseason),
                 venue = COALESCE(EXCLUDED.venue, g.venue),
                 venue_code = COALESCE(EXCLUDED.venue_code, g.venue_code),
                 overtime = COALESCE(EXCLUDED.overtime, g.overtime),
                 attendance = COALESCE(EXCLUDED.attendance, g.attendance),
                 status = COALESCE(EXCLUDED.status, g.status),
                 home_half1 = COALESCE(EXCLUDED.home_half1, g.home_half1),
                 home_half2 = COALESCE(EXCLUDED.home_half2, g.home_half2),
                 away_half1 = COALESCE(EXCLUDED.away_half1, g.away_half1),
                 away_half2 = COALESCE(EXCLUDED.away_half2, g.away_half2),
                 updated_at = now()
             WHERE g.season = EXCLUDED.season
               AND (CASE WHEN g.home_score IS NULL AND g.away_score IS NULL
                         THEN EXCLUDED.game_date ELSE g.game_date END,
                    COALESCE(EXCLUDED.home_team_id, g.home_team_id),
                    COALESCE(EXCLUDED.away_team_id, g.away_team_id),
                    COALESCE(EXCLUDED.home_score, g.home_score),
                    COALESCE(EXCLUDED.away_score, g.away_score),
                    g.is_neutral_site OR EXCLUDED.is_neutral_site,
                    COALESCE(g.is_conference, EXCLUDED.is_conference),
                    COALESCE(EXCLUDED.is_postseason, g.is_postseason),
                    COALESCE(EXCLUDED.venue, g.venue),
                    COALESCE(EXCLUDED.venue_code, g.venue_code),
                    COALESCE(EXCLUDED.overtime, g.overtime),
                    COALESCE(EXCLUDED.attendance, g.attendance),
                    COALESCE(EXCLUDED.status, g.status),
                    COALESCE(EXCLUDED.home_half1, g.home_half1),
                    COALESCE(EXCLUDED.home_half2, g.home_half2),
                    COALESCE(EXCLUDED.away_half1, g.away_half1),
                    COALESCE(EXCLUDED.away_half2, g.away_half2))
                   IS DISTINCT FROM
                   (g.game_date, g.home_team_id, g.away_team_id, g.home_score, g.away_score,
                    g.is_neutral_site, g.is_conference, g.is_postseason, g.venue, g.venue_code,
                    g.overtime, g.attendance, g.status,
                    g.home_half1, g.home_half2, g.away_half1, g.away_half2)
             RETURNING (xmax = 0)",
        )
        .bind(season)
        .bind(unique.iter().map(|r| r.natstat_id.clone()).collect::<Vec<_>>())
        .bind(unique.iter().map(|r| r.game_date).collect::<Vec<_>>())
        .bind(unique.iter().map(|r| resolve(&r.home_code)).collect::<Vec<_>>())
        .bind(unique.iter().map(|r| resolve(&r.away_code)).collect::<Vec<_>>())
        .bind(int(|r| r.home_score))
        .bind(int(|r| r.away_score))
        .bind(unique.iter().map(|r| r.is_neutral).collect::<Vec<_>>())
        .bind(unique.iter().map(|r| r.is_conference).collect::<Vec<_>>())
        .bind(unique.iter().map(|r| r.is_postseason).collect::<Vec<_>>())
        .bind(col(|r| r.venue.clone()))
        .bind(col(|r| r.venue_code.clone()))
        .bind(col(|r| r.overtime.clone()))
        .bind(int(|r| r.attendance))
        .bind(col(|r| r.status.clone()))
        .bind(int(|r| r.home_half1))
        .bind(int(|r| r.home_half2))
        .bind(int(|r| r.away_half1))
        .bind(int(|r| r.away_half2))
        .fetch_all(&mut *tx)
        .await?;
        out.inserted = inserted.iter().filter(|&&i| i).count() as u64;
        out.updated = inserted.len() as u64 - out.inserted;
    }

    out.prune_note = match prune_reason_to_skip(fetch_complete, unique.len()) {
        Some(why) => Some(why.to_string()),
        None => {
            let ids: Vec<String> = unique.iter().map(|r| r.natstat_id.clone()).collect();
            prune_vanished(&mut tx, season, today, &ids, &mut out.pruned).await?
        }
    };

    out.double_booked = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM (
             SELECT team_id, game_date FROM (
                 SELECT home_team_id AS team_id, game_date FROM games
                  WHERE season = $1 AND game_date > $2 AND home_team_id IS NOT NULL
                 UNION ALL
                 SELECT away_team_id, game_date FROM games
                  WHERE season = $1 AND game_date > $2 AND away_team_id IS NOT NULL
             ) t GROUP BY 1, 2 HAVING count(*) > 1
         ) d",
    )
    .bind(season)
    .bind(today)
    .fetch_one(&mut *tx)
    .await? as u64;

    tx.commit().await?;
    info!(
        season,
        inserted = out.inserted,
        updated = out.updated,
        pruned = out.pruned,
        double_booked = out.double_booked,
        prune_note = out.prune_note.as_deref().unwrap_or(""),
        "schedule refreshed"
    );
    Ok(out)
}

/// Why the prune must not run at all, if it must not.
fn prune_reason_to_skip(fetch_complete: bool, fetched: usize) -> Option<&'static str> {
    if !fetch_complete {
        Some("prune skipped: fetch incomplete")
    } else if fetched == 0 {
        Some("prune skipped: feed returned no games")
    } else {
        None
    }
}

/// Delete unplayed future games the feed no longer lists, within the guards.
/// Returns a note when it declined some or all of them.
async fn prune_vanished(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    season: i32,
    today: NaiveDate,
    feed_ids: &[String],
    pruned: &mut u64,
) -> Result<Option<String>, sqlx::Error> {
    let blocked = PRUNE_BLOCKERS
        .iter()
        .map(|t| format!("EXISTS (SELECT 1 FROM {t} x WHERE x.game_id = g.id)"))
        .collect::<Vec<_>>()
        .join(" OR ");
    // Candidates: unplayed, future, and gone from a complete fetch.
    let candidates = "SELECT g.id FROM games g
                       WHERE g.season = $1 AND g.game_date > $2
                         AND g.home_score IS NULL AND g.away_score IS NULL
                         AND NOT (g.natstat_id = ANY($3))";

    let (total, held): (i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*), count(*) FILTER (WHERE {blocked})
           FROM games g WHERE g.id IN ({candidates})"
    ))
    .bind(season)
    .bind(today)
    .bind(feed_ids)
    .fetch_one(&mut **tx)
    .await?;
    if total == 0 {
        return Ok(None);
    }
    let unplayed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM games
          WHERE season = $1 AND game_date > $2 AND home_score IS NULL AND away_score IS NULL",
    )
    .bind(season)
    .bind(today)
    .fetch_one(&mut **tx)
    .await?;
    let cap = MAX_PRUNE_FLOOR.max((unplayed as f64 * MAX_PRUNE_SHARE) as i64);
    if total - held > cap {
        return Ok(Some(format!(
            "prune skipped: {} vanished games exceeds the cap of {cap} ({unplayed} unplayed)",
            total - held
        )));
    }

    *pruned = sqlx::query_scalar::<_, i64>(&format!(
        "WITH doomed AS (
             SELECT g.id FROM games g WHERE g.id IN ({candidates}) AND NOT ({blocked})
         ),
         s AS (DELETE FROM schedules WHERE game_id IN (SELECT id FROM doomed))
         , d AS (DELETE FROM games WHERE id IN (SELECT id FROM doomed) RETURNING 1)
         SELECT count(*) FROM d"
    ))
    .bind(season)
    .bind(today)
    .bind(feed_ids)
    .fetch_one(&mut **tx)
    .await? as u64;

    Ok((held > 0).then(|| format!("kept {held} vanished games that have dependent rows")))
}

#[cfg(test)]
mod tests {
    use super::prune_reason_to_skip;

    #[test]
    fn prune_needs_a_complete_nonempty_fetch() {
        assert!(prune_reason_to_skip(false, 5_000).is_some());
        assert!(prune_reason_to_skip(true, 0).is_some());
        assert_eq!(prune_reason_to_skip(true, 5_000), None);
    }
}
