use crate::NatStatClient;
use crate::client::NatStatError;
use crate::notify;
use crate::run_ledger::{
    RunLedger, StepStatus, detect_count_regressions, first_uncovered_ingest_date, heal_window,
};
use crate::team_id_by_code_and_season;
use crate::torvik::TorkvikClient;
use chrono::{Datelike, NaiveDate, Timelike, Utc};
use cstat_core::compute::{ComputeReport, compute_all};
use cstat_core::invariants::{self, Severity};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// True if `date` (a `YYYY-MM-DD` string) falls in the **dense** part of the
/// men's-college-basketball season, when essentially every night has D1 games —
/// used to decide whether a zero-row box-score ingest is an anomaly (in-season)
/// or expected (off-season).
///
/// Conservatively trims the nights that reliably have NO D1 games so a legit
/// gameless night isn't mistaken for a broken feed: the pre-tip ramp (Nov 1–5),
/// the **Dec 24–25 holiday break**, and the post-title tail (after ~Apr 7).
/// Mid-window off-nights still exist and are *not* excluded, so this is a coarse
/// false-positive guard, not a schedule — a schedule-aware check (cross-ref the
/// NatStat schedule for the window) is the real fix, tracked as a follow-up.
/// An unparseable date is treated as *out* of season so a bad date string can't
/// spuriously degrade a run.
fn is_core_season_date(date: &str) -> bool {
    let Ok(d) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return false;
    };
    match d.month() {
        11 => d.day() >= 6,
        12 => !matches!(d.day(), 24 | 25),
        1..=3 => true,
        4 => d.day() <= 7,
        _ => false,
    }
}

/// Orchestrates full-season data ingestion.
pub struct SeasonIngester<'a> {
    client: &'a NatStatClient,
    pool: &'a PgPool,
    season: i32,
}

/// Knobs for `bootstrap_season` — by default it runs every step.
#[derive(Debug, Clone, Copy)]
pub struct BootstrapOptions {
    /// When true, fetch Barttorvik player season stats after the NatStat steps.
    pub torvik: bool,
    /// When true, run the cstat-core compute pipeline at the end so derived
    /// stats (four factors, percentiles, archetypes-input columns, …) are
    /// fresh.
    pub compute: bool,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            torvik: true,
            compute: true,
        }
    }
}

impl<'a> SeasonIngester<'a> {
    pub fn new(client: &'a NatStatClient, pool: &'a PgPool, season: i32) -> Self {
        Self {
            client,
            pool,
            season,
        }
    }

    /// Run the NatStat-only portion of season ingestion in dependency order:
    /// 1. Teams (reference data, needed for foreign keys)
    /// 2. Games (results, needs team IDs)
    /// 3. Player performances (box scores — also auto-creates player records)
    /// 4. Team details (TCR, record, conference)
    /// 5. Team performances (team-level box scores for four factors)
    /// 6. ELO ratings (real ratings from /elo endpoint)
    /// 7. Game forecasts (per-game ELO snapshots, win exp, betting lines from /forecasts)
    pub async fn ingest_full_season(&self) -> Result<IngestReport, NatStatError> {
        let mut report = IngestReport::default();

        info!(season = self.season, "starting full season ingestion");

        info!("step 1/7: ingesting teams");
        report.teams = super::teams::ingest_teams(self.client, self.pool, self.season).await?;

        info!("step 2/7: ingesting games");
        report.games = super::games::ingest_games(self.client, self.pool, self.season).await?;

        info!("step 3/7: ingesting player performances");
        report.player_performances =
            super::games::ingest_player_performances(self.client, self.pool, self.season).await?;

        info!("step 4/7: ingesting team details");
        report.team_details =
            super::teams::ingest_team_details(self.client, self.pool, self.season).await?;

        info!("step 5/7: ingesting team performances");
        report.team_performances =
            super::games::ingest_all_team_performances(self.client, self.pool, self.season).await?;

        info!("step 6/7: ingesting ELO ratings");
        report.elo_ratings =
            super::elo::ingest_elo_ratings(self.client, self.pool, self.season).await?;

        info!("step 7/7: ingesting game forecasts");
        report.game_forecasts =
            super::elo::ingest_game_forecasts(self.client, self.pool, self.season).await?;

        info!(
            season = self.season,
            teams = report.teams,
            games = report.games,
            player_performances = report.player_performances,
            team_details = report.team_details,
            team_performances = report.team_performances,
            elo_ratings = report.elo_ratings,
            game_forecasts = report.game_forecasts,
            "season ingestion complete"
        );

        Ok(report)
    }

    /// Bootstrap a brand-new season end-to-end: NatStat ingest, Barttorvik
    /// ingest, and the compute pipeline. This is the single command for
    /// "add a new season" — its output, plus running the archetype trainer,
    /// is everything needed before the new year shows up in the UI.
    pub async fn bootstrap_season(
        &self,
        opts: BootstrapOptions,
    ) -> Result<BootstrapReport, NatStatError> {
        let ingest = self.ingest_full_season().await?;

        let torvik = if opts.torvik {
            let torvik_client = TorkvikClient::new();
            // Torvik failures shouldn't kill a season bootstrap — NatStat
            // data is the load-bearing part and Torvik can be re-run later
            // with `cstat-ingest torvik --year YYYY`. Log and continue.
            match super::torvik::ingest_torvik_player_stats(&torvik_client, self.pool, self.season)
                .await
            {
                Ok((upserted, matched)) => Some(TorvikReport { upserted, matched }),
                Err(e) => {
                    warn!(season = self.season, error = %e, "Torvik ingest failed; continuing");
                    None
                }
            }
        } else {
            None
        };

        let compute = if opts.compute {
            Some(
                compute_all(self.pool, self.season)
                    .await
                    .map_err(NatStatError::Database)?,
            )
        } else {
            None
        };

        Ok(BootstrapReport {
            ingest,
            torvik,
            compute,
        })
    }

