//! Per-step ingest run ledger (`ingest_runs` table). The nightly orchestrator
//! opens a [`RunLedger`] for one invocation and records each step's outcome as
//! it finishes, so a crash mid-run still leaves a durable audit trail and the
//! freshness/health route can report the last successful run per step.
//!
//! Ledger writes are intentionally **fail-soft**: a failure to record a step
//! must never abort the ingest it is observing — we log a warning and move on.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

/// Season-scoped served tables the row-count sanity gate (M5a) snapshots after
/// each successful compute. In-season these only ever grow or hold flat, so a
/// material shrink vs the prior run is a red flag (a truncated feed, a botched
/// compute wiping rows). Kept deliberately tight to the load-bearing served set.
pub const ROW_COUNT_TABLES: &[&str] = &[
    "games",
    "team_game_stats",
    "player_game_stats",
    "team_season_stats",
    "player_season_stats",
];

/// The window-scoped, load-bearing box-score steps. A date only counts as
/// "covered" for the self-heal scan once **all** of these succeeded for a
/// run — each is a hard-abort step, and they are the only ones whose work is
/// scoped to the run's date window (`elo`/`torvik`/`compute` are season-wide,
/// so they say nothing about which dates were ingested).
pub const BOX_SCORE_STEPS: &[&str] = &["games", "player_perfs", "team_perfs"];

/// A tracked table must drop by more than BOTH thresholds vs the prior run to
/// count as a regression: a relative floor (guards against normal churn) AND an
/// absolute floor (guards against noise on small early-season tables, and lets a
/// few phantom-player/dup-game repair deletions pass without alarm).
const REGRESSION_REL_FLOOR: f64 = 0.05; // >5% drop
const REGRESSION_ABS_FLOOR: i64 = 25; // and >25 rows

/// Outcome of a single pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Ok,
    Failed,
    Skipped,
}

impl StepStatus {
    /// Stored string form (the `ingest_runs.status` column).
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
        }
    }
}

/// Records each step of one nightly invocation to `ingest_runs`. All of a
/// run's step rows share its [`run_id`](RunLedger::run_id).
pub struct RunLedger<'a> {
    pool: &'a PgPool,
    run_id: Uuid,
    season: i32,
    /// The date window this run's ingest covers, stamped onto every step row
    /// (migration 043). Set once via [`set_window`](RunLedger::set_window) after
    /// the self-heal has settled the final window. `None` until then — the
    /// coverage scan ignores NULL windows.
    window: Option<(NaiveDate, NaiveDate)>,
}

impl<'a> RunLedger<'a> {
    /// Open a ledger for a single run, minting the grouping `run_id`.
    pub fn start(pool: &'a PgPool, season: i32) -> Self {
        Self {
            pool,
            run_id: Uuid::new_v4(),
            season,
            window: None,
        }
    }

    /// Stamp the date window this run covers onto every subsequent step row.
    /// Call once, after the self-heal has settled the final window and before
    /// the first [`record`](RunLedger::record) — this is what lets
    /// [`first_uncovered_ingest_date`] scan real coverage rather than guess it
    /// from wall-clock finish times.
    pub fn set_window(&mut self, start: NaiveDate, end: NaiveDate) {
        self.window = Some((start, end));
    }

