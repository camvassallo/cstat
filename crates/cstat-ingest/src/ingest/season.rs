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

/// What the PBP self-heal should do about the dates currently missing PBP.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PbpHealPlan {
    /// Widen the PBP window back to this date. `None` = leave the window alone,
    /// either because nothing is missing or because everything missing is
    /// already inside it (or out of reach).
    from: Option<chrono::NaiveDate>,
    /// Missing dates this run will **not** fetch. Reported to an operator.
    ///
    /// Computed against the window the run actually ends up covering, not
    /// against the cap floor: when the box-score heal has already widened the
    /// window past the floor, the PBP step rides on it and covers dates the cap
    /// alone would call out of reach. Filtering on the floor instead produced a
    /// DEGRADED summary naming dates the same run had just healed.
    unreachable: Vec<chrono::NaiveDate>,
}

/// Decide the PBP heal from the dates that are *actually* missing play-by-play
/// (`deficient`, ascending), the oldest date the cap allows fetching (`floor`),
/// and the box-score window's start (`box_start`).
///
/// The rule is one line — widen to the earliest missing date we can reach — but
/// it is the piece that broke three times, so it lives here as a pure function
/// with its properties pinned by tests rather than inline in a 600-line
/// orchestrator. The two failures worth remembering:
///
/// - **Reaching back to the cap floor rather than to a missing date** re-fetched
///   a full cap-width window every night. Since the floor advances with the
///   calendar, that window slid forward and never reached an old gap: pure
///   re-work, nightly, converging on nothing.
/// - **Refusing to heal at all when the oldest missing date was out of reach**
///   then abandoned every *newer* missing date too, because the scan reports the
///   oldest first. One unreachable date suppressed recovery of holes that were
///   two days old until they aged out of reach as well.
///
/// So unreachable dates are split off and reported, and the heal starts at the
/// earliest reachable one. Dates at or after `box_start` need no widening — the
/// run already covers them.
fn plan_pbp_heal(
    deficient: &[chrono::NaiveDate],
    floor: chrono::NaiveDate,
    box_start: chrono::NaiveDate,
) -> PbpHealPlan {
    let from = deficient
        .iter()
        .find(|d| **d >= floor && **d < box_start)
        .copied();
    // Where the PBP step will actually start: the healed date if we widened,
    // otherwise the box window's own start (which a box heal may already have
    // pushed back further than the PBP cap would reach on its own).
    let covered_from = from.unwrap_or(box_start);
    PbpHealPlan {
        from,
        unreachable: deficient
            .iter()
            .filter(|d| **d < covered_from)
            .copied()
            .collect(),
    }
}

/// The slice of `[from, to]` a run may claim as ingested coverage, or `None`
/// when nothing in it had finished.
///
/// Clamped to yesterday because the cron fires 09:30 UTC while date D's games
/// don't tip until ~D 23:00 UTC — a run on D ingests none of D's games, so
/// stamping `window_end = D` would claim D covered and no later run would ever
/// re-fetch it. A window with nothing complete in it (an operator's `--from
/// today --to today`) claims nothing rather than an inverted range. An
/// unparseable date claims nothing.
///
/// `today` is passed in rather than read from `crate::today_utc()` so this stays
/// a pure function. That is not just tidiness: the clock lives in a process-wide
/// atomic that other unit tests in this same test binary write, so a version
/// reading it directly could only be tested against whatever clock happened to
/// be installed when the test ran.
fn claimable_window(from: &str, to: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    match (
        NaiveDate::parse_from_str(from, "%Y-%m-%d"),
        NaiveDate::parse_from_str(to, "%Y-%m-%d"),
    ) {
        (Ok(ws), Ok(we)) => {
            let ce = we.min(today - chrono::Duration::days(1));
            (ws <= ce).then_some((ws, ce))
        }
        _ => None,
    }
}