    /// Incremental update: refresh recent games and performances. Optionally
    /// re-runs the compute pipeline so derived stats stay in sync (default
    /// for the CLI; opt-out lets a caller batch several updates first).
    pub async fn ingest_recent(
        &self,
        start_date: &str,
        end_date: &str,
        run_compute: bool,
    ) -> Result<UpdateReport, NatStatError> {
        let mut ingest = IngestReport::default();

        info!(
            season = self.season,
            start_date, end_date, "starting incremental ingestion"
        );

        ingest.games = super::games::ingest_games_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        ingest.player_performances = super::games::ingest_player_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        // Team-level box scores must be ingested too — `team_game_stats` feeds
        // four-factors / AdjEM / W-L derivation. Omitting this is why games
        // brought in by the incremental path historically carried player box
        // scores but no team box scores (issue #148).
        ingest.team_performances = super::games::ingest_team_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        info!(
            season = self.season,
            games = ingest.games,
            player_performances = ingest.player_performances,
            team_performances = ingest.team_performances,
            "incremental ingestion complete"
        );

        let compute = if run_compute {
            Some(
                compute_all(self.pool, self.season)
                    .await
                    .map_err(NatStatError::Database)?,
            )
        } else {
            None
        };

        Ok(UpdateReport { ingest, compute })
    }