    /// The grouping id shared by every step row of this run.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Record one finished step. Fail-soft: a ledger write error is logged and
    /// swallowed so it can't abort the pipeline it's observing.
    pub async fn record(
        &self,
        step: &str,
        status: StepStatus,
        rows_touched: Option<i64>,
        started_at: DateTime<Utc>,
        error: Option<&str>,
    ) {
        let ended_at = Utc::now();
        let (window_start, window_end) = match self.window {
            Some((s, e)) => (Some(s), Some(e)),
            None => (None, None),
        };
        let res = sqlx::query(
            "INSERT INTO ingest_runs \
             (run_id, season, step, status, rows_touched, started_at, ended_at, error, \
              window_start, window_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(self.run_id)
        .bind(self.season)
        .bind(step)
        .bind(status.as_str())
        .bind(rows_touched)
        .bind(started_at)
        .bind(ended_at)
        .bind(error)
        .bind(window_start)
        .bind(window_end)
        .execute(self.pool)
        .await;

        if let Err(e) = res {
            warn!(step, error = %e, "failed to record ingest_runs step; continuing");
        }
    }

    /// Read the season-scoped row count of every [`ROW_COUNT_TABLES`] table
    /// (M5a). Pure read — persisting is a separate step ([`persist_counts`]) so
    /// the caller can compare *before* deciding whether these counts deserve to
    /// become the next run's baseline. Fail-soft: a per-table query error is
    /// logged and that table is skipped, never aborting the run it observes.
    ///
    /// [`persist_counts`]: RunLedger::persist_counts
    pub async fn snapshot_counts(&self) -> Vec<(&'static str, i64)> {
        let mut counts = Vec::new();
        for &table in ROW_COUNT_TABLES {
            // `table` is a compile-time constant from ROW_COUNT_TABLES, never
            // user input — safe to interpolate into the count query.
            let n: i64 = match sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE season = $1"
            ))
            .bind(self.season)
            .fetch_one(self.pool)
            .await
            {
                Ok(n) => n,
                Err(e) => {
                    warn!(table, error = %e, "row-count snapshot query failed; skipping table");
                    continue;
                }
            };
            counts.push((table, n));
        }
        counts
    }

    /// Persist a [`snapshot_counts`] result under this run's id, making it the
    /// baseline the next run compares against.
    ///
    /// **Only call this for counts that passed the gate.** Persisting a
    /// regressed snapshot would make the corruption the new normal: the next run
    /// would compare corrupt-to-corrupt, see no drop, and post the green SUCCESS
    /// Slack summary — so the gate would alert exactly once and then actively
    /// assert health over a broken table. (The dead-man's-switch heartbeat is
    /// green on a degraded run either way; the Slack summary is what changes.)
    /// Holding the last-known-good baseline instead keeps the gate firing every
    /// night until the counts actually recover.
    ///
    /// Fail-soft: a per-table insert error is logged and skipped.
    ///
    /// [`snapshot_counts`]: RunLedger::snapshot_counts
    pub async fn persist_counts(&self, counts: &[(&'static str, i64)]) {
        for (table, n) in counts {
            let res = sqlx::query(
                "INSERT INTO ingest_run_table_counts (run_id, season, table_name, row_count) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(self.run_id)
            .bind(self.season)
            .bind(table)
            .bind(n)
            .execute(self.pool)
            .await;
            if let Err(e) = res {
                warn!(table, error = %e, "failed to persist run table count; continuing");
            }
        }
    }

    /// Load the most recent *prior* run's row-count snapshot for this season
    /// (excludes the current run). Empty if this is the first snapshotting run.
    /// Fail-soft: a query error yields an empty vec (no prior data → no gate).
    pub async fn prior_run_table_counts(&self) -> Vec<(String, i64)> {
        let prior_run: Option<Uuid> = sqlx::query_scalar(
            "SELECT run_id FROM ingest_run_table_counts \
             WHERE season = $1 AND run_id <> $2 \
             ORDER BY recorded_at DESC LIMIT 1",
        )
        .bind(self.season)
        .bind(self.run_id)
        .fetch_optional(self.pool)
        .await
        .unwrap_or(None);

        let Some(prior_run) = prior_run else {
            return Vec::new();
        };

        sqlx::query_as::<_, (String, i64)>(
            "SELECT table_name, row_count FROM ingest_run_table_counts WHERE run_id = $1",
        )
        .bind(prior_run)
        .fetch_all(self.pool)
        .await
        .unwrap_or_default()
    }
}

/// The **earliest game date we have not fully ingested**, searching back
/// `lookback_days` from `before` — the signal the nightly self-heal (M5b) uses
/// to widen a defaulted window over skipped nights. `None` = nothing to heal.
/// Fail-soft: any query error yields `None` (the heal simply no-ops). Excludes
/// `exclude_run_id` so the in-flight run can't match itself.
///
/// This is a true gap scan, not a high-water mark. A `MAX(window_end)` frontier
/// assumes coverage is *contiguous*, which breaks on the most likely operator
/// move during an outage: cron dies after 11-05 leaving 11-06/07 skipped, the
/// operator runs `nightly --from 11-07 --to 11-08` (a range ending **today** —
/// the natural thing to type), and the high-water mark jumps to 11-08. No gap is
/// visible and 11-06 is lost forever, silently. Scanning for the first uncovered
/// date finds 11-06 regardless of what landed after it.
///
/// A date counts as covered only when **every** window-scoped box-score step
/// succeeded for some run ([`BOX_SCORE_STEPS`]). `games` is step 1 and records
/// `ok` before `player_perfs` can abort the run, so keying on `games` alone
/// would let a half-finished run mark its window covered: the games rows land
/// with final scores, the statlines never do, and no later run re-ingests them.
/// That hole is self-perpetuating — the invariant gate would report "completed
/// game missing a `team_game_stats` side" every night forever.
///
/// Two floors keep the scan honest:
/// - **`window_end <= as_of`** discards absurd future windows. A typo'd
///   `--to 2030-01-01` would otherwise mark every date covered and disable
///   healing permanently.
/// - **`MIN(window_start)`** stops the scan from looking back before the first
///   window we ever recorded. Without it, the run right after migration 043
///   (when all prior rows have NULL windows) would see the whole lookback as
///   uncovered and pull `lookback_days` of NatStat for nothing.
///
/// `lookback_days` bounds how far back a hole stays visible. It must exceed the
/// caller's heal cap, or [`heal_window`] could never report an unrecoverable
/// shortfall; past it, an un-backfilled hole ages out rather than degrading
/// every run forever.
pub async fn first_uncovered_ingest_date(
    pool: &PgPool,
    exclude_run_id: Uuid,
    before: NaiveDate,
    as_of: NaiveDate,
    lookback_days: i64,
) -> Option<NaiveDate> {
    let horizon = before - chrono::Duration::days(lookback_days);
    let last = before - chrono::Duration::days(1);
    sqlx::query_scalar(
        "WITH complete AS ( \
             SELECT run_id, MIN(window_start) AS ws, MAX(window_end) AS we \
             FROM ingest_runs \
             WHERE step = ANY($1) AND status = 'ok' AND run_id <> $2 \
               AND window_start IS NOT NULL AND window_end IS NOT NULL \
               AND window_end <= $3 \
             GROUP BY run_id \
             HAVING COUNT(DISTINCT step) = $4 \
         ), \
         horizon AS ( \
             SELECT CASE WHEN MIN(ws) IS NULL THEN NULL \
                         ELSE GREATEST($5::date, MIN(ws)) END AS lo \
             FROM complete \
         ) \
         SELECT MIN(d)::date \
         FROM horizon, generate_series(horizon.lo, $6::date, INTERVAL '1 day') AS d \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM complete c WHERE d::date BETWEEN c.ws AND c.we \
         )",
    )
    .bind(BOX_SCORE_STEPS)
    .bind(exclude_run_id)
    .bind(as_of)
    .bind(BOX_SCORE_STEPS.len() as i64)
    .bind(horizon)
    .bind(last)
    .fetch_one(pool)
    .await
    .ok()
    .flatten()
}

/// A self-heal window widening (M5b) — the result of [`heal_window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealPlan {
    /// The widened window start.
    pub from: NaiveDate,
    /// Days between the gap start and [`from`](HealPlan::from) that the
    /// `max_heal_days` cap refused to re-ingest. `0` = the gap is fully healed.
    /// **Non-zero means the heal is only PARTIAL**: those game dates stay
    /// un-ingested and need a manual `nightly --from <last success>` backfill.
    /// The caller degrades the run on this, because silently serving a
    /// permanent hole is exactly what the gate exists to prevent.
    pub unrecovered_days: i64,
}

/// Given a defaulted `[default_from, default_to]` window and `gap_start` — the
/// earliest un-ingested date, from [`first_uncovered_ingest_date`] — return the
/// widened window, or `None` when the default window already covers the gap.
/// The widened `from` is `gap_start` itself, floored at
/// `default_to − max_heal_days` so a long off-season silence can't trigger a
/// huge NatStat pull. When that floor bites, the plan reports the days it could
/// not recover rather than pretending the gap is closed. Pure — unit-tested.
pub fn heal_window(
    default_from: NaiveDate,
    default_to: NaiveDate,
    gap_start: Option<NaiveDate>,
    max_heal_days: i64,
) -> Option<HealPlan> {
    let gap_start = gap_start?;
    // The gap begins at/after the default start → the default window covers it.
    if gap_start >= default_from {
        return None;
    }
    let floor = default_to - chrono::Duration::days(max_heal_days);
    let healed = gap_start.max(floor);
    // The floor may pull `healed` back up to (or past) default_from — then
    // there's nothing to widen.
    if healed >= default_from {
        return None;
    }
    Some(HealPlan {
        from: healed,
        unrecovered_days: (healed - gap_start).num_days(),
    })
}

/// Compare a prior run's snapshot against the current counts and return a human
/// line per *material* regression (a tracked table that shrank by more than both
/// [`REGRESSION_REL_FLOOR`] and [`REGRESSION_ABS_FLOOR`]). Tables absent from the
/// prior snapshot (first run, or a newly-tracked table) are skipped. Pure —
/// unit-tested.
pub fn detect_count_regressions(
    prior: &[(String, i64)],
    current: &[(&'static str, i64)],
) -> Vec<String> {
    let mut out = Vec::new();
    for (table, cur) in current {
        let Some((_, prev)) = prior.iter().find(|(t, _)| t == table) else {
            continue;
        };
        let drop = prev - cur;
        if drop > REGRESSION_ABS_FLOOR && (drop as f64) > (*prev as f64) * REGRESSION_REL_FLOOR {
            out.push(format!("{table}: {prev} → {cur} (−{drop})"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_are_stable() {
        // The health route + alerting query on these literals — they must not
        // drift.
        assert_eq!(StepStatus::Ok.as_str(), "ok");
        assert_eq!(StepStatus::Failed.as_str(), "failed");
        assert_eq!(StepStatus::Skipped.as_str(), "skipped");
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn heal_window_no_gap_when_gap_starts_inside_default_window() {
        // Gap starts at default_from → the default window already covers it.
        assert_eq!(
            heal_window(d("2026-11-07"), d("2026-11-08"), Some(d("2026-11-07")), 14),
            None
        );
        // Gap starts after default_from → still inside the default window.
        assert_eq!(
            heal_window(d("2026-11-07"), d("2026-11-08"), Some(d("2026-11-08")), 14),
            None
        );
    }

    #[test]
    fn heal_window_widens_to_cover_skipped_nights() {
        // Gap opened 3 days back → widen `from` to it; fully closed (well
        // inside the cap).
        assert_eq!(
            heal_window(d("2026-11-07"), d("2026-11-08"), Some(d("2026-11-04")), 14),
            Some(HealPlan {
                from: d("2026-11-04"),
                unrecovered_days: 0,
            })
        );
    }

    #[test]
    fn heal_window_floors_a_long_silence() {
        // Ancient gap start → clamp to default_to − max_heal_days rather than
        // the stale date (bounds the NatStat pull). In production the lookback
        // keeps `gap_start` far tighter than this; the math must still hold.
        assert_eq!(
            heal_window(d("2026-11-07"), d("2026-11-08"), Some(d("2026-06-01")), 14),
            Some(HealPlan {
                from: d("2026-10-25"),
                // 06-01 → 10-25 is beyond the cap and stays un-ingested — the
                // heal is partial and must NOT read as a clean recovery.
                unrecovered_days: 146,
            })
        );
    }

    #[test]
    fn heal_window_reports_days_the_cap_could_not_recover() {
        // In-season outage just past the cap: resuming 12-01 with the gap open
        // since 11-11 heals back only to 11-17, leaving 11-11..11-17 (6 days of
        // real games) un-ingested. The plan must surface that so the run degrades
        // instead of reporting a green "gap healed".
        let plan =
            heal_window(d("2026-11-30"), d("2026-12-01"), Some(d("2026-11-11")), 14).unwrap();
        assert_eq!(plan.from, d("2026-11-17"));
        assert_eq!(plan.unrecovered_days, 6);
    }

    #[test]
    fn heal_window_none_when_no_gap_found() {
        assert_eq!(
            heal_window(d("2026-11-07"), d("2026-11-08"), None, 14),
            None
        );
    }

    #[test]
    fn regressions_flag_only_material_drops() {
        let prior = vec![
            ("games".to_string(), 1000i64),
            ("player_game_stats".to_string(), 40_000),
            ("team_season_stats".to_string(), 360),
        ];
        // games cratered (−300, 30%) → flagged; player_game_stats grew → not;
        // team_season_stats lost 3 rows (<5% and <25) → not.
        let current = vec![
            ("games", 700i64),
            ("player_game_stats", 41_000),
            ("team_season_stats", 357),
        ];
        let out = detect_count_regressions(&prior, &current);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("games:"), "{out:?}");
    }

    #[test]
    fn regressions_respect_absolute_floor() {
        // A 20-row drop on a 100-row table is 20% (over the rel floor) but under
        // the 25-row absolute floor → not flagged (early-season noise).
        let prior = vec![("team_season_stats".to_string(), 100i64)];
        let current = vec![("team_season_stats", 80i64)];
        assert!(detect_count_regressions(&prior, &current).is_empty());
    }

    #[test]
    fn regressions_skip_tables_absent_from_prior() {
        let prior: Vec<(String, i64)> = vec![];
        let current = vec![("games", 10i64)];
        assert!(detect_count_regressions(&prior, &current).is_empty());
    }
}