/// Classify the two `torvik_games` sub-step outcomes into the step's ledger
/// status plus any lines for the degraded run summary. Pure — unit-tested.
///
/// The asymmetry is the point, and it is not an oversight to be tidied away
/// later. `torvik_games` is in `SERVED_CRITICAL` (`routes/health.rs`) because it
/// writes `torvik_player_game_stats`, the `pit_cam_v3` serving input — so a
/// failed **persist** must fail the step, and the 503 that follows on
/// `/api/health/ingest` is correct. The **rebound backfill** is an enrichment
/// layered on top of that same fetch; collapsing both into "any error fails the
/// step" would take the health endpoint red over data that is not what makes the
/// step critical, and a red light nobody can act on is how a real outage gets
/// missed.
///
/// Both previously swallowed their error into a `0` and let the step record
/// `ok`, so a failed write posted a green summary and reset the 36h staleness
/// clock while the pit CamPom source silently aged.
fn classify_torvik_games_outcome(
    persist_err: Option<&str>,
    rebound_err: Option<&str>,
) -> (StepStatus, Vec<String>) {
    let mut lines = Vec::new();
    if let Some(e) = rebound_err {
        lines.push(format!("torvik_games: rebound backfill failed: {e}"));
    }
    let status = match persist_err {
        Some(e) => {
            lines.push(format!("torvik_games: per-game persist failed: {e}"));
            StepStatus::Failed
        }
        None => StepStatus::Ok,
    };
    (status, lines)
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
                compute_all(
                    self.pool,
                    self.season,
                    crate::should_infer_newcomers(self.season),
                )
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
                compute_all(
                    self.pool,
                    self.season,
                    crate::should_infer_newcomers(self.season),
                )
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
        // The PBP heal's own cap, deliberately tighter than MAX_HEAL_DAYS: a
        // re-pull of play-by-play is roughly two orders of magnitude more rows
        // (~530/game) than the box-score steps for the same dates, so a wide
        // window is a real draw on the hourly rate budget. A week covers the
        // realistic case — one or a few `playbyplay` nights lost while the
        // served-critical chain kept succeeding — and anything longer is a
        // deliberate operator backfill, not something to do automatically at
        // 09:30 with no one watching. Whatever the cap leaves behind keeps
        // showing up in `pbp_date_coverage_gap` until someone backfills it.
        const MAX_PBP_HEAL_DAYS: i64 = 7;

        // Wall-clock start, for the run duration in the summary. A nightly that
        // suddenly takes 3× as long is the shape of the prod-DB latency / N+1
        // regressions this pipeline has already hit twice, and it is invisible
        // in a summary that only reports counts.
        //
        // Taken FIRST, before the two self-heal coverage scans below. Those are
        // the newest and most query-heavy things in the run — the PBP scan
        // probes per game across a 30M-row table — so a timer started after them
        // would be blind to exactly the regression it exists to surface.
        let run_started = Utc::now();

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

        // --- play-by-play gap self-heal (issue #247) ---
        // A night where the box-score steps succeeded and `playbyplay` failed
        // leaves the date fully covered as far as `BOX_SCORE_STEPS` is
        // concerned, so the box-score heal above never revisits it — and because
        // `compute_pbp_lineups` is a season-scoped DELETE-then-rebuild, every
        // later run rebuilds the whole season's `lineup_aggregates` /
        // `player_on_off` / `lineup_stints` around that hole. Nothing was
        // self-correcting and nothing alerted.
        //
        // **This scans the DATA, not the ledger**, and that is the whole design.
        // The first three attempts derived PBP coverage from `ingest_runs`
        // window claims, and every one of them shipped a variant of the same
        // bug, because a window claim cannot express "some dates in this range
        // have PBP and others don't": a heal that fetched a range containing any
        // rows certified every date inside it, including the empty ones, forever.
        // Patching that produced further holes — a zero-row success still
        // claiming coverage, an over-cap gap blocking recovery of newer dates
        // that were trivially reachable, an operator backfill that silenced the
        // alert without filling anything. `pbp_deficient_dates` asks the
        // question directly against `games` and `play_by_play`, which is what
        // the alert has always done, so the two are now incapable of
        // disagreeing and a hole is self-clearing: fill the rows by any means
        // and both go quiet, with no bookkeeping to get right.
        //
        // Convergence, which the ledger version never had: the heal starts at
        // the earliest deficient date that is REACHABLE (inside the cap), so it
        // re-pulls only from a date that is actually missing rather than from a
        // sliding cap-width floor. Once those dates are filled they leave the
        // scan and the window narrows back on its own. Deficient dates older
        // than the cap don't block the reachable ones — they are recovered
        // anyway and the unreachable tail is reported separately, which is the
        // opposite of the previous behaviour, where one old gap suppressed the
        // heal entirely until every newer hole had also aged out.
        let mut pbp_heal_note: Option<String> = None;
        let mut pbp_heal_shortfall: Option<String> = None;
        // Whether the `playbyplay` step actually succeeded. `report.pbp_rows`
        // alone cannot answer that — it stays at its `0` default when the step
        // errors, so the in-season empty-PBP check below would read a plain
        // fetch failure as "the feed is silently empty" and post a second,
        // contradictory issue line next to the recorded error.
        let mut pbp_step_ok = false;
        let pbp_start_date: String = match (
            self_heal,
            NaiveDate::parse_from_str(&start_date, "%Y-%m-%d"),
            NaiveDate::parse_from_str(&end_date, "%Y-%m-%d"),
        ) {
            (true, Ok(df), Ok(dt)) => {
                let horizon = dt - chrono::Duration::days(HEAL_LOOKBACK_DAYS);
                let floor = dt - chrono::Duration::days(MAX_PBP_HEAL_DAYS);
                match cstat_core::invariants::pbp_deficient_dates(
                    self.pool,
                    self.season,
                    Some(horizon),
                )
                .await
                {
                    Ok(deficient) => {
                        // The heal chases only dates with a real slate; the
                        // ALERT still reports every deficient date, including
                        // one- and two-game ones.
                        //
                        // Those light-slate dates are the case the heal cannot
                        // converge on. A whole-night ingest failure always hits
                        // a full slate, so a 1–2 game date at zero coverage is
                        // far more likely a game the source never published (the
                        // only such date in twelve seasons, 2019-12-24, is
                        // exactly that) than a pipeline fault. Re-fetching it
                        // cannot fill it, so it stays deficient and gets picked
                        // again the next night, and the next — each run paging a
                        // multi-day range and DELETE/INSERT-replacing the
                        // play-by-play of every already-complete game in it,
                        // until the date drifts past the cap. Detection loses
                        // nothing: the date is still named in the warnings line
                        // every night, where a human can judge what the nightly
                        // cannot.
                        let dates: Vec<NaiveDate> = deficient
                            .iter()
                            .filter(|d| d.games >= cstat_core::invariants::PBP_DATE_MIN_GAMES)
                            .map(|d| d.date)
                            .collect();
                        let plan = plan_pbp_heal(&dates, floor, df);
                        if !plan.unreachable.is_empty() {
                            warn!(
                                season = self.season,
                                count = plan.unreachable.len(),
                                oldest = %plan.unreachable[0],
                                "play-by-play dates older than the heal cap — needs a manual \
                                 backfill; the nightly cannot reach them"
                            );
                            // A NOTE, not a `failures` entry — deliberately, and
                            // not a repeat of the round-one mistake that
                            // suppressed this alert on the reasoning that the
                            // invariant already named the dates. That reasoning
                            // was wrong then for two specific reasons, both of
                            // which have since been fixed: the warnings line
                            // carried no samples, and the heal was silently
                            // re-pulling a window nightly. Now the standing
                            // `pbp_date_coverage_gap` warning does name these
                            // dates on every summary, and the heal no longer
                            // touches them.
                            //
                            // What remains is a hole no nightly action can
                            // close — the source may simply never publish it.
                            // Degrading the run turns that into a DEGRADED post
                            // every night for the ~23 nights it takes to age out
                            // of the lookback, on the channel that also carries
                            // real feed outages. Visible on every summary,
                            // colour reserved for what a run can act on.
                            //
                            // The backfill range spans all of them, not just the
                            // oldest: one command, not one per date.
                            pbp_heal_shortfall = Some(format!(
                                "{n} game date(s) older than the {MAX_PBP_HEAL_DAYS}d heal cap \
                                 still have no play-by-play ({sample}) — the nightly cannot \
                                 reach these. Backfill with `cstat-ingest playbyplay \
                                 --year {season} --from {oldest} --to {newest}`; any run that \
                                 lands the rows clears it, since the check reads the rows",
                                n = plan.unreachable.len(),
                                sample = plan
                                    .unreachable
                                    .iter()
                                    .take(3)
                                    .map(|d| d.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                oldest = plan.unreachable[0],
                                newest = plan.unreachable[plan.unreachable.len() - 1],
                                season = self.season,
                            ));
                        }
                        match plan.from {
                            Some(target) => {
                                let healed_str = target.format("%Y-%m-%d").to_string();
                                warn!(
                                    season = self.season,
                                    box_from = %start_date,
                                    pbp_from = %healed_str,
                                    deficient_dates = dates.len(),
                                    "self-heal: widening play-by-play window to the earliest \
                                     reachable date missing PBP"
                                );
                                pbp_heal_note = Some(format!(
                                    "play-by-play self-heal widened its window start \
                                     {start_date} → {healed_str} ({n} game date(s) missing PBP)",
                                    n = dates.len(),
                                ));
                                healed_str
                            }
                            None => start_date.clone(),
                        }
                    }
                    Err(e) => {
                        // A scan that can't run must not take the run with it —
                        // PBP is best-effort. Fall back to the plain window.
                        warn!(season = self.season, error = %e, "play-by-play coverage scan failed; skipping the PBP self-heal this run");
                        start_date.clone()
                    }
                }
            }
            _ => start_date.clone(),
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
        let stamped = claimable_window(&start_date, &end_date, crate::today_utc());
        if let Some((ws, ce)) = stamped {
            ledger.set_window(ws, ce);
        }

        // The PBP step deliberately stamps NO window of its own. It used to, so
        // a coverage scan could read it back — but that scan now reads the
        // `play_by_play` rows directly (see the heal block above), and a second,
        // per-step notion of coverage would only be another thing to keep
        // consistent with the data. Its ledger row still records status, rows
        // and timing like every other step.

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
            pbp_rows: 0,
            lineups_fetched: 0,
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

        // Detect the public egress IP (fail-soft) so a barttorvik 403 can be tied
        // to the exact outbound IP Railway used this run. Carried into the
        // preflight ledger row and the degraded Slack alert below, because having
        // it only in the Railway logs is what made this a log-dig twice — see
        // `preflight::detect_egress_ip` and `docs/torvik_egress_block.md`.
        let egress_ip = crate::preflight::detect_egress_ip().await;

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
            // Stamp the egress IP onto the ledger row: for Torvik the refusal is
            // IP-scoped, so "which IP was this run" is the first question asked
            // of a failure, and `ingest_runs` is what survives log retention.
            let detail = match &egress_ip {
                Some(ip) => format!("serving-critical feed(s) down: {down} (egress IP {ip})"),
                None => format!("serving-critical feed(s) down: {down}"),
            };
            ledger
                .record("preflight", StepStatus::Failed, None, t0, Some(&detail))
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
                // Both sub-steps used to swallow their error into a `0` and let
                // the step record `ok`. That made a served-critical step report
                // green over a failed write: the gzip fetch succeeded, so
                // `torvik_games` logged `ok`, the SUCCESS summary posted, and
                // `/api/health/ingest` had its 36h staleness clock reset — while
                // `torvik_player_game_stats`, the `pit_cam_v3` serving input,
                // was never written. The only trace was a `warn!` in the Railway
                // logs. Outcomes are now split by what each actually costs.
                let persisted =
                    super::torvik::apply_persist_torvik_game_stats(self.pool, &games, self.season)
                        .await;
                let rebounds =
                    super::torvik::apply_rebound_backfill(self.pool, &games, self.season).await;

                if let Ok(n) = &rebounds {
                    report.torvik_rebounds_updated = *n;
                }
                if let Ok(n) = &persisted {
                    report.torvik_games_persisted = *n;
                }
                let persist_err = persisted.as_ref().err().map(|e| e.to_string());
                let rebound_err = rebounds.as_ref().err().map(|e| e.to_string());
                if let Some(e) = &rebound_err {
                    warn!(season = self.season, error = %e, "Torvik rebound backfill failed");
                }
                if let Some(e) = &persist_err {
                    warn!(season = self.season, error = %e, "Torvik per-game persist failed; pit CamPom source not refreshed");
                }

                let (status, lines) =
                    classify_torvik_games_outcome(persist_err.as_deref(), rebound_err.as_deref());
                // Every failure this step saw goes in the ledger row, not just
                // the one that decided the status. `None` on a clean run.
                //
                // Caveat worth knowing before relying on it: when only the
                // rebound backfill failed the status stays `ok`, and both
                // operator surfaces that read this table filter on status —
                // `--prod-status` lists recent *failures*, `/api/health/ingest`
                // reports per-step freshness — so the text is reachable only by
                // querying `ingest_runs` directly. The degraded Slack line below
                // is the surface that actually carries it to a human; this is
                // the durable copy for someone digging later.
                let detail = (!lines.is_empty()).then(|| lines.join("; "));
                ledger
                    .record(
                        "torvik_games",
                        status,
                        (status == StepStatus::Ok).then_some(report.torvik_games_persisted as i64),
                        t0,
                        detail.as_deref(),
                    )
                    .await;
                failures.extend(lines);
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

        // --- 7b. play-by-play (best-effort — feeds `compute_pbp_lineups`) ---
        // Date-scoped to the ingest window, so off-season (or any night with no
        // games in range) it short-circuits to a no-op with ZERO API calls
        // (`ingest_pbp_scoped` returns early on an empty game scope). Must run
        // BEFORE `compute_all`: its step-10 `compute_pbp_lineups` reads
        // `play_by_play` for the season and early-returns when prod holds none —
        // this is the step that makes prod produce `lineup_aggregates` /
        // `player_on_off` / `lineup_stints` itself instead of importing them from
        // a laptop. Best-effort: PBP feeds only display surfaces (duos/trios,
        // on-off, RAPM) and 3-of-60 trajectory features (which degrade to a
        // sentinel), so a fetch failure degrades the run rather than aborting the
        // served-critical chain. Uses the date-range path, never `gamecode`:
        // NatStat only honours the `range` filter on page 1, so a paginated
        // gamecode query silently runs away into the global season stream.
        //
        // Runs over `pbp_start_date`, not `start_date` — its own self-healed
        // window (issue #247). Equal to `start_date` on every healthy night;
        // wider only when a past `playbyplay` step failed on a night whose box
        // scores succeeded, which the box-score coverage scan cannot see.
        let t0 = Utc::now();
        match super::playbyplay::ingest_play_by_play_by_date_range(
            self.client,
            self.pool,
            self.season,
            &pbp_start_date,
            &end_date,
        )
        .await
        {
            Ok(pbp) => {
                report.pbp_rows = pbp.rows;
                pbp_step_ok = true;
                ledger
                    .record(
                        "playbyplay",
                        StepStatus::Ok,
                        Some(pbp.rows as i64),
                        t0,
                        None,
                    )
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "play-by-play refresh failed; PBP-derived surfaces (lineups/on-off/RAPM) may be stale");
                ledger
                    .record("playbyplay", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("playbyplay: {msg}"));
            }
        }

        // --- 7c. captured NatStat lineups object (best-effort) ---
        // Supplies EXACT 5-man membership where available; `compute_pbp_lineups`
        // prefers it over the weaker PBP-reconstructed stints, so a prod without
        // it would compute different, worse `lineup_aggregates`. Window-scoped
        // (same reason as PBP: keeps the sweep bounded to the night's games — a
        // full-season sweep would fire on the first prod run, whose
        // `natstat_lineup_games` ledger is empty). Fetch-limited as a runaway
        // backstop even within the window (e.g. a wide self-heal window).
        // Best-effort like PBP.
        //
        // Deliberately scoped to `start_date`, NOT the PBP heal's window.
        //
        // Riding the healed window looks right — the two feeds fail together,
        // and `compute_pbp_lineups` prefers this exact 5-man membership over the
        // PBP-reconstructed stints, so a healed date whose lineups were left
        // behind keeps the weaker source. That is a real gap. But this sweep is
        // oldest-first and truncates at the fetch limit below, so over a
        // week-wide window (300–600 uncaptured peak-season games) the cap is
        // spent on the oldest dates and never reaches the current night's — and
        // *those* games are unrecoverable, because tomorrow's window no longer
        // contains them and the PBP heal will not widen back to a date whose PBP
        // has since landed. Trading a degraded source on old dates for a missing
        // one on last night's games is the wrong trade, and the right fix needs
        // the sweep's ordering and cap reworked together.
        //
        // So the lineups hole stays open and stays known, as it was before this
        // issue. Tracked as the `lineups` follow-up to #247.
        const NIGHTLY_LINEUPS_FETCH_LIMIT: u64 = 500;
        let t0 = Utc::now();
        match super::lineups::ingest_lineups_for_season(
            self.client,
            self.pool,
            self.season,
            Some((&start_date, &end_date)),
            Some(NIGHTLY_LINEUPS_FETCH_LIMIT),
            false,
        )
        .await
        {
            Ok(l) => {
                report.lineups_fetched = l.games_fetched;
                ledger
                    .record(
                        "lineups",
                        StepStatus::Ok,
                        Some(l.games_fetched as i64),
                        t0,
                        None,
                    )
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(season = self.season, error = %msg, "lineups capture failed; exact 5-man membership may be stale");
                ledger
                    .record("lineups", StepStatus::Failed, None, t0, Some(&msg))
                    .await;
                failures.push(format!("lineups: {msg}"));
            }
        }

        // --- 8. compute (load-bearing — recomputes every derived metric) ---
        if run_compute {
            let t0 = Utc::now();
            match compute_all(
                self.pool,
                self.season,
                crate::should_infer_newcomers(self.season),
            )
            .await
            {
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
        // Warning-severity findings, rendered as one compact `check count` line
        // for the run summary. Until this existed they reached only the tracing
        // log, which meant a `Warning` check was invisible to anyone not tailing
        // Railway — and so a new one could not do the job it was added for.
        // Carries the samples, not just the count. A line reading
        // `pbp_date_coverage_gap 1` tells the operator a PBP hole exists but not
        // which date, so the backfill still needs a psql session — and the PBP
        // heal's shortfall message explicitly relies on this line naming the
        // dates. `InvariantViolation` already caps its samples, and they are
        // trimmed again here so a standing multi-violation check (e.g. #232's
        // 22) stays one readable line. Deliberately does NOT degrade the run — a
        // warning is a source-data hole the pipeline faithfully reflects.
        let mut invariant_warnings: Option<String> = None;
        if report.compute.is_some() {
            let t0 = Utc::now();
            match invariants::check_season(self.pool, self.season).await {
                Ok(violations) => {
                    let mut errors = 0i64;
                    let mut warnings: Vec<String> = Vec::new();
                    for v in &violations {
                        match v.severity {
                            Severity::Error => {
                                errors += 1;
                                warn!(season = self.season, "INVARIANT VIOLATED — {v}");
                            }
                            Severity::Warning => {
                                info!(season = self.season, "invariant warning — {v}");
                                // Up to 3 of the (already capped) samples, so
                                // the reader gets the actual dates/ids to act on
                                // without the line running away.
                                const SLACK_SAMPLES: usize = 3;
                                let shown: Vec<&str> = v
                                    .samples
                                    .iter()
                                    .take(SLACK_SAMPLES)
                                    .map(String::as_str)
                                    .collect();
                                let unshown = v.count - shown.len() as i64;
                                warnings.push(if shown.is_empty() {
                                    format!("{} {}", v.check, v.count)
                                } else if unshown > 0 {
                                    format!(
                                        "{} {} ({}, +{unshown})",
                                        v.check,
                                        v.count,
                                        shown.join(", ")
                                    )
                                } else {
                                    format!("{} {} ({})", v.check, v.count, shown.join(", "))
                                });
                            }
                        }
                    }
                    if !warnings.is_empty() {
                        invariant_warnings = Some(warnings.join(" · "));
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

        // --- In-season empty-PBP check (issue #247) ---
        // The total-loss case, and the one blind spot `pbp_date_coverage_gap`
        // structurally cannot cover: that invariant no-ops when the season holds
        // no PBP *at all* (it has nothing to measure a share against, and firing
        // on every date would drown the replay harnesses). But a season that
        // never receives any PBP is precisely the worst outcome — and it is
        // silent end to end. `ingest_pbp_scoped` returns `Ok` with zero rows
        // when a page yields no in-scope plays, so the step records `ok`, the
        // coverage scan sees the window covered and reports no gap, and
        // `pbp_present_but_lineups_empty` is itself gated on PBP being present.
        // The season would serve empty `lineup_aggregates` / `player_on_off` /
        // `lineup_stints` — blank duos/trios, on-off and RAPM, and the three
        // on/off trajectory features pinned to their sentinel — under a green
        // summary every night.
        //
        // Three gates, each closing a false-fire path:
        //
        // - **`player_performances`, not `games`.** `ingest_games_by_date_range`
        //   upserts *scheduled* rows too, so a games count says only that the
        //   schedule exists, not that anything was played. Keying on it would
        //   guarantee a false alarm on the season's most-watched nights: the
        //   09:30 UTC cron on Final Four Saturday runs Apr 3..Apr 4, where Apr 3
        //   has no games and Apr 4's semifinals are scheduled but untipped —
        //   games > 0, zero plays, and `is_core_season_date` true. Statlines
        //   exist only for games actually played, which is the evidence this
        //   check needs. (The sibling `empty_box` check above keys off perfs for
        //   the same reason.)
        //
        // - **The step must have succeeded.** `report.pbp_rows` stays 0 when
        //   the step errors, so without this the run would post a second issue
        //   line diagnosing a silently-empty feed right beneath the recorded
        //   HTTP error — two issues for one fault, the second contradicting the
        //   first and pointing at the wrong signal.
        //
        // - **Core-season only, and not under a simulated clock.** `simulate`
        //   seeds no PBP fixtures by design and replays in-season windows, so
        //   this would fire on every window of every replay. A *leftover*
        //   `CSTAT_SIMULATED_DATE` on prod would silence this check, but that
        //   condition is separately and loudly degraded by its own guard above,
        //   so it cannot hide quietly.
        //
        // - **The SEASON holds no PBP, not just this run's window.** Keying on
        //   the run's own row count gave the check zero settle slack, which
        //   contradicts the two-day `PBP_SETTLE_DAYS` that exists precisely
        //   because the feed can publish a night behind its box scores: with a
        //   one-night lag the window legitimately yields nothing and the run
        //   would post DEGRADED every night while nothing was wrong. The
        //   condition this alert is actually for is total loss, so ask that.
        if report.pbp_rows == 0
            && pbp_step_ok
            && report.ingest.player_performances > 0
            && is_core_season_date(&end_date)
            && crate::simulated_today().is_none()
            && !cstat_core::invariants::season_has_any_pbp(self.pool, self.season)
                .await
                .unwrap_or(true)
        {
            warn!(
                season = self.season,
                start_date = pbp_start_date.as_str(),
                end_date = end_date.as_str(),
                perfs = report.ingest.player_performances,
                "in-season nightly ingested zero play-by-play rows and the season holds none at all"
            );
            failures.push(format!(
                "the {season} season holds NO play-by-play at all, and this in-season run \
                 ({perfs} player statlines, 0 play rows, step reported success) added none — \
                 the PBP feed may be silently empty, which nothing else detects while there is \
                 no PBP to measure against (`pbp_date_coverage_gap` needs some)",
                season = self.season,
                perfs = report.ingest.player_performances,
            ));
        }

        // --- Ledger self-check ---
        // Deliberately the LAST thing before the run summary, so it counts every
        // `ledger.record` call in the run no matter where a future step is added
        // — positioning it next to the final `record` would silently under-count
        // the moment someone appended one below it.
        //
        // Ledger writes are fail-soft so an audit failure can't abort a healthy
        // ingest — but "one write blipped" and "every write has failed for days"
        // are the same silence, and only the second matters. Surfacing the count
        // degrades the Slack summary without touching control flow.
        //
        // Not hypothetical: a rewound `ingest_runs_id_seq` (issue #186, second
        // occurrence) made every INSERT a duplicate-key violation for three
        // nights in July 2026 while each run reported OK. Everything downstream
        // that reads the ledger — the 36h staleness route, the M5b coverage
        // scan, and `sync_to_prod.sh`'s full-sync guard — silently inverted:
        // the guard in particular reads a dark ledger as "prod is idle" and so
        // UNBLOCKS the destructive full sync that causes this in the first place.
        let ledger_write_failures = ledger.write_failures();
        if ledger_write_failures > 0 {
            warn!(
                season = self.season,
                failures = ledger_write_failures,
                "ingest_runs ledger writes FAILED — audit trail incomplete; check \
                 ingest_runs_id_seq vs max(id) (./scripts/sync_to_prod.sh --prod-status)"
            );
            failures.push(format!(
                "ingest_runs ledger: {ledger_write_failures} step write(s) FAILED — \
                 audit trail incomplete (health/self-heal/sync-guard all read it)"
            ));
        }

        // --- Run-completion notification (2.4) ---
        // Hard-fail steps already alerted-and-aborted above. A run that reaches
        // here either completed clean (success heartbeat) or completed with a
        // best-effort feed down (degraded warning). The success ping doubles as
        // a "the cron fired and finished" heartbeat.
        // A self-heal widened the window this run — surface it in whichever
        // summary posts (a healed run is still a SUCCESS: it recovered the gap).
        let heal_line: String = [
            heal_note.as_deref(),
            pbp_heal_note.as_deref(),
            pbp_heal_shortfall.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|n| format!("\n_:arrows_counterclockwise: {n}_"))
        .collect();
        // Warning-severity invariants, on BOTH summaries: a degraded run is
        // exactly when you also want to know which standing holes are open, and
        // a healthy run is where a *new* hole (a lost `playbyplay` night) has to
        // announce itself, since nothing else about that run looks wrong.
        let warn_line = match &invariant_warnings {
            Some(w) => format!("\n_:mag: warnings: {w}_"),
            None => String::new(),
        };
        // Which dates this run actually covered, and how long it took. Both were
        // log-only, and both are the first thing you want when reading a summary
        // cold — "0 games" means something completely different over an
        // off-season night than over a healed 9-day window.
        // On a PBP heal night the PBP step reached further back than the box
        // scores, so a single range would understate what the run covered.
        // Shown only on a heal night — on every ordinary night the two windows
        // are identical and a second copy of the same range is noise.
        let elapsed_secs = (Utc::now() - run_started).num_seconds().max(0);
        let pbp_range = if pbp_start_date == start_date {
            String::new()
        } else {
            format!("   ·   *PBP:*  {pbp_start_date} → {end_date}")
        };
        let window_line = format!(
            "*Window:*  {start_date} → {end_date}{pbp_range}   ·   *Took:*  {mins}m{secs:02}s",
            mins = elapsed_secs / 60,
            secs = elapsed_secs % 60,
        );
        // Repairs are rare and individually notable (a swapped game, a phantom
        // roster, a misidentified same-name player), so they are omitted
        // entirely on the overwhelmingly common night when all four are zero
        // rather than printed as a row of zeroes nobody reads.
        let repair_line = match &report.compute {
            Some(c)
                if c.corrected_swapped_games
                    + c.repaired_phantom_swaps
                    + c.reattached_misidentified
                    + c.deduplicated_players
                    > 0 =>
            {
                format!(
                    "\n*Repairs:*  {} swapped · {} phantom · {} misidentified · {} deduped",
                    c.corrected_swapped_games,
                    c.repaired_phantom_swaps,
                    c.reattached_misidentified,
                    c.deduplicated_players,
                )
            }
            _ => String::new(),
        };
        let torvik_line = match &report.torvik {
            Some(t) => format!(
                "*Torvik:*  {} season · {} per-game",
                t.upserted, report.torvik_games_persisted
            ),
            None => "*Torvik:*  skipped".to_string(),
        };
        // "Compute: ok" told you only that the step returned. These four counts
        // are the load-bearing derived products — player rows, the AdjEM solve,
        // CamPom, archetypes — so a step that silently computed over nothing
        // (the shape of every stale-input bug this pipeline has had) reads as a
        // row of zeroes instead of as "ok".
        let compute_str = match &report.compute {
            Some(c) => format!(
                "{} player-seasons · {} AdjEM · {} CamPom · {} archetypes",
                c.player_season_stats, c.adjusted_efficiency, c.campom, c.archetypes
            ),
            None => "skipped".to_string(),
        };
        // Shared by BOTH summaries. Originally only the SUCCESS post carried it,
        // which meant any single degraded line — including one from a
        // best-effort enrichment like the Torvik rebound backfill — replaced the
        // whole picture with a bare issue list, throwing away exactly the counts
        // that make a silently-empty run visible. A degraded run is when you
        // want the numbers most.
        let stat_block = format!(
            "{window_line}\n\
             *Box scores:*  {games} games · {pp} player perfs · {tp} team perfs\n\
             *Feeds:*  {elo} ELO · {fc} forecasts\n\
             {torvik_line}\n\
             *PBP/lineups:*  {pbp} play rows · {lu} lineup games\n\
             *Compute:*  {compute_str}{repair_line}\n\
             *Rate budget:*  {remaining}/{budget}",
            window_line = window_line,
            games = report.ingest.games,
            pp = report.ingest.player_performances,
            tp = report.ingest.team_performances,
            elo = report.ingest.elo_ratings,
            fc = report.ingest.game_forecasts,
            torvik_line = torvik_line,
            pbp = report.pbp_rows,
            lu = report.lineups_fetched,
            compute_str = compute_str,
            repair_line = repair_line,
            remaining = tokens_after,
            budget = budget,
        );
        if failures.is_empty() {
            notify::post_slack(
                notify::SlackChannel::Cron,
                &format!(
                    ":white_check_mark: *Nightly ingest OK* — season {season}\n\
                     {stat_block}{heal_line}{warn_line}\n\
                     _run {run_id}_",
                    season = self.season,
                    stat_block = stat_block,
                    heal_line = heal_line,
                    warn_line = warn_line,
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
            // Egress IP on every degraded alert, not just Torvik ones: it is one
            // short string, and for the whole class of IP-scoped refusals it is
            // the difference between reading the alert and digging through
            // Railway logs. Omitted from the SUCCESS alert, where it is noise.
            let egress_line = match &egress_ip {
                Some(ip) => format!("\n_egress IP {ip}_"),
                None => String::new(),
            };
            notify::post_slack(
                notify::SlackChannel::Cron,
                &format!(
                    ":warning: *Nightly ingest DEGRADED* — season {season}\n\
                     {stat_block}\n\
                     Completed with {n} issue(s):\n\
                     {issues}{heal_line}{warn_line}{egress_line}\n\
                     _run {run_id}_",
                    season = self.season,
                    stat_block = stat_block,
                    n = failures.len(),
                    issues = issues,
                    heal_line = heal_line,
                    warn_line = warn_line,
                    egress_line = egress_line,
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
    /// Play-by-play rows ingested this run (0 off-season / no games in window).
    pub pbp_rows: u64,
    /// Games whose NatStat lineups object was captured this run.
    pub lineups_fetched: u64,
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
        writeln!(
            f,
            "PBP/lineups: {} play-by-play rows, {} lineup games captured",
            self.pbp_rows, self.lineups_fetched
        )?;
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
    use super::{
        StepStatus, claimable_window, classify_torvik_games_outcome, is_core_season_date,
        plan_pbp_heal,
    };

    fn nd(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn nothing_missing_means_no_widening() {
        let plan = plan_pbp_heal(&[], nd("2026-12-08"), nd("2026-12-14"));
        assert_eq!(plan.from, None);
        assert!(plan.unreachable.is_empty());
    }

    #[test]
    fn heal_starts_at_the_earliest_reachable_missing_date() {
        // Not at the cap floor. Starting at the floor would re-fetch 12-08..12-10,
        // which are not missing — and because the floor advances nightly, that
        // window slides forward instead of converging on anything.
        let plan = plan_pbp_heal(
            &[nd("2026-12-11"), nd("2026-12-12")],
            nd("2026-12-08"),
            nd("2026-12-14"),
        );
        assert_eq!(plan.from, Some(nd("2026-12-11")));
        assert!(plan.unreachable.is_empty());
    }

    #[test]
    fn an_unreachable_date_does_not_block_the_reachable_ones() {
        // The regression that made round two's fix worse than the bug: the scan
        // reports oldest-first, so keying off its first element and bailing when
        // that one was out of reach abandoned every newer hole as well — until
        // those aged out of reach too, one date at a time.
        let plan = plan_pbp_heal(
            &[nd("2026-12-01"), nd("2026-12-11"), nd("2026-12-12")],
            nd("2026-12-08"),
            nd("2026-12-14"),
        );
        assert_eq!(
            plan.from,
            Some(nd("2026-12-11")),
            "12-11 and 12-12 are trivially reachable and must still be healed"
        );
        assert_eq!(plan.unreachable, vec![nd("2026-12-01")]);
    }

    #[test]
    fn unreachable_dates_are_reported_but_never_fetched() {
        let plan = plan_pbp_heal(
            &[nd("2026-12-01"), nd("2026-12-02")],
            nd("2026-12-08"),
            nd("2026-12-14"),
        );
        assert_eq!(plan.from, None, "nothing reachable — do not widen");
        assert_eq!(plan.unreachable, vec![nd("2026-12-01"), nd("2026-12-02")]);
    }

    #[test]
    fn a_wide_box_heal_covers_dates_the_pbp_cap_alone_could_not() {
        // The box-score heal reaches back 14 days to the PBP cap's 7, and the
        // PBP step rides on the box window when it has nothing of its own to
        // widen. So on an outage-recovery night, dates between `box_start` and
        // `floor` ARE fetched — and must not be reported as unrecoverable.
        // Filtering `unreachable` on the floor alone posted a DEGRADED summary
        // naming three dates the same run had just healed.
        let plan = plan_pbp_heal(
            &[nd("2026-12-03"), nd("2026-12-05"), nd("2026-12-06")],
            nd("2026-12-08"), // PBP cap floor
            nd("2026-12-04"), // box heal already widened this far back
        );
        assert_eq!(
            plan.from, None,
            "no date is both >= floor and < box_start, so nothing to widen"
        );
        assert_eq!(
            plan.unreachable,
            vec![nd("2026-12-03")],
            "12-05 and 12-06 are inside the box window this run already fetches"
        );
    }

    #[test]
    fn a_date_already_inside_the_box_window_needs_no_widening() {
        // The box heal (or the plain default window) already covers it, so
        // widening would claim work the run does anyway.
        let plan = plan_pbp_heal(&[nd("2026-12-14")], nd("2026-12-08"), nd("2026-12-14"));
        assert_eq!(plan.from, None);
        assert!(plan.unreachable.is_empty());
    }

    #[test]
    fn the_heal_converges_as_dates_are_filled() {
        // The property the ledger-based version never had. Each night the window
        // starts at the earliest date still missing, so as dates fill in the
        // window narrows and finally stops widening at all — rather than
        // re-pulling a fixed cap-width range forever.
        let floor = nd("2026-12-08");
        let box_start = nd("2026-12-14");
        assert_eq!(
            plan_pbp_heal(&[nd("2026-12-10"), nd("2026-12-12")], floor, box_start).from,
            Some(nd("2026-12-10"))
        );
        // 12-10 landed; only 12-12 is left.
        assert_eq!(
            plan_pbp_heal(&[nd("2026-12-12")], floor, box_start).from,
            Some(nd("2026-12-12"))
        );
        // All filled.
        assert_eq!(plan_pbp_heal(&[], floor, box_start).from, None);
    }

    // `today` is an explicit argument, so these are deterministic regardless of
    // what any other test in this binary has done to the process-global clock.
    const TODAY: &str = "2026-11-10";

    #[test]
    fn a_settled_window_is_claimed_whole() {
        assert_eq!(
            claimable_window("2026-11-05", "2026-11-06", nd(TODAY)),
            Some((nd("2026-11-05"), nd("2026-11-06")))
        );
    }

    #[test]
    fn a_window_running_past_yesterday_is_clamped_to_it() {
        // The load-bearing half: date D's games don't tip until ~D 23:00 UTC, so
        // a run on D may never claim D. Claiming it would put D permanently
        // beyond the coverage scan — every outage silently losing exactly one
        // date, the last good run's own day.
        assert_eq!(
            claimable_window("2026-11-05", TODAY, nd(TODAY)),
            Some((nd("2026-11-05"), nd("2026-11-09")))
        );
    }

    #[test]
    fn a_window_with_nothing_settled_claims_nothing() {
        // An operator's `--from today --to today` must claim nothing rather than
        // stamp an inverted range.
        assert_eq!(claimable_window(TODAY, TODAY, nd(TODAY)), None);
    }

    #[test]
    fn an_unparseable_date_claims_nothing() {
        assert_eq!(
            claimable_window("not-a-date", "2026-11-06", nd(TODAY)),
            None
        );
        assert_eq!(claimable_window("2026-11-05", "", nd(TODAY)), None);
    }

    #[test]
    fn a_clean_torvik_games_step_reports_ok_and_says_nothing() {
        let (status, lines) = classify_torvik_games_outcome(None, None);
        assert_eq!(status, StepStatus::Ok);
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn a_failed_persist_fails_the_step() {
        // The regression this exists for: the write of the `pit_cam_v3` serving
        // input blew up, and the step used to record `ok` anyway — green Slack
        // summary, 36h staleness clock reset on /api/health/ingest, and the only
        // trace a `warn!` in the Railway logs.
        let (status, lines) = classify_torvik_games_outcome(Some("deadlock detected"), None);
        assert_eq!(status, StepStatus::Failed);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("per-game persist failed"), "{lines:?}");
    }

    #[test]
    fn a_failed_rebound_backfill_degrades_but_does_not_fail_the_step() {
        // Deliberately NOT symmetric with the persist case. `torvik_games` is
        // served-critical because of the persist; failing the step here would
        // 503 the health endpoint over an enrichment, and a red light nobody can
        // act on is how a real outage gets missed.
        let (status, lines) = classify_torvik_games_outcome(None, Some("timeout"));
        assert_eq!(
            status,
            StepStatus::Ok,
            "the rebound backfill is not what makes this step served-critical"
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("rebound backfill failed"), "{lines:?}");
    }

    #[test]
    fn both_failing_reports_both() {
        let (status, lines) = classify_torvik_games_outcome(Some("boom"), Some("timeout"));
        assert_eq!(status, StepStatus::Failed);
        assert_eq!(
            lines.len(),
            2,
            "neither failure may be swallowed: {lines:?}"
        );
    }

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