    /// In-season **nightly** orchestration — the production "keep the site
    /// current" path. Refreshes the full *served-critical* input set in
    /// dependency order, recomputes derived stats, and records every step to
    /// the `ingest_runs` ledger.
    ///
    /// The load-bearing difference from [`ingest_recent`](Self::ingest_recent):
    /// it refreshes **game forecasts and Torvik (with per-game persistence)
    /// BEFORE `compute_all`**. `compute_campom` rebuilds `cam_gbpm_v3` from
    /// `torvik_player_stats`, and the served point-in-time model reads
    /// `torvik_player_game_stats`; without the Torvik refresh a nightly
    /// recompute would rebuild CamPom from stale Torvik and the in-season
    /// predictions would silently rot even though NatStat games ingest fine.
    ///
    /// Step isolation: the NatStat box-score steps and the final compute are
    /// load-bearing — a failure there aborts the run (returns `Err`). Forecasts,
    /// ELO ratings, and Torvik are best-effort — a failure is logged, recorded
    /// as `failed` in the ledger, and the run continues (yesterday's value beats
    /// none).
    ///
    /// Steps: games → player perfs → team perfs → forecasts → ELO ratings →
    /// Torvik (season + per-game) → compute. Two `ingest_full_season` steps are
    /// deliberately **omitted** because nothing they uniquely feed changes
    /// mid-season: `teams` (the reference team list — new teams only appear at a
    /// season bootstrap) and `team_details` (TCR is unserved, W-L is overwritten
    /// by `compute_derived_game_fields`, and conference is static in-season).
    pub async fn nightly(
        &self,
        start_date: &str,
        end_date: &str,
        run_compute: bool,
        self_heal: bool,
    ) -> Result<NightlyReport, NatStatError> {
        // Cap on how far back the self-heal will widen a defaulted window, so a
        // long off-season silence can't trigger a months-wide NatStat pull. A
        // genuine multi-night in-season outage is comfortably inside this.
        const MAX_HEAL_DAYS: i64 = 14;
        // How far back the gap scan looks. Deliberately wider than
        // MAX_HEAL_DAYS: the band between the two is what lets a run *report* a
        // gap it cannot itself recover (see `HealPlan::unrecovered_days`) — if
        // the scan stopped at the cap, an over-cap outage would silently look
        // fully healed. Past this, an un-backfilled hole ages out instead of
        // degrading every run forever.
        const HEAL_LOOKBACK_DAYS: i64 = 30;
        // The relation between the two is load-bearing, not incidental: at
        // lookback <= cap the scan could never surface a gap the cap can't
        // reach, `unrecovered_days` would always be 0, and the PARTIAL alert
        // would quietly stop existing — a gate that reports green over a real
        // hole, which is the exact failure this milestone is built to prevent.
        // Fail the build rather than let a future tweak inverting them ship.
        const _: () = assert!(HEAL_LOOKBACK_DAYS > MAX_HEAL_DAYS);

        let mut ledger = RunLedger::start(self.pool, self.season);

        // --- backfill-gap self-heal (M5b) ---
        // If the cron missed one or more nights, a plain yesterday..today window
        // would leave those game dates permanently un-ingested. When enabled
        // (the CLI passes this only for a DEFAULT window, never an operator's
        // explicit --from), widen the start back to the earliest date we have
        // not fully ingested so the gap heals on the next run with no manual
        // intervention. The re-covered dates are a harmless idempotent overlap.
        // Off for `simulate` (it drives its own clock) and for an explicit
        // operator window. A parse failure or an absent/covered scan no-ops.
        let mut heal_note: Option<String> = None;
        // Set when the MAX_HEAL_DAYS cap left game dates un-ingested — pushed to
        // `failures` (declared below) so a partial heal degrades the run.
        let mut heal_shortfall: Option<String> = None;
        // Set when the run fired before games settle — likewise pushed to
        // `failures` below (see the settle-hour check after the heal block).
        let mut early_run_note: Option<String> = None;
        let (start_date, end_date): (String, String) = {
            let widened = if self_heal {
                match (
                    NaiveDate::parse_from_str(start_date, "%Y-%m-%d"),
                    NaiveDate::parse_from_str(end_date, "%Y-%m-%d"),
                ) {
                    (Ok(df), Ok(dt)) => {
                        let gap = first_uncovered_ingest_date(
                            self.pool,
                            ledger.run_id(),
                            df,
                            dt,
                            HEAL_LOOKBACK_DAYS,
                        )
                        .await;
                        heal_window(df, dt, gap, MAX_HEAL_DAYS).map(|h| (h, gap))
                    }
                    _ => None,
                }
            } else {
                None
            };
            match widened {
                Some((plan, gap)) => {
                    let healed_str = plan.from.format("%Y-%m-%d").to_string();
                    // `gap` is the first date we have NOT fully ingested — not
                    // the date a run last executed. Say so: an operator reading
                    // this while debugging must not confuse the two.
                    let gap_str = gap
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    warn!(
                        season = self.season,
                        original_from = start_date,
                        healed_from = %healed_str,
                        gap_start = %gap_str,
                        unrecovered_days = plan.unrecovered_days,
                        "self-heal: widening nightly window to recover skipped night(s)"
                    );
                    heal_note = Some(format!(
                        "self-heal widened window start {start_date} → {healed_str} \
                         (first un-ingested date {gap_str})"
                    ));
                    // The cap bit: dates between the gap start and the healed
                    // start are NOT re-ingested by this run, and once they fall
                    // outside HEAL_LOOKBACK_DAYS nothing will pick them up. A
                    // silently-permanent hole in the served box scores is worse
                    // than a noisy alert — degrade the run and hand the operator
                    // the exact backfill command.
                    if plan.unrecovered_days > 0 {
                        warn!(
                            season = self.season,
                            unrecovered_days = plan.unrecovered_days,
                            "self-heal capped — earlier skipped dates need a manual backfill"
                        );
                        heal_shortfall = Some(format!(
                            "self-heal only PARTIAL: capped at {MAX_HEAL_DAYS}d, so \
                             {days} day(s) before {healed_str} were not re-ingested \
                             (gap opens {gap_str}) — backfill manually with \
                             `cstat-ingest nightly --year {season} --from {gap_str} --to {healed_str}`",
                            days = plan.unrecovered_days,
                            season = self.season,
                        ));
                    }
                    (healed_str, end_date.to_string())
                }
                None => (start_date.to_string(), end_date.to_string()),
            }
        };

        // Stamp the settled window (post-heal) onto every step row this run
        // writes — this is what lets the next run's `first_uncovered_ingest_date`
        // scan real coverage instead of guessing from wall-clock finish times.
        // Must happen before the first `ledger.record` below. Recording it for
        // operator windows too is the point: a manual backfill contributes the
        // dates it actually covered, so a range that misses the gap no longer
        // hides it.
        //
        // Claim only dates whose games had actually FINISHED when we fetched
        // them. The default window runs yesterday..today, but the cron fires
        // 09:30 UTC and date D's games don't tip until ~D 23:00 UTC — so a run
        // on D ingests exactly none of D's games. Stamping `window_end = D`
        // would claim D covered, and the next scan would skip it forever: every
        // outage would silently drop exactly one date (the last good run's own
        // day), with no alert, while the pre-tip `games` rows sat there scoreless
        // and statline-less. Clamping to yesterday costs nothing on the happy
        // path — the run on D+1 claims D, which is precisely when D's games were
        // ingested.
        //
        // A window with nothing complete in it (e.g. an operator's `--from today
        // --to today`) claims no coverage at all rather than an inverted range.
        let stamped: Option<(NaiveDate, NaiveDate)> = match (
            NaiveDate::parse_from_str(&start_date, "%Y-%m-%d"),
            NaiveDate::parse_from_str(&end_date, "%Y-%m-%d"),
        ) {
            (Ok(ws), Ok(we)) => {
                let ce = we.min(crate::today_utc() - chrono::Duration::days(1));
                (ws <= ce).then_some((ws, ce))
            }
            _ => None,
        };
        if let Some((ws, ce)) = stamped {
            ledger.set_window(ws, ce);
        }

        // The clamp above is only sound because the cron fires *after* last
        // night's games finish (09:30 UTC vs a ~08:00 settle). Nothing in the
        // code enforces that — the schedule lives in `railway.cron.json`, one
        // character away from `"0 2 * * *"`. At 02:00 UTC the run would fetch
        // last night's games mid-flight, record partial box scores, and still
        // claim their date covered, so no later run would ever re-fetch them:
        // the exact silent loss the clamp exists to prevent, reintroduced by a
        // config edit. Deriving the clamp from the instant instead does NOT
        // rescue this — an early run then claims nothing at all, leaving the
        // scan with no complete runs and killing the self-heal just as quietly.
        // An early cron is a broken pipeline either way, so say so out loud.
        //
        // Gate on what we actually CLAIMED, not on the requested range. Only the
        // newest claimable date — yesterday — is ever contentious; everything
        // older settled long ago. Keying off the requested `end_date` instead
        // would miss `--from X --to yesterday` (the shape the runbook's own
        // backfill instructions hand you) run before 08:00, which claims
        // yesterday while its games are still in flight — the very hole this
        // guard exists to close. It would also false-fire on `--from today --to
        // today`, which claims nothing at all. Skipped under a simulated clock,
        // which injects a date but no time-of-day for this to read.
        if crate::simulated_today().is_none()
            && let Some((_, ce)) = stamped
        {
            let now = Utc::now();
            if ce == now.date_naive() - chrono::Duration::days(1)
                && now.hour() < crate::GAMES_SETTLE_HOUR_UTC
            {
                warn!(
                    hour_utc = now.hour(),
                    settle_hour_utc = crate::GAMES_SETTLE_HOUR_UTC,
                    "nightly fired before games settle — box scores may be mid-flight"
                );
                // Deliberately doesn't assume the cron ran this: the same check
                // fires for a hand-run live window at 03:00, where "fix the cron
                // schedule" would be nonsense advice. Name both remedies and let
                // the reader pick.
                early_run_note = Some(format!(
                    "ran at {hour:02}:xx UTC, before the ~{settle:02}:00 UTC settle time — \
                     last night's games may still have been in progress, so this run's box \
                     scores can be partial and its coverage claim too optimistic. If this \
                     was the cron, move its schedule later (`railway.cron.json`; production \
                     is 09:30 UTC); if it was manual, re-run it after {settle:02}:00 UTC.",
                    hour = now.hour(),
                    settle = crate::GAMES_SETTLE_HOUR_UTC,
                ));
            }
        }

        let mut report = NightlyReport {
            ingest: IngestReport::default(),
            torvik: None,
            torvik_games_persisted: 0,
            torvik_rebounds_updated: 0,
            compute: None,
            run_id: ledger.run_id(),
        };

        // Best-effort step failures accumulate here; a non-empty list at the end
        // of the run fires a single summary Slack alert. Hard-fail steps alert
        // immediately (with this context) before aborting.
        let mut failures: Vec<String> = Vec::new();

        // A self-heal that hit the MAX_HEAL_DAYS cap recovered only part of the
        // gap — surface it as a degraded run (see the heal block above).
        if let Some(shortfall) = heal_shortfall {
            failures.push(shortfall);
        }

        // The run fired before last night's games had settled (see above).
        if let Some(note) = early_run_note {
            failures.push(note);
        }

        // A leftover CSTAT_SIMULATED_DATE pins the default window to one past
        // date forever while every monitor stays green (fresh ledger rows,
        // green heartbeat, happy /api/health/ingest) — the site would go
        // silently stale. Mark the run degraded so the Slack summary surfaces
        // it. `env_simulated_date` applies the same parse as `today_utc`, so
        // this only fires when the clock is actually pinned — an empty or
        // unparsable value (which the clock ignores) can't false-alarm every
        // night. The simulate harness advances the clock programmatically
        // (`set_simulated_today`), not via env, so replay windows stay clean.
        if let Some(sim_date) = crate::env_simulated_date() {
            warn!(
                %sim_date,
                "nightly running with CSTAT_SIMULATED_DATE set — window defaults are simulated"
            );
            failures.push(format!(
                "clock override active: CSTAT_SIMULATED_DATE={sim_date} — the nightly window \
                 is pinned to a simulated date; unset this on the cron service"
            ));
        }

        // Rate-budget headroom (2.5): snapshot tokens before the run so we can
        // log consumption and warn if a busy night eats most of the budget.
        let budget = crate::rate_budget_from_env();
        let tokens_before = self.client.rate_limit_remaining().await;

        info!(
            season = self.season,
            start_date = start_date.as_str(),
            end_date = end_date.as_str(),
            run_id = %ledger.run_id(),
            rate_budget = budget,
            rate_tokens_available = tokens_before,
            "starting nightly ingestion"
        );

        // --- 0. preflight connectivity check (M3 1.2) ---
        // Probe the serving-critical feeds up front so a dead dependency is
        // diagnosed here rather than surfacing as an opaque mid-run failure. 247
        // is skipped (`include_tfs = false`): it's offseason roster-construction,
        // never in the nightly chain, so probing it nightly would just burn a 247
        // call on an already-expired in-season token. Does NOT gate control flow
        // — the per-step isolation below already fail-softs best-effort feeds and
        // hard-fails the serving-critical chain — but a down serving-critical feed
        // is recorded as a failed ledger step and added to the degraded summary.
        let t0 = Utc::now();
        let preflight = crate::preflight::run(self.client, self.pool, self.season, false).await;
        preflight.log();
        if preflight.critical_down() {
            let down = preflight.down_feeds().join(", ");
            ledger
                .record(
                    "preflight",
                    StepStatus::Failed,
                    None,
                    t0,
                    Some(&format!("serving-critical feed(s) down: {down}")),
                )
                .await;
            failures.push(format!("preflight: serving-critical feed(s) down: {down}"));
        } else {
            ledger
                .record("preflight", StepStatus::Ok, None, t0, None)
                .await;
        }

        // --- 1. games (load-bearing) ---
        let t0 = Utc::now();
        match super::games::ingest_games_by_date_range(
            self.client,
            self.pool,
            self.season,
            &start_date,
            &end_date,
        )
        .await
        {
            Ok(n) => {
                report.ingest.games = n;
                ledger
                    .record("games", StepStatus::Ok, Some(n as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                ledger
                    .record("games", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                notify::post_slack(notify::SlackChannel::Cron, &format!(
                    ":rotating_light: cstat nightly ABORTED (season {}, run {}) — step `games` failed: {msg}",
                    self.season,
                    ledger.run_id()
                ))
                .await;
                // Signal the dead-man's-switch immediately on a hard abort so the
                // monitor pages now rather than after its full grace period.
                notify::ping_heartbeat(false).await;
                return Err(e);
            }
        }

        // --- 2. player performances (load-bearing) ---
        let t0 = Utc::now();
        match super::games::ingest_player_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            &start_date,
            &end_date,
        )
        .await
        {
            Ok(n) => {
                report.ingest.player_performances = n;
                ledger
                    .record("player_perfs", StepStatus::Ok, Some(n as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                ledger
                    .record("player_perfs", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                notify::post_slack(notify::SlackChannel::Cron, &format!(
                    ":rotating_light: cstat nightly ABORTED (season {}, run {}) — step `player_perfs` failed: {msg}",
                    self.season,
                    ledger.run_id()
                ))
                .await;
                notify::ping_heartbeat(false).await;
                return Err(e);
            }
        }

        // --- 3. team performances (load-bearing — feeds four factors / AdjEM / W-L) ---
        let t0 = Utc::now();
        match super::games::ingest_team_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            &start_date,
            &end_date,
        )
        .await
        {
            Ok(n) => {
                report.ingest.team_performances = n;
                ledger
                    .record("team_perfs", StepStatus::Ok, Some(n as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                ledger
                    .record("team_perfs", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                notify::post_slack(notify::SlackChannel::Cron, &format!(
                    ":rotating_light: cstat nightly ABORTED (season {}, run {}) — step `team_perfs` failed: {msg}",
                    self.season,
                    ledger.run_id()
                ))
                .await;
                notify::ping_heartbeat(false).await;
                return Err(e);
            }
        }

        // --- 4. game forecasts (best-effort — refreshes `game_forecasts`:
        // per-game pre/post ELO, win expectancy, betting lines). May be empty
        // off-season / before NatStat's nightly run, so it must not fail the run. ---
        let t0 = Utc::now();
        match super::elo::ingest_game_forecasts(self.client, self.pool, self.season).await {
            Ok(n) => {
                report.ingest.game_forecasts = n;
                ledger
                    .record("forecasts", StepStatus::Ok, Some(n as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "forecasts refresh failed; continuing");
                ledger
                    .record("forecasts", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("forecasts: {msg}"));
            }
        }

        // --- 5. ELO ratings (best-effort — the `/elo` endpoint is the SOLE
        // writer of `team_season_stats.elo_rating`/`elo_rank`, which feeds the
        // served `diff_elo_rating` model feature; `compute_all` never touches
        // those columns, so without this step the ELO feature would go stale
        // all season. Distinct from forecasts above, which writes `game_forecasts`.
        // May be empty before NatStat's nightly ELO run — fail-soft. ---
        let t0 = Utc::now();
        match super::elo::ingest_elo_ratings(self.client, self.pool, self.season).await {
            Ok(n) => {
                report.ingest.elo_ratings = n;
                ledger
                    .record("elo", StepStatus::Ok, Some(n as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "ELO ratings refresh failed; continuing");
                ledger
                    .record("elo", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("elo: {msg}"));
            }
        }

        // --- 6. Torvik player season stats (best-effort — feeds cam_gbpm_v3) ---
        let torvik_client = TorkvikClient::new();
        let t0 = Utc::now();
        match super::torvik::ingest_torvik_player_stats(&torvik_client, self.pool, self.season)
            .await
        {
            Ok((upserted, matched)) => {
                report.torvik = Some(TorvikReport { upserted, matched });
                ledger
                    .record("torvik", StepStatus::Ok, Some(upserted as i64), t0, None)
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "Torvik season-stats refresh failed; served CamPom may be stale");
                ledger
                    .record("torvik", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("torvik: {msg}"));
            }
        }

        // --- 7. Torvik per-game persistence + rebound backfill (best-effort) ---
        // One gzip fetch (`{year}_all_advgames.json.gz`) feeds both. This keeps
        // `torvik_player_game_stats` — the pit_cam_v3 serving input — fresh.
        let t0 = Utc::now();
        match torvik_client.fetch_game_stats(self.season).await {
            Ok(games) => {
                let persisted = match super::torvik::apply_persist_torvik_game_stats(
                    self.pool,
                    &games,
                    self.season,
                )
                .await
                {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(season = self.season, error = %e, "Torvik per-game persist failed");
                        0
                    }
                };
                let rebounds = match super::torvik::apply_rebound_backfill(
                    self.pool,
                    &games,
                    self.season,
                )
                .await
                {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(season = self.season, error = %e, "Torvik rebound backfill failed");
                        0
                    }
                };
                report.torvik_games_persisted = persisted;
                report.torvik_rebounds_updated = rebounds;
                ledger
                    .record(
                        "torvik_games",
                        StepStatus::Ok,
                        Some(persisted as i64),
                        t0,
                        None,
                    )
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "Torvik game-stats fetch failed; pit CamPom source not refreshed");
                ledger
                    .record("torvik_games", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("torvik_games: {msg}"));
            }
        }

        // --- 8. compute (load-bearing — recomputes every derived metric) ---
        if run_compute {
            let t0 = Utc::now();
            match compute_all(self.pool, self.season).await {
                Ok(c) => {
                    ledger
                        .record("compute", StepStatus::Ok, None, t0, None)
                        .await;
                    report.compute = Some(c);
                }
                Err(e) => {
                    let msg = e.to_string();
                    ledger
                        .record("compute", StepStatus::Failed, None, t0, Some(&msg))
                        .await;
                    notify::post_slack(notify::SlackChannel::Cron, &format!(
                        ":rotating_light: cstat nightly ABORTED (season {}, run {}) — step `compute` failed: {msg}",
                        self.season,
                        ledger.run_id()
                    ))
                    .await;
                    notify::ping_heartbeat(false).await;
                    return Err(NatStatError::Database(e));
                }
            }
        }

        // --- 9. post-compute invariant gates (M5 quality gates) ---
        // Structural "did compute do its job" checks against the just-written
        // derived tables (`cstat_core::invariants` — the same set the `simulate`
        // harness runs per window). `Error`-severity violations mean the pipeline
        // produced something wrong from the data it had, so they go into
        // `failures` and fire the DEGRADED Slack summary; `Warning`s are source-
        // data holes the pipeline faithfully reflects (see `Severity` docs) and
        // only log. This never hard-aborts: the served-critical chain already
        // completed above, so a gate failure alerts rather than kills the run.
        // Gated on a compute having actually run (the checks assume fresh
        // derived tables).
        if report.compute.is_some() {
            let t0 = Utc::now();
            match invariants::check_season(self.pool, self.season).await {
                Ok(violations) => {
                    let mut errors = 0i64;
                    for v in &violations {
                        match v.severity {
                            Severity::Error => {
                                errors += 1;
                                warn!(season = self.season, "INVARIANT VIOLATED — {v}");
                            }
                            Severity::Warning => {
                                info!(season = self.season, "invariant warning — {v}");
                            }
                        }
                    }
                    if errors > 0 {
                        let summary = violations
                            .iter()
                            .filter(|v| v.severity == Severity::Error)
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join("; ");
                        ledger
                            .record(
                                "invariants",
                                StepStatus::Failed,
                                Some(errors),
                                t0,
                                Some(&summary),
                            )
                            .await;
                        failures.push(format!(
                            "invariant gate: {errors} error-severity violation(s) — {summary}"
                        ));
                    } else {
                        ledger
                            .record("invariants", StepStatus::Ok, Some(0), t0, None)
                            .await;
                    }
                }
                Err(e) => {
                    // The gate query itself failed to run — surface it as degraded
                    // (a check that can't execute is worth an alert) without
                    // aborting the otherwise-complete run.
                    let msg = e.to_string();
                    warn!(season = self.season, error = %msg, "invariant gate query failed to run");
                    ledger
                        .record("invariants", StepStatus::Failed, None, t0, Some(&msg))
                        .await;
                    failures.push(format!("invariant gate failed to run: {msg}"));
                }
            }
        }

        // --- 10. row-count sanity vs the prior run (M5a) ---
        // Snapshot the season-scoped row count of every served table, persist it
        // under this run's id, and compare against the most recent prior run's
        // snapshot. In-season these tables only ever grow or hold flat, so a
        // material shrink (see `detect_count_regressions`) means a feed handed us
        // a truncated payload or compute wiped rows it shouldn't have — degraded,
        // not fatal (the served-critical chain already completed). Gated on a
        // compute having run, so the counts reflect freshly-written derived
        // tables. The first-ever snapshotting run has no prior and simply records
        // the baseline.
        if report.compute.is_some() {
            let t0 = Utc::now();
            let current = ledger.snapshot_counts().await;
            let prior = ledger.prior_run_table_counts().await;
            let regressions = detect_count_regressions(&prior, &current);
            if regressions.is_empty() {
                // Clean → these counts become the next run's baseline.
                ledger.persist_counts(&current).await;
                ledger
                    .record(
                        "row_counts",
                        StepStatus::Ok,
                        Some(current.len() as i64),
                        t0,
                        None,
                    )
                    .await;
            } else {
                // Deliberately do NOT persist a regressed snapshot: it would
                // become the baseline, the next run would compare corrupt to
                // corrupt, see no drop, and post the green SUCCESS summary over
                // a still-broken table. Keeping the last-known-good baseline
                // makes the gate re-fire every night until the counts recover.
                let summary = regressions.join("; ");
                warn!(
                    season = self.season,
                    "row-count regression vs prior run — {summary}"
                );
                ledger
                    .record(
                        "row_counts",
                        StepStatus::Failed,
                        Some(regressions.len() as i64),
                        t0,
                        Some(&summary),
                    )
                    .await;
                failures.push(format!("row-count regression vs prior run: {summary}"));
            }
        }

        // --- NatStat v4→v3 fallback visibility (M3 1.4) ---
        // The client silently downgrades to the v3 host on a persistent v4
        // timeout/5xx (it only logs a warning). A downgrade means v4 was failing
        // for this whole run — surface it as a ledger row + degraded line so a
        // prolonged v4 outage is visible instead of masked by the v3 serve.
        if self.client.used_v3_fallback() {
            let t0 = Utc::now();
            warn!(
                season = self.season,
                "NatStat v4 host fell back to v3 during this run — v4 may be down"
            );
            ledger
                .record(
                    "natstat_v4",
                    StepStatus::Failed,
                    None,
                    t0,
                    Some("v4 host persistently failed — fell back to v3 for the remainder of the run"),
                )
                .await;
            failures.push("natstat v4→v3 fallback (v4 host was failing)".to_string());
        }

        // --- Rate-budget headroom (2.5) ---
        // The token bucket refills mid-run (~budget/3600 per sec), so `consumed`
        // (before − after) UNDER-reports actual calls on a long run — refill
        // masks them. The operationally meaningful number is the *remaining*
        // headroom: if the bucket is draining faster than it refills it trends
        // toward 0 and the next calls block on the limiter. So we warn on either
        // signal — a big visible drawdown OR low absolute headroom — and log
        // both, with `remaining` as the one to watch.
        let tokens_after = self.client.rate_limit_remaining().await;
        let consumed = tokens_before.saturating_sub(tokens_after);
        let pct_used = if budget > 0 {
            (consumed as f64 / budget as f64) * 100.0
        } else {
            0.0
        };
        let pct_remaining = if budget > 0 {
            (tokens_after as f64 / budget as f64) * 100.0
        } else {
            100.0
        };
        if pct_used >= 80.0 || pct_remaining <= 20.0 {
            warn!(
                season = self.season,
                consumed,
                remaining = tokens_after,
                budget,
                pct_used = format!("{pct_used:.0}%"),
                pct_remaining = format!("{pct_remaining:.0}%"),
                "nightly rate-budget headroom low"
            );
            failures.push(format!(
                "rate budget low: ~{consumed} consumed, {tokens_after}/{budget} remaining ({pct_remaining:.0}%)"
            ));
        } else {
            info!(
                season = self.season,
                consumed,
                remaining = tokens_after,
                budget,
                pct_used = format!("{pct_used:.0}%"),
                pct_remaining = format!("{pct_remaining:.0}%"),
                "nightly rate-budget headroom"
            );
        }

        // --- In-season empty-ingest sanity check ---
        // A run where the load-bearing `games`/`player_perfs`/`team_perfs` steps
        // all succeeded (no hard-fail abort above) but returned *zero* rows is
        // fine in the off-season — but during the core season it means the box-
        // score feed silently handed us an empty result on a night that had
        // games. That's the "OK run, actually broken" case the per-step failure
        // handling can't catch (an empty success is still a success), so we flag
        // it as degraded. Heuristic: gated to the core-season window on the run's
        // end date (see `is_core_season_date`) to avoid firing on legitimate
        // off-season quiet nights. A schedule-aware version (cross-check against
        // the NatStat schedule for the window) is a possible follow-up.
        let empty_box = report.ingest.games == 0
            && report.ingest.player_performances == 0
            && report.ingest.team_performances == 0;
        if empty_box && is_core_season_date(&end_date) {
            warn!(
                season = self.season,
                start_date = start_date.as_str(),
                end_date = end_date.as_str(),
                "in-season nightly ingested zero box scores — feed may be empty/broken"
            );
            failures.push(format!(
                "empty box-score ingest for {start_date}..{end_date} during the season \
                 (0 games / 0 player perfs / 0 team perfs) — feed may be silently empty"
            ));
        }

        // --- Run-completion notification (2.4) ---
        // Hard-fail steps already alerted-and-aborted above. A run that reaches
        // here either completed clean (success heartbeat) or completed with a
        // best-effort feed down (degraded warning). The success ping doubles as
        // a "the cron fired and finished" heartbeat.
        // A self-heal widened the window this run — surface it in whichever
        // summary posts (a healed run is still a SUCCESS: it recovered the gap).
        let heal_line = match &heal_note {
            Some(n) => format!("\n_:arrows_counterclockwise: {n}_"),
            None => String::new(),
        };
        if failures.is_empty() {
            let torvik_line = match &report.torvik {
                Some(t) => format!(
                    "*Torvik:*  {} season · {} per-game",
                    t.upserted, report.torvik_games_persisted
                ),
                None => "*Torvik:*  skipped".to_string(),
            };
            let compute_str = if report.compute.is_some() {
                "ok"
            } else {
                "skipped"
            };
            notify::post_slack(
                notify::SlackChannel::Cron,
                &format!(
                    ":white_check_mark: *Nightly ingest OK* — season {season}\n\
                     *Box scores:*  {games} games · {pp} player perfs · {tp} team perfs\n\
                     *Feeds:*  {elo} ELO · {fc} forecasts\n\
                     {torvik_line}\n\
                     *Compute:*  {compute_str}   ·   *Rate budget:*  {remaining}/{budget}{heal_line}\n\
                     _run {run_id}_",
                    season = self.season,
                    games = report.ingest.games,
                    pp = report.ingest.player_performances,
                    tp = report.ingest.team_performances,
                    elo = report.ingest.elo_ratings,
                    fc = report.ingest.game_forecasts,
                    torvik_line = torvik_line,
                    compute_str = compute_str,
                    remaining = tokens_after,
                    budget = budget,
                    heal_line = heal_line,
                    run_id = ledger.run_id(),
                ),
            )
            .await;
        } else {
            let issues = failures
                .iter()
                .map(|f| format!("•  {f}"))
                .collect::<Vec<_>>()
                .join("\n");
            notify::post_slack(
                notify::SlackChannel::Cron,
                &format!(
                    ":warning: *Nightly ingest DEGRADED* — season {season}\n\
                     Completed with {n} issue(s):\n\
                     {issues}{heal_line}\n\
                     _run {run_id}_",
                    season = self.season,
                    n = failures.len(),
                    issues = issues,
                    heal_line = heal_line,
                    run_id = ledger.run_id(),
                ),
            )
            .await;
        }

        // --- Edge cache coherence (2.7) ---
        // After a successful compute the served tables changed; purge the edge
        // so fresh rankings/predictions land immediately instead of waiting out
        // the 5-min TTL. No-op unless CF_* env is configured. Fail-soft.
        if report.compute.is_some() {
            notify::purge_edge_cache().await;
        }

        // --- Dead-man's-switch heartbeat ---
        // Reaching here means the served-critical chain (games/perfs/compute)
        // completed — the run did its core job — so this is a SUCCESS ping even
        // when `failures` is non-empty (a best-effort feed degraded, or the
        // advisory empty-box heuristic tripped; those are visible in
        // #cron-job-alerts and must not page the dead-man's-switch). The `/fail`
        // ping is reserved for the hard-abort paths above, which page the monitor
        // immediately instead of waiting out its grace period. So the external
        // monitor pages on exactly two things: a run that never pinged (never
        // ran), or a run that pinged `/fail` (aborted). No-op unless HEARTBEAT_URL
        // is set. Fail-soft.
        notify::ping_heartbeat(true).await;

        info!(
            season = self.season,
            run_id = %ledger.run_id(),
            degraded = !failures.is_empty(),
            "nightly ingestion complete"
        );

        Ok(report)
    }

    /// Ingest everything needed for a single team: roster (player metadata),
    /// team details (TCR/ELO/W-L), per-player box scores, and per-team box
    /// scores. Lives on `SeasonIngester` (rather than the bin) so the same
    /// orchestration is reachable from tests and from any other caller.
    pub async fn ingest_team(&self, team_code: &str) -> Result<TeamReport, NatStatError> {
        let code = team_code.to_uppercase();
        info!(season = self.season, code = %code, "ingesting full team data");

        let roster =
            super::players::ingest_team_roster(self.client, self.pool, self.season, &code).await?;

        let team_details = match team_id_by_code_and_season(self.pool, Some(&code), self.season)
            .await?
        {
            Some(team_id) => {
                super::teams::ingest_single_team_details(
                    self.client,
                    self.pool,
                    self.season,
                    &team_id,
                    &code,
                )
                .await?
            }
            None => {
                warn!(season = self.season, code = %code, "team not in DB; skipping team details");
                false
            }
        };

        let player_performances = super::games::ingest_player_performances_by_team(
            self.client,
            self.pool,
            self.season,
            &code,
        )
        .await?;

        let team_performances =
            super::games::ingest_team_performances(self.client, self.pool, self.season, &code)
                .await?;

        Ok(TeamReport {
            code,
            roster,
            team_details,
            player_performances,
            team_performances,
        })
    }
}

/// Summary of an ingestion run.
#[derive(Debug, Default)]
pub struct IngestReport {
    pub teams: u64,
    pub games: u64,
    pub player_performances: u64,
    pub team_details: u64,
    pub team_performances: u64,
    pub elo_ratings: u64,
    pub game_forecasts: u64,
}

impl std::fmt::Display for IngestReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ingested: {} teams, {} games, {} player perfs, {} team details, {} team perfs, {} ELO ratings, {} game forecasts",
            self.teams,
            self.games,
            self.player_performances,
            self.team_details,
            self.team_performances,
            self.elo_ratings,
            self.game_forecasts
        )
    }
}

#[derive(Debug, Default)]
pub struct TorvikReport {
    pub upserted: u64,
    pub matched: u64,
}

/// Aggregate report from `bootstrap_season`.
#[derive(Debug)]
pub struct BootstrapReport {
    pub ingest: IngestReport,
    pub torvik: Option<TorvikReport>,
    pub compute: Option<ComputeReport>,
}

impl std::fmt::Display for BootstrapReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.ingest)?;
        if let Some(t) = &self.torvik {
            writeln!(
                f,
                "Torvik: {} upserted, {} matched to cstat players",
                t.upserted, t.matched
            )?;
        }
        if let Some(c) = &self.compute {
            writeln!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Aggregate report for the `update` command — incremental ingest plus an
/// optional compute pass.
#[derive(Debug)]
pub struct UpdateReport {
    pub ingest: IngestReport,
    pub compute: Option<ComputeReport>,
}

impl std::fmt::Display for UpdateReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.ingest)?;
        if let Some(c) = &self.compute {
            writeln!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Aggregate report for the `nightly` command — the full served-critical
/// refresh (box scores + forecasts + Torvik) plus the compute pass.
#[derive(Debug)]
pub struct NightlyReport {
    pub ingest: IngestReport,
    /// `None` when the Torvik season-stats refresh failed (served CamPom may
    /// be stale until the next successful run).
    pub torvik: Option<TorvikReport>,
    pub torvik_games_persisted: u64,
    pub torvik_rebounds_updated: u64,
    pub compute: Option<ComputeReport>,
    /// Grouping id for this run's rows in the `ingest_runs` ledger.
    pub run_id: Uuid,
}

impl std::fmt::Display for NightlyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Nightly run {}", self.run_id)?;
        // Only the fields nightly actually refreshes — `teams`/`team_details`
        // are intentionally not part of this path (see `nightly` docs), so
        // listing them as "0" would misleadingly read as a no-op.
        writeln!(
            f,
            "Ingested: {} games, {} player perfs, {} team perfs, {} ELO ratings, {} game forecasts",
            self.ingest.games,
            self.ingest.player_performances,
            self.ingest.team_performances,
            self.ingest.elo_ratings,
            self.ingest.game_forecasts,
        )?;
        match &self.torvik {
            Some(t) => writeln!(
                f,
                "Torvik: {} upserted, {} matched; {} per-game rows, {} rebound rows",
                t.upserted, t.matched, self.torvik_games_persisted, self.torvik_rebounds_updated
            )?,
            None => writeln!(
                f,
                "Torvik: refresh FAILED — served CamPom may be stale (see ingest_runs)"
            )?,
        }
        if let Some(c) = &self.compute {
            writeln!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Per-team report from `ingest_team`.
#[derive(Debug)]
pub struct TeamReport {
    pub code: String,
    pub roster: u64,
    pub team_details: bool,
    pub player_performances: u64,
    pub team_performances: u64,
}

impl std::fmt::Display for TeamReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{}: {} roster, team details {}, {} player perfs, {} team perfs",
            self.code,
            self.roster,
            if self.team_details { "OK" } else { "skipped" },
            self.player_performances,
            self.team_performances,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::is_core_season_date;

    #[test]
    fn core_season_window_covers_nov_through_early_april() {
        // Dense nights: essentially every one has D1 games.
        assert!(is_core_season_date("2025-11-10")); // season underway
        assert!(is_core_season_date("2025-12-20"));
        assert!(is_core_season_date("2025-12-26")); // games resume after the break
        assert!(is_core_season_date("2026-01-15"));
        assert!(is_core_season_date("2026-02-28"));
        assert!(is_core_season_date("2026-03-30")); // tournament
        assert!(is_core_season_date("2026-04-06")); // title-game week
    }

    #[test]
    fn predictable_gameless_nights_are_excluded() {
        // The false-positive cases the heuristic must NOT flag as a broken feed:
        assert!(!is_core_season_date("2025-11-02")); // pre-tip ramp
        assert!(!is_core_season_date("2025-12-24")); // holiday break
        assert!(!is_core_season_date("2025-12-25")); // holiday break
        assert!(!is_core_season_date("2026-04-09")); // after the title game
    }

    #[test]
    fn off_season_is_excluded() {
        assert!(!is_core_season_date("2026-04-30"));
        assert!(!is_core_season_date("2026-05-01"));
        assert!(!is_core_season_date("2026-07-12")); // deep off-season
        assert!(!is_core_season_date("2025-10-31")); // before the tip window
    }

    #[test]
    fn unparseable_date_is_treated_as_off_season() {
        // A bad date string must not spuriously degrade a run.
        assert!(!is_core_season_date(""));
        assert!(!is_core_season_date("not-a-date"));
        assert!(!is_core_season_date("2026-13-40"));
    }
}
