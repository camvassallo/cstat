use anyhow::Result;
use clap::{Parser, Subcommand};
use cstat_core::Database;
use cstat_ingest::current_natstat_season;
use cstat_ingest::ingest::{BootstrapOptions, SeasonIngester};
use cstat_ingest::{NatStatClient, TorkvikClient};
use tracing::{info, warn};

/// CLI default for `--year`. Resolved at parse time so the binary picks up
/// the current NCAA basketball season automatically as the calendar rolls
/// over (Nov+ → next year).
fn default_season() -> i32 {
    current_natstat_season()
}

/// Default `nightly` window: yesterday..today (UTC). Run at ~04:30 ET this
/// covers the prior night's games plus any corrections from NatStat's overnight
/// re-tabulation. Returns `(from, to)` as `YYYY-MM-DD`.
fn default_nightly_window() -> (String, String) {
    let today = cstat_ingest::today_utc();
    let yesterday = today - chrono::Duration::days(1);
    (
        yesterday.format("%Y-%m-%d").to_string(),
        today.format("%Y-%m-%d").to_string(),
    )
}

#[derive(Parser)]
#[command(name = "cstat-ingest", about = "NatStat data ingestion CLI for cstat")]
struct Cli {
    /// Clear all cached API responses before running (forces fresh fetches).
    #[arg(long, global = true)]
    no_cache: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bootstrap a season end-to-end: NatStat ingest + Torvik + compute.
    /// One command for "add a new season". Use `--no-torvik` / `--no-compute`
    /// to skip parts (e.g. when you'll batch them yourself).
    Season {
        /// Season year (e.g. 2026 for the 2025-26 season). Defaults to the
        /// current NCAA basketball season.
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Skip the Barttorvik player-stats step.
        #[arg(long)]
        no_torvik: bool,

        /// Skip the compute pipeline.
        #[arg(long)]
        no_compute: bool,
    },

    /// Ingest only teams for a season.
    Teams {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest only players for a season.
    Players {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest everything for a single team: roster, details (TCR), and player performances.
    Team {
        /// Team code (e.g. DUKE, UNC, KU)
        code: String,

        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// After ingest, run the season-wide compute pipeline so the new
        /// rows get rate stats, percentiles, AdjEM, etc. Compute is
        /// season-scoped, not team-scoped, so this re-derives every team
        /// for `year` — handy after `team` but heavier than the bare ingest.
        #[arg(long)]
        also_compute: bool,
    },

    /// Ingest games (and optionally box scores) for a date range.
    Games {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        to: Option<String>,
    },

    /// Ingest player performances (box scores) for a date range.
    Perfs {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        to: Option<String>,
    },

    /// Ingest play-by-play via the live API (intra-season path). Requires a
    /// date, date range, or gamecode — there is intentionally NO full-season
    /// default, since that's ~6,700 calls / ~13 hrs at the 500/hr cap. For
    /// backfill use `bootstrap-csv --with-pbp` (CSV `COPY`, seconds/season).
    /// Raw PBP is stored locally only and never synced to prod.
    PlayByPlay {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Single date (YYYY-MM-DD), e.g. yesterday's games for the nightly job.
        #[arg(long)]
        date: Option<String>,

        /// Start date (YYYY-MM-DD), paired with --to for a range.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD), paired with --from for a range.
        #[arg(long)]
        to: Option<String>,

        /// Single NatStat gamecode (e.g. 1511104).
        #[arg(long)]
        gamecode: Option<String>,
    },

    /// Capture NatStat's per-game server-computed 5-man lineup units (the
    /// `games;lineups` hydrate) into the durable `natstat_lineups` tables —
    /// the Tier-2 cross-season membership source. ~1 call per Final game (a
    /// season ≈ 11-13 hrs at the 500/hr cap); restart-safe — already-captured
    /// games are skipped via the `natstat_lineup_games` ledger. Local-only,
    /// never synced to prod.
    Lineups {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Stop after fetching N games (budget control for partial runs).
        #[arg(long)]
        limit: Option<u64>,

        /// Re-attempt games previously recorded as errors.
        #[arg(long)]
        retry_errors: bool,
    },

    /// Incremental update: fetch recent games and performances, then run
    /// compute so derived stats stay fresh. Use `--no-compute` to skip the
    /// post-step (e.g. when batching several updates).
    Update {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        from: String,

        /// End date (YYYY-MM-DD)
        #[arg(long)]
        to: String,

        /// Skip the compute pipeline at the end.
        #[arg(long)]
        no_compute: bool,
    },

    /// Nightly in-season refresh: the full served-critical input set (box
    /// scores + game forecasts + Torvik with per-game persistence) followed by
    /// compute, with each step recorded to the `ingest_runs` ledger. Unlike
    /// `update`, this refreshes Torvik BEFORE compute so the served CamPom /
    /// pit_cam_v3 inputs don't go stale. Defaults the window to yesterday..today
    /// (UTC) so late stat corrections are picked up.
    Nightly {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD). Defaults to yesterday (UTC).
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). Defaults to today (UTC).
        #[arg(long)]
        to: Option<String>,

        /// Skip the compute pipeline at the end.
        #[arg(long)]
        no_compute: bool,
    },

