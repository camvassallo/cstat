//! The daytime refresh: ESPN betting lines and broadcast listings between
//! nightly runs, on its own Railway cron (`railway.hourly.json`).
//!
//! The nightly fetches lines once, at 09:30 UTC, so tomorrow's slate stayed
//! blank all day and a game's line stayed at its 4:30 AM number until the next
//! morning. This job keeps the Schedule tab current through the day and writes
//! nothing but display data. NatStat's lines are the record panel's fallback
//! and stay with the nightly, so this job makes no NatStat calls.
//!
//! **What it must not do.** No box scores and no `compute_all`. Compute without
//! a fresh Torvik pull is the stale-CamPom bug the nightly's ordering exists to
//! prevent, and a mid-slate compute would move ratings and projections for
//! games still to tip. Its absence is the design, not a gap.
//!
//! Three rules keep it from disturbing what reads the nightly's ledger:
//!
//! - **Its steps have their own names.** `/api/health/ingest` and the sync
//!   guard read freshness by step name, so a step shared with the nightly would
//!   let this job make a dead nightly look alive.
//! - **It never stamps a window.** `first_uncovered_ingest_date` reads stamped
//!   windows as dates fully ingested, and this job ingests no box scores. Never
//!   call `set_window` on this ledger.
//! - **It reads only games that have not tipped.** The nightly owns the closing
//!   line, which it reads the morning after.

use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use sqlx::PgPool;
use tracing::{info, warn};

use crate::ingest::season::{broadcasts_window_step, espn_lines_window_step};
use crate::run_ledger::{RunLedger, StepStatus};

pub const STEP_BROADCASTS: &str = "hourly_broadcasts";
pub const STEP_ESPN_LINES: &str = "hourly_espn_lines";
pub const STEP_FREEZE: &str = "hourly_pregame_freeze";

/// The Eastern days the job covers: today and tomorrow.
///
/// Two days is also what keeps this job from deleting anything:
/// `broadcasts::run` prunes a game only when the days either side of it were
/// fetched, and neither day here has both neighbours. Pruning stays with the
/// nightly's wider window.
pub fn slate_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (today, today + Duration::days(1))
}

/// Is there an unplayed game in `from..=to`? False on an empty day and for the
/// whole off-season, when the cron still fires but has nothing to read.
pub async fn slate_ahead(
    pool: &PgPool,
    season: i32,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM games \
         WHERE season = $1 AND game_date BETWEEN $2 AND $3 \
           AND (home_score IS NULL OR away_score IS NULL))",
    )
    .bind(season)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await
}

/// Did the last upcoming sweep go unfrozen? Only then is there anything to
/// freeze: the upcoming rows change only in the nightly, so otherwise a freeze
/// here would rewrite identical rows and move their `captured_at`, which
/// `backfill_played` reads as when the projection was made.
///
/// Those rows were computed before today's games tipped, so a catch-up after a
/// game with no known time has started still records a pregame number.
pub async fn freeze_owed(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(
            (SELECT max(ended_at) FROM ingest_runs
              WHERE step = 'upcoming_projections' AND status = 'ok')
            > COALESCE(
                (SELECT max(ended_at) FROM ingest_runs
                  WHERE step IN ('pregame_freeze', $1) AND status = 'ok'),
                '-infinity'),
            false)",
    )
    .bind(STEP_FREEZE)
    .fetch_one(pool)
    .await
}

/// Run the refresh. Returns whether there was a slate to refresh.
///
/// Every step is best-effort and independent: a failure is a `failed` ledger
/// row and the next run retries, while the nightly's own reads still land. So
/// it sends no Slack alert, which at this cadence would only teach people to
/// mute the channel.
pub async fn run(pool: &PgPool, season: i32) -> Result<bool> {
    // Live requests, which a replay does not serve from fixtures.
    if crate::simulated_today().is_some() {
        info!("simulated clock; skipping the hourly refresh");
        return Ok(false);
    }
    let (from, to) = slate_window(crate::today_eastern());
    if !slate_ahead(pool, season, from, to).await? {
        info!(
            season,
            "no unplayed games today or tomorrow; nothing to refresh"
        );
        return Ok(false);
    }

    let ledger = RunLedger::start(pool, season);

    // Before the ESPN lines, which read its event matches.
    broadcasts_window_step(pool, &ledger, STEP_BROADCASTS, from, to).await;

    let now_eastern = crate::eastern_datetime(Utc::now());
    espn_lines_window_step(pool, &ledger, STEP_ESPN_LINES, from, to, Some(now_eastern)).await;

    let t0 = Utc::now();
    match freeze_owed(pool).await {
        Ok(false) => {}
        Ok(true) => {
            match crate::game_projections::freeze_pregame(pool, season, now_eastern).await {
                Ok(r) => {
                    ledger
                        .record(
                            STEP_FREEZE,
                            StepStatus::Ok,
                            Some((r.model + r.preseason) as i64),
                            t0,
                            None,
                        )
                        .await;
                }
                Err(e) => {
                    let msg = e.to_string();
                    warn!(season, error = %msg, "pregame freeze catch-up failed");
                    ledger
                        .record(STEP_FREEZE, StepStatus::Failed, None, t0, Some(&msg))
                        .await;
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            warn!(season, error = %msg, "could not read whether a pregame freeze is owed");
            ledger
                .record(STEP_FREEZE, StepStatus::Failed, None, t0, Some(&msg))
                .await;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_ledger::BOX_SCORE_STEPS;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn the_slate_window_is_today_and_tomorrow() {
        assert_eq!(
            slate_window(d("2026-11-06")),
            (d("2026-11-06"), d("2026-11-07"))
        );
    }

    /// Freshness is read by step name, so sharing one with the nightly would
    /// let this job vouch for it.
    #[test]
    fn no_step_shares_a_name_with_the_nightly() {
        for step in [STEP_BROADCASTS, STEP_ESPN_LINES, STEP_FREEZE] {
            assert!(step.starts_with("hourly_"), "{step}");
            assert!(!BOX_SCORE_STEPS.contains(&step), "{step}");
        }
    }
}