    /// Offline season-replay harness (M4): replay a historical season's CSVs
    /// through the REAL nightly orchestrator, window by window, against an
    /// isolated sim database — synthesized NatStat fixtures in api_cache, no
    /// live API calls, no rate budget, invariant checks after every window,
    /// and an idempotency re-run at the end. Refuses to run against the main
    /// or prod database. Start the sim DB with:
    /// `docker compose --profile sim up -d postgres-sim`
    Simulate {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Replay start date (YYYY-MM-DD), e.g. the season's opening day.
        #[arg(long)]
        from: String,

        /// Replay end date (YYYY-MM-DD).
        #[arg(long)]
        to: String,

        /// Days per simulated nightly window (1 = the production daily
        /// cadence; 7 ≈ a weekly catch-up / self-heal window).
        #[arg(long, default_value_t = 1)]
        step_days: i64,

        /// Drop + recreate the sim DB schema before replaying.
        #[arg(long)]
        reset: bool,

        /// Sim database URL. Defaults to $CSTAT_SIM_DATABASE_URL, then the
        /// docker-compose sim service (postgres://cstat:cstat@localhost:5433/cstat_sim).
        #[arg(long)]
        database_url: Option<String>,

        /// Directory holding the season CSV exports.
        #[arg(long, default_value = "data/natstat_csv")]
        csv_dir: std::path::PathBuf,

        /// Ingest-only replay: skip compute (and the compute-dependent
        /// invariant checks) in each window.
        #[arg(long)]
        no_compute: bool,
    },

    /// Ingest ELO ratings from /elo endpoint.
    Elo {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest per-game forecasts (ELO snapshots, win exp, betting lines) from /forecasts.
    Forecasts {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Run compute pipeline: derive season stats, schedules, percentiles from raw data.
    Compute {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Show rate limit status.
    Status,

    /// Preflight connectivity check: ping every feed (DB, NatStat, Torvik, and
    /// 247 if a JWT is set) and report reachable/skipped/down. Exits non-zero if
    /// a serving-critical feed is down (or, with --strict, if any feed is down).
    Preflight {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Also fail (non-zero exit) when a non-critical feed (247) is down.
        #[arg(long)]
        strict: bool,
    },

    /// Clean up expired cache entries.
    CleanCache,

    /// Ingest Barttorvik player season stats (advanced metrics, shot zones, bio).
    Torvik {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Also backfill missing rebounds from Torvik game-level data.
        #[arg(long)]
        rebounds: bool,

        /// Also persist per-game Torvik rows into `torvik_player_game_stats`.
        /// Prereq for point-in-time CamPom (see ROADMAP §"CamPom overfitting audit").
        #[arg(long)]
        persist_games: bool,
    },

    /// Ingest head coaches from barttorvik's `coachdict.json` into the
    /// `coaches` / `coach_seasons` tables. One HTTP call covers every season;
    /// we land the seasons we carry teams for, resolving each coachdict team
    /// name to a cstat team and flagging offseason coaching changes (`is_new_hc`).
    /// Foundation for the Coach-Above-Expectation metric — see
    /// `docs/coach_above_expectation_design.md`.
    Coaches {
        /// Restrict ingest to a single season. Defaults to all seasons present
        /// in the `teams` table. (The change-flag still reads the full dict.)
        #[arg(short, long)]
        year: Option<i32>,
    },

    /// End-to-end backtest for the roster-impact projection
    /// pipeline. Composes projected rosters for each target season, scores
    /// them with roster_impact_model.onnx, and compares to actual AdjEM.
    ProjectionsBacktest {
        /// Target seasons to backtest (comma-separated). Defaults to
        /// 2022..2026 — every season the projected roster can be
        /// reconstructed for (transfers solid 2021+, recruits 2014+).
        #[arg(long, value_delimiter = ',', default_values_t = [2022, 2023, 2024, 2025, 2026])]
        years: Vec<i32>,

        /// Optional JSON dump of per-team predictions for downstream
        /// residual analysis. One record per scored team:
        /// `{team_id, team_name, season, roster_proj, boxscore_proj, baseline, actual}`.
        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },

    /// Materialize the preseason roster-impact projection per (season, team)
    /// into `team_preseason_projection`. Runs the same season-wide roster-impact
    /// composition `/api/projections` does, once per target year, and writes
    /// each team's projected AdjEM so the predict route can read it cheaply
    /// for the preseason × pit early-season blend (ROADMAP §6).
    ComputeProjections {
        /// Target seasons to compute (comma-separated). Each needs
        /// `transfers(year − 1)` + recruits + a played base season.
        #[arg(long, value_delimiter = ',', default_values_t = [2024, 2025, 2026])]
        years: Vec<i32>,
    },

    /// Per-week crossover calibration for the preseason × pit early-season
    /// blend (ROADMAP §6). Replays a played season's games week by week and
    /// reports preseason-only / pit-only / best-blend MAE on the shared subset,
    /// the currently-scheduled blend's MAE, the crossover week, and an HCA
    /// sweep. Needs `compute-projections --years YEAR` to have run first.
    MeasureBlendAccuracy {
        /// Seasons to pool for the calibration (comma-separated). Each must be a
        /// played season with preseason projections materialized — 2024–2026
        /// today. Pooling all three gives the schedule grid-search more signal.
        #[arg(long, value_delimiter = ',', default_values_t = [2024, 2025, 2026])]
        years: Vec<i32>,
    },

    /// Compare CamPom composites in torvik_player_stats against an external reference CSV.
    /// Pass condition: max abs diff < 0.01 across every CamPom intermediate and final.
    CampomParity {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Path to the baseline CSV. Defaults to docs/campom_2026_baseline.csv.
        #[arg(long, default_value = "docs/campom_2026_baseline.csv")]
        baseline: std::path::PathBuf,
    },

    /// Ingest the 247Sports transfer portal for a class year (matches 247's `year=`
    /// query param, i.e. the spring portal-cycle calendar year, NOT cstat-season).
    /// Requires TFS_247_JWT env var (capture from DevTools; ~6 hour expiry).
    Transfers {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Skip pages whose lastUpdated predates our DB cursor.
        /// Default is a full refresh (every page touched).
        #[arg(long)]
        incremental: bool,

        /// Load from a local snapshot file instead of hitting the live API.
        /// Useful for the initial seed and for reproducible local dev.
        #[arg(long)]
        bootstrap_from: Option<std::path::PathBuf>,

        /// Skip ingest entirely and only run the cstat_player_id resolution
        /// pass against the rows already in the `transfers` table. Useful for
        /// back-resolving historical years once the destination season's
        /// players are ingested. Mutually exclusive with `--bootstrap-from`
        /// and ignores `--incremental`.
        #[arg(long, conflicts_with = "bootstrap_from")]
        resolve_only: bool,

        /// Skip the cstat_player_id resolution pass after ingest. By default
        /// we resolve `(full_name, source_institution)` → `players.id` joins.
        #[arg(long)]
        no_resolve_players: bool,
    },

    /// Load NBA draft early-entrants into the `draft_entrants` table from the
    /// `data/draft/{year}_early_entrants.json` captures. These are the firm
    /// departures the roster projection removes (a drafted player doesn't
    /// return). Idempotent upsert; run once after capturing a new draft year
    /// (see scripts/build_historical_draft_entrants.py for the historical set).
    Draft {
        /// Directory of `{year}_early_entrants.json` files.
        #[arg(long, default_value = "data/draft")]
        dir: std::path::PathBuf,

        /// Provenance stamp written to each row's `source` column.
        #[arg(long, default_value = "tankathon")]
        source: String,
    },

    /// Load curated non-portal, non-draft program exits into the
    /// `player_departures` table from the `data/departures/{year}_departures.json`
    /// captures — players who signed professionally abroad, retired, were
    /// dismissed, or otherwise left without touching the portal or the draft.
    /// No feed reports these; the rows are hand-entered from news reports.
    /// Idempotent upsert. Run `departures-audit` first to find the candidates.
    Departures {
        /// Directory of `{year}_departures.json` files.
        #[arg(long, default_value = "data/departures")]
        dir: std::path::PathBuf,
    },

    /// Load the hand-curated eligibility returns into `player_returns` —
    /// players whose `class_year` says they graduated but who are coming back
    /// under the NCAA 5-in-5 rule (issue #220). `granted` rows project as
    /// ordinary returners; `contested` rows go to the uncertain (`?`) bucket
    /// and widen the team's floor/ceiling band.
    Returns {
        /// Directory of `{year}_returns.json` files.
        #[arg(long, default_value = "data/returns")]
        dir: std::path::PathBuf,
    },

    /// Print the offseason attrition worklist: curated rows that resolve to
    /// nobody (silent no-ops from a typo'd name) in `player_departures` and in
    /// `player_returns`, then the returners the projection currently assumes
    /// are coming back, ranked by CamPom.
    /// Read-only. Run it in July and skim the top of the list against the news.
    DeparturesAudit {
        /// Base season N — the completed season being projected into N+1.
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Floor on base-season CamPom for the at-risk list.
        #[arg(long, default_value_t = 4.0)]
        min_cam: f64,

        /// Include US players. Off by default — the international cohort is
        /// where the unreported exits concentrate — but domestic dismissals
        /// and medical retirements land in the same blind spot.
        #[arg(long)]
        all_nationalities: bool,

        /// Cap on printed at-risk rows.
        #[arg(long, default_value_t = 60)]
        limit: usize,
    },

    /// Ingest 247Sports composite recruit rankings for a class year. `year` is
    /// the recruiting class year (= spring of HS graduation, = 247's URL
    /// `{year}-basketball` slug). Class-of-2026 recruits first appear in
    /// cstat-season 2027 box scores.
    /// Requires TFS_247_JWT env var (same JWT as Transfers; ~6h expiry).
    Recruits {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Institution groups to ingest. Comma-separated; values:
        /// `highschool`, `juco`, `prep`. Defaults to `highschool` — the
        /// `compositerecruitrankings` endpoint returns identical content
        /// for all three values (verified empirically against class-of-2026:
        /// first player is "Tyran Stokes" regardless of `InstitutionGroup=`).
        /// JUCO/prep rankings live elsewhere on 247; wire those up here once
        /// we find the right endpoint.
        #[arg(long, value_delimiter = ',', default_value = "highschool")]
        groups: Vec<String>,

        /// Ingest the public national commits feed
        /// (`/season/{year}-basketball/commits/`) instead of the composite
        /// rankings. This is the cookie-free path (no `TFS_247_*` needed) and
        /// captures every commit — unranked / international / prep / G-League —
        /// that the ranked composite omits (issue #175). Rows land tagged
        /// `institution_group='commits'` and never clobber composite-owned
        /// rows, so run it alongside the normal composite ingest (two
        /// invocations) for full coverage. Mutually exclusive with the
        /// composite-only `--bootstrap-from` / `--dump-snapshot`; `--groups`
        /// is ignored in this mode (the feed has no per-cohort split).
        #[arg(long, conflicts_with_all = ["bootstrap_from", "dump_snapshot", "resolve_only"])]
        commits_feed: bool,

        /// Load from a local snapshot file instead of hitting the live API.
        #[arg(long)]
        bootstrap_from: Option<std::path::PathBuf>,

        /// Save the live fetch to this path before upserting (for snapshot capture).
        #[arg(long)]
        dump_snapshot: Option<std::path::PathBuf>,

        /// Skip ingest entirely and only run the resolution passes against the
        /// rows already in the `recruits` table. Useful for back-resolving
        /// historical class years once the freshman season's players are
        /// ingested. Mutually exclusive with `--bootstrap-from` and
        /// `--dump-snapshot`.
        #[arg(long, conflicts_with_all = ["bootstrap_from", "dump_snapshot"])]
        resolve_only: bool,

        /// Skip the committed_team_id resolution pass after ingest.
        #[arg(long)]
        no_resolve_teams: bool,

        /// Skip the cstat_player_id resolution pass after ingest. Pass 2 is
        /// mostly a no-op until cstat-season `year + 1` box scores are ingested.
        #[arg(long)]
        no_resolve_players: bool,
    },

    /// Bulk-load a season from NatStat dashboard CSV exports — alternative
    /// to the rate-limited `season` API path for backfilling historical
    /// years. Expects `data/natstat_csv/{year}/NatStat-MBB{year}-{Kind}-*.csv`
    /// for kinds Teams, Games, Team_Statlines, Player_Statlines.
    /// Skips Players.csv (different ID space — see module docs). Play-by-Play
    /// is opt-in via `--with-pbp` (full bootstrap) or `--pbp-only` (PBP alone).
    /// Runs in seconds; pair with `compute --year` to derive season stats
    /// afterward, or pass `--also-compute`.
    BootstrapCsv {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Directory containing the season's CSV files.
        #[arg(long, default_value = "data/natstat_csv")]
        dir: std::path::PathBuf,

        /// Run `compute_all` after loading. Off by default so multi-season
        /// bulk loads can batch one compute pass at the end.
        #[arg(long)]
        also_compute: bool,

        /// Skip the `/elo` ingest step. CSV exports don't include ELO ratings,
        /// so we fetch them from the live API (~4 paginated calls per season,
        /// trivial against the 500/hr budget). Pass `--no-elo` only for an
        /// air-gapped load — otherwise leave it on so historical seasons land
        /// with the same ELO coverage as the live `season` path.
        #[arg(long)]
        no_elo: bool,

        /// Also load `Play-by-Play.csv` into the local-only `play_by_play`
        /// table. Off by default (~3.35M rows/season); raw PBP never syncs to
        /// prod. Requires the season's games + player rows already loaded
        /// (this same run loads them first).
        #[arg(long)]
        with_pbp: bool,

        /// Load ONLY `Play-by-Play.csv`, against already-loaded games/players —
        /// skips the box-score re-bootstrap entirely. Use this to add PBP to a
        /// season ingested via the live API without reverting box scores to the
        /// CSV snapshot. Ignores --with-pbp / --no-elo / --also-compute.
        #[arg(long, conflicts_with = "with_pbp")]
        pbp_only: bool,
    },

    /// Fetch a raw API endpoint and dump the JSON (for exploration).
    Explore {
        /// Endpoint (e.g., "teams", "players", "playerperfs")
        endpoint: String,

        /// Range params (e.g., "2026,DUKE")
        #[arg(short, long)]
        range: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cstat_ingest=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // A leftover simulated clock is dangerous on a real service: the nightly
    // window pins to one past date forever while every monitor stays green,
    // and predict blends live forecasts against a stale "today". Every run of
    // this binary announces it loudly; the nightly additionally marks the run
    // degraded so the Slack summary surfaces it in prod. Uses the same parse
    // as `today_utc` so an empty/unparsable value (ignored by the clock)
    // doesn't warn about a pin that isn't happening.
    if let Some(sim_date) = cstat_ingest::env_simulated_date() {
        warn!(
            %sim_date,
            "CSTAT_SIMULATED_DATE is set — all date-sensitive defaults (season, nightly window) \
             use the simulated clock. Unset it on any real service."
        );
    }

    // Simulate dispatches BEFORE the shared main-DB setup below: an isolated,
    // offline sim run must not connect to (or migrate!) the main DATABASE_URL
    // database, must not require NATSTAT_API_KEY, and `--no-cache` must never
    // clear the main DB's api_cache as a side effect.
    if let Commands::Simulate {
        year,
        from,
        to,
        step_days,
        reset,
        database_url,
        csv_dir,
        no_compute,
    } = cli.command
    {
        let sim_url = database_url
            .or_else(|| std::env::var("CSTAT_SIM_DATABASE_URL").ok())
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "postgres://cstat:cstat@localhost:5433/cstat_sim".to_string());
        let opts = cstat_ingest::simulate::SimulateOptions {
            season: year,
            from: chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d")
                .map_err(|e| anyhow::anyhow!("--from {from}: {e}"))?,
            to: chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d")
                .map_err(|e| anyhow::anyhow!("--to {to}: {e}"))?,
            step_days,
            reset,
            csv_dir,
            database_url: sim_url,
            no_compute,
        };
        return cstat_ingest::simulate::run(opts).await;
    }

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let api_key = std::env::var("NATSTAT_API_KEY").expect("NATSTAT_API_KEY must be set");

    let db = Database::connect(&database_url).await?;
    db.migrate().await?;
    info!("connected to database");

    let client = NatStatClient::new(
        db.pool.clone(),
        api_key,
        cstat_ingest::rate_budget_from_env(),
    );

    if cli.no_cache {
        let cleared = client.clear_all_cache().await?;
        info!(cleared, "cleared API cache");
        println!("Cleared {cleared} cached API responses");
    }

    match cli.command {
        Commands::Season {
            year,
            no_torvik,
            no_compute,
        } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester
                .bootstrap_season(BootstrapOptions {
                    torvik: !no_torvik,
                    compute: !no_compute,
                })
                .await?;
            print!("{report}");
        }

        Commands::Teams { year } => {
            let count = cstat_ingest::ingest::teams::ingest_teams(&client, &db.pool, year).await?;
            println!("Ingested {count} teams for {year}");
        }

        Commands::Players { year } => {
            let count =
                cstat_ingest::ingest::players::ingest_all_rosters(&client, &db.pool, year).await?;
            println!("Ingested {count} players for {year}");
        }

        Commands::Team {
            code,
            year,
            also_compute,
        } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_team(&code).await?;
            print!("{report}");
            if also_compute {
                info!(year, "running compute_all after team ingest");
                let report = cstat_core::compute::compute_all(
                    &db.pool,
                    year,
                    cstat_ingest::should_infer_newcomers(year),
                )
                .await?;
                println!("{report}");
            }
        }

        Commands::Games { year, from, to } => {
            let count = match (from, to) {
                (Some(f), Some(t)) => {
                    cstat_ingest::ingest::games::ingest_games_by_date_range(
                        &client, &db.pool, year, &f, &t,
                    )
                    .await?
                }
                _ => cstat_ingest::ingest::games::ingest_games(&client, &db.pool, year).await?,
            };
            println!("Ingested {count} games for {year}");
        }

        Commands::Perfs { year, from, to } => {
            let count = match (from, to) {
                (Some(f), Some(t)) => {
                    cstat_ingest::ingest::games::ingest_player_performances_by_date_range(
                        &client, &db.pool, year, &f, &t,
                    )
                    .await?
                }
                _ => {
                    cstat_ingest::ingest::games::ingest_player_performances(&client, &db.pool, year)
                        .await?
                }
            };
            println!("Ingested {count} player performances for {year}");
        }

        Commands::PlayByPlay {
            year,
            date,
            from,
            to,
            gamecode,
        } => {
            use cstat_ingest::ingest::playbyplay;
            let report = match (date, from, to, gamecode) {
                (Some(d), _, _, _) => {
                    playbyplay::ingest_play_by_play_by_date(&client, &db.pool, year, &d).await?
                }
                (_, Some(f), Some(t), _) => {
                    playbyplay::ingest_play_by_play_by_date_range(&client, &db.pool, year, &f, &t)
                        .await?
                }
                (_, _, _, Some(g)) => {
                    playbyplay::ingest_play_by_play_by_gamecode(&client, &db.pool, year, &g).await?
                }
                _ => {
                    anyhow::bail!(
                        "play-by-play needs --date, --from/--to, or --gamecode (no full-season \
                         default — use `bootstrap-csv --with-pbp` for backfill)"
                    );
                }
            };
            println!("{report}");
        }

        Commands::Lineups {
            year,
            limit,
            retry_errors,
        } => {
            let report = cstat_ingest::ingest::lineups::ingest_lineups_for_season(
                &client,
                &db.pool,
                year,
                None, // CLI = full-season backfill sweep (nightly passes its window)
                limit,
                retry_errors,
            )
            .await?;
            println!("{report}");
        }

        Commands::Elo { year } => {
            let count =
                cstat_ingest::ingest::elo::ingest_elo_ratings(&client, &db.pool, year).await?;
            println!("Ingested {count} ELO ratings for {year}");
        }

        Commands::Forecasts { year } => {
            let count =
                cstat_ingest::ingest::elo::ingest_game_forecasts(&client, &db.pool, year).await?;
            println!("Ingested {count} game forecasts for {year}");
        }

        Commands::Compute { year } => {
            let report = cstat_core::compute::compute_all(
                &db.pool,
                year,
                cstat_ingest::should_infer_newcomers(year),
            )
            .await?;
            println!("{report}");
        }

        Commands::BootstrapCsv {
            year,
            dir,
            also_compute,
            no_elo,
            with_pbp,
            pbp_only,
        } => {
            use cstat_ingest::ingest::bootstrap_csv;
            if pbp_only {
                let count = bootstrap_csv::load_pbp_only(&db.pool, year, &dir).await?;
                println!("Loaded {count} play_by_play rows for {year} (PBP-only)");
                return Ok(());
            }
            let report =
                bootstrap_csv::bootstrap_from_csv_dir(&db.pool, year, &dir, with_pbp).await?;
            println!("{report}");
            if !no_elo {
                info!(year, "fetching /elo for CSV-bootstrapped season");
                let elo_count =
                    cstat_ingest::ingest::elo::ingest_elo_ratings(&client, &db.pool, year).await?;
                println!("Ingested {elo_count} ELO ratings for {year}");
            }
            if also_compute {
                info!(year, "running compute_all after CSV bootstrap");
                let compute = cstat_core::compute::compute_all(
                    &db.pool,
                    year,
                    cstat_ingest::should_infer_newcomers(year),
                )
                .await?;
                println!("{compute}");
            }
        }

        Commands::Update {
            year,
            from,
            to,
            no_compute,
        } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_recent(&from, &to, !no_compute).await?;
            print!("{report}");
        }

        Commands::Nightly {
            year,
            from,
            to,
            no_compute,
        } => {
            let (default_from, default_to) = default_nightly_window();
            // Self-heal only a DEFAULTED window (the production cron path). An
            // operator who passes an explicit --from wants exactly that range,
            // so a manual backfill is never silently widened.
            let self_heal = from.is_none();
            let from = from.unwrap_or(default_from);
            let to = to.unwrap_or(default_to);
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.nightly(&from, &to, !no_compute, self_heal).await?;
            print!("{report}");
        }

        // Simulate is dispatched before the main-DB setup at the top of
        // main() — an isolated sim run never touches DATABASE_URL. This arm
        // is unreachable but keeps the match exhaustive.
        Commands::Simulate { .. } => unreachable!("simulate dispatches before main-DB setup"),

        Commands::Status => {
            let remaining = client.rate_limit_remaining().await;
            let budget = cstat_ingest::rate_budget_from_env();
            println!("Local rate limit tokens: {remaining}/{budget}");
        }

        Commands::Preflight { year, strict } => {
            // Standalone check includes 247 (an operator may run this before a
            // transfers/recruits capture); the nightly deliberately does not.
            let report = cstat_ingest::preflight::run(&client, &db.pool, year, true).await;
            print!("{}", report.render());
            let fail = if strict {
                report.any_down()
            } else {
                report.critical_down()
            };
            if fail {
                let down = report.down_feeds().join(", ");
                anyhow::bail!("preflight failed — feed(s) down: {down}");
            }
        }

        Commands::CleanCache => {
            let removed = client.cleanup_cache().await?;
            println!("Removed {removed} expired cache entries");
        }

        Commands::Torvik {
            year,
            rebounds,
            persist_games,
        } => {
            let torvik = TorkvikClient::new();
            let (upserted, matched) =
                cstat_ingest::ingest::torvik::ingest_torvik_player_stats(&torvik, &db.pool, year)
                    .await?;
            println!(
                "Torvik player stats: {upserted} upserted, {matched} matched to cstat players \
                 ({} unlinked)",
                upserted.saturating_sub(matched)
            );

            // Both flags consume the same gzip JSON from
            // `barttorvik.com/{year}_all_advgames.json.gz` — fetch once and
            // dispatch so a `--rebounds --persist-games` daily-ingest call
            // doesn't pull ~25 MB twice.
            if rebounds || persist_games {
                let games = torvik.fetch_game_stats(year).await?;
                if rebounds {
                    let updated = cstat_ingest::ingest::torvik::apply_rebound_backfill(
                        &db.pool, &games, year,
                    )
                    .await?;
                    println!("Rebound backfill: {updated} game rows updated");
                }
                if persist_games {
                    let inserted = cstat_ingest::ingest::torvik::apply_persist_torvik_game_stats(
                        &db.pool, &games, year,
                    )
                    .await?;
                    println!("Torvik per-game persistence: {inserted} rows upserted");
                }
            }
        }

        Commands::Coaches { year } => {
            let torvik = TorkvikClient::new();
            let report =
                cstat_ingest::ingest::coaches::ingest_coaches(&torvik, &db.pool, year).await?;
            println!(
                "coachdict: {} coach-seasons across {} season(s) — {} teams matched, {} unmatched, {} distinct coaches, {} new-HC flags",
                report.rows,
                report.seasons,
                report.matched_teams,
                report.unmatched_teams,
                report.distinct_coaches,
                report.new_hc,
            );
        }

        Commands::CampomParity { year, baseline } => {
            let report = cstat_ingest::campom_parity::run(&db.pool, year, &baseline).await?;
            report.print();
            if !report.passed() {
                std::process::exit(1);
            }
        }

        Commands::ProjectionsBacktest { years, output } => {
            let model_dir =
                std::env::var("MODEL_DIR").unwrap_or_else(|_| "training/models".to_string());
            let predictor =
                cstat_core::inference::Predictor::load(std::path::Path::new(&model_dir))
                    .map_err(|e| anyhow::anyhow!("failed to load models from {model_dir}: {e}"))?;
            cstat_ingest::projections_backtest::run(
                &db.pool,
                &predictor,
                std::path::Path::new(&model_dir),
                &years,
                output.as_deref(),
            )
            .await?;
        }

        Commands::ComputeProjections { years } => {
            let model_dir =
                std::env::var("MODEL_DIR").unwrap_or_else(|_| "training/models".to_string());
            let predictor =
                cstat_core::inference::Predictor::load(std::path::Path::new(&model_dir))
                    .map_err(|e| anyhow::anyhow!("failed to load models from {model_dir}: {e}"))?;
            cstat_ingest::compute_projections::run(
                &db.pool,
                &predictor,
                std::path::Path::new(&model_dir),
                &years,
            )
            .await?;
        }

        Commands::MeasureBlendAccuracy { years } => {
            let model_dir =
                std::env::var("MODEL_DIR").unwrap_or_else(|_| "training/models".to_string());
            let predictor =
                cstat_core::inference::Predictor::load(std::path::Path::new(&model_dir))
                    .map_err(|e| anyhow::anyhow!("failed to load models from {model_dir}: {e}"))?;
            cstat_ingest::measure_blend_accuracy::run(&db.pool, &predictor, &years).await?;
        }

        Commands::Transfers {
            year,
            incremental,
            bootstrap_from,
            resolve_only,
            no_resolve_players,
        } => {
            if resolve_only {
                if no_resolve_players {
                    anyhow::bail!("--resolve-only with --no-resolve-players is a no-op");
                }
                let n =
                    cstat_ingest::ingest::transfers::resolve_cstat_joins(&db.pool, year).await?;
                println!("transfers {year}: cstat_player_id resolved on {n} row(s)");
            } else {
                let report = if let Some(path) = bootstrap_from {
                    info!("bootstrapping transfers from {}", path.display());
                    cstat_ingest::ingest::transfers::bootstrap_from_snapshot(&db.pool, year, &path)
                        .await?
                } else {
                    // Guest-first: mints a token off the public portal page when
                    // `TFS_247_JWT` is unset, so this runs with no credential at
                    // all. A subscriber token still wins when present.
                    let tfs = cstat_ingest::TfsClient::from_env_or_guest(year).await?;
                    match cstat_ingest::ingest::transfers::ingest_live(
                        &tfs,
                        &db.pool,
                        year,
                        incremental,
                    )
                    .await
                    {
                        Ok(r) => r,
                        // JWT expiry (401/403) is the #1 247 connectivity risk
                        // (~6h token, no renewal). Don't hard-fail: fall back to
                        // the last committed snapshot so stale-but-present beats
                        // empty, and point at the re-capture runbook.
                        Err(cstat_ingest::ingest::transfers::TransferIngestError::Tfs(
                            cstat_ingest::TfsError::JwtExpired { status },
                        )) => {
                            let snapshot =
                                std::path::PathBuf::from(format!("data/transfers/{year}_raw.json"));
                            warn!(
                                year,
                                status,
                                snapshot = %snapshot.display(),
                                "247 JWT rejected (expired/revoked) — re-capture it (see docs/247_jwt_recapture.md). Falling back to last snapshot"
                            );
                            if snapshot.exists() {
                                cstat_ingest::ingest::transfers::bootstrap_from_snapshot(
                                    &db.pool, year, &snapshot,
                                )
                                .await?
                            } else {
                                anyhow::bail!(
                                    "247 JWT expired (HTTP {status}) and no fallback snapshot at {}. \
                                     Re-capture the JWT (docs/247_jwt_recapture.md) and re-run.",
                                    snapshot.display()
                                );
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                println!(
                    "transfers {}: {} upserted, {} pruned across {} page(s)",
                    report.year, report.upserts, report.pruned, report.total_pages
                );

                if !no_resolve_players {
                    let n = cstat_ingest::ingest::transfers::resolve_cstat_joins(&db.pool, year)
                        .await?;
                    println!("transfers {year}: cstat_player_id resolved on {n} row(s)");
                }
            }
        }

        Commands::Draft { dir, source } => {
            let reports =
                cstat_ingest::ingest::draft::bootstrap_from_dir(&db.pool, &dir, &source).await?;
            let total: usize = reports.iter().map(|r| r.rows).sum();
            for r in &reports {
                println!("draft {}: {} entrants", r.year, r.rows);
            }
            println!(
                "draft: {} entrants across {} year(s) loaded into draft_entrants",
                total,
                reports.len()
            );
        }

        Commands::Departures { dir } => {
            let reports =
                cstat_ingest::ingest::departures::bootstrap_from_dir(&db.pool, &dir).await?;
            let total: usize = reports.iter().map(|r| r.rows).sum();
            for r in &reports {
                println!("departures {}: {} exit(s)", r.year, r.rows);
            }
            println!(
                "departures: {} exit(s) across {} year(s) loaded into player_departures. \
                 Run `departures-audit` to confirm they resolved to real roster players.",
                total,
                reports.len()
            );
        }

        Commands::Returns { dir } => {
            let reports = cstat_ingest::ingest::returns::bootstrap_from_dir(&db.pool, &dir).await?;
            let total: usize = reports.iter().map(|r| r.rows).sum();
            let contested: usize = reports.iter().map(|r| r.contested).sum();
            for r in &reports {
                println!(
                    "returns {}: {} return(s), {} contested",
                    r.year, r.rows, r.contested
                );
            }
            println!(
                "returns: {} return(s) across {} year(s) loaded into player_returns \
                 ({contested} still contested). A row that matches no roster player is a \
                 silent no-op — run `departures-audit --year N` to confirm each one placed \
                 its player; it exits 2 if any didn't.",
                total,
                reports.len()
            );
        }

        Commands::DeparturesAudit {
            year,
            min_cam,
            all_nationalities,
            limit,
        } => {
            let model_dir =
                std::env::var("MODEL_DIR").unwrap_or_else(|_| "training/models".to_string());
            let predictor =
                cstat_core::inference::Predictor::load(std::path::Path::new(&model_dir))
                    .map_err(|e| anyhow::anyhow!("failed to load models from {model_dir}: {e}"))?;
            let opts = cstat_ingest::departures_audit::AuditOptions {
                base_season: year,
                min_cam,
                intl_only: !all_nationalities,
                limit,
            };
            let unmatched =
                cstat_ingest::departures_audit::run(&db.pool, &predictor, &opts).await?;
            if unmatched > 0 {
                // Non-zero exit so a scripted run can't quietly ship a capture
                // where a typo'd row is doing nothing.
                std::process::exit(2);
            }
        }

        Commands::Recruits {
            year,
            groups,
            commits_feed,
            bootstrap_from,
            dump_snapshot,
            resolve_only,
            no_resolve_teams,
            no_resolve_players,
        } => {
            if resolve_only {
                if no_resolve_teams && no_resolve_players {
                    anyhow::bail!(
                        "--resolve-only with both --no-resolve-teams and --no-resolve-players is a no-op"
                    );
                }
                if !no_resolve_teams {
                    let n =
                        cstat_ingest::ingest::recruits::resolve_team_joins(&db.pool, year).await?;
                    println!("recruits {year}: committed_team_id resolved on {n} row(s)");
                }
                if !no_resolve_players {
                    let n = cstat_ingest::ingest::recruits::resolve_player_joins(&db.pool, year)
                        .await?;
                    println!("recruits {year}: cstat_player_id resolved on {n} row(s)");
                }
            } else {
                let report = if commits_feed {
                    info!(year, "ingesting 247 national commits feed (cookie-free)");
                    let client = cstat_ingest::Recruit247Client::public();
                    cstat_ingest::ingest::recruits::ingest_commits(&client, &db.pool, year).await?
                } else if let Some(path) = bootstrap_from {
                    info!("bootstrapping recruits from {}", path.display());
                    cstat_ingest::ingest::recruits::bootstrap_from_snapshot(&db.pool, year, &path)
                        .await?
                } else {
                    let parsed: Vec<cstat_ingest::InstitutionGroup> = groups
                        .iter()
                        .filter_map(|g| {
                            let parsed = cstat_ingest::InstitutionGroup::parse(g);
                            if parsed.is_none() {
                                tracing::warn!(group = g, "unknown institution_group — skipping");
                            }
                            parsed
                        })
                        .collect();
                    if parsed.is_empty() {
                        anyhow::bail!(
                            "no valid institution_groups parsed from `--groups`; pass any of: highschool, juco, prep"
                        );
                    }
                    let client = cstat_ingest::Recruit247Client::from_env()?;
                    cstat_ingest::ingest::recruits::ingest_live(
                        &client,
                        &db.pool,
                        year,
                        &parsed,
                        dump_snapshot.as_deref(),
                    )
                    .await?
                };
                let by_group: Vec<String> = report
                    .by_group
                    .iter()
                    .map(|(g, n)| format!("{g}={n}"))
                    .collect();
                println!(
                    "recruits {}: {} upserted across {} page(s) ({})",
                    report.year,
                    report.upserts,
                    report.total_pages,
                    by_group.join(", ")
                );

                if !no_resolve_teams {
                    let n =
                        cstat_ingest::ingest::recruits::resolve_team_joins(&db.pool, year).await?;
                    println!("recruits {year}: committed_team_id resolved on {n} row(s)");
                }
                if !no_resolve_players {
                    let n = cstat_ingest::ingest::recruits::resolve_player_joins(&db.pool, year)
                        .await?;
                    println!("recruits {year}: cstat_player_id resolved on {n} row(s)");
                }
            }
        }

        Commands::Explore { endpoint, range } => {
            let response = client.get(&endpoint, range.as_deref(), None, None).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }

    Ok(())
}
