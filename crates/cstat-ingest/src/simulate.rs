//! Offline season-replay harness (M4, `docs/in_season_ingest_plan.md` Phase 3).
//!
//! Replays a historical season through the **real** `SeasonIngester::nightly`
//! orchestrator, date window by date window, against an isolated simulation
//! database — no live NatStat calls, no rate budget, and hard guarantees that
//! the main database (the `sync_to_prod.sh` source of truth) is never touched.
//!
//! How offline replay works: `NatStatClient::get` consults the Postgres
//! `api_cache` table *before* the rate limiter or any HTTP, keyed only by
//! `(endpoint, range, offset)`. The harness synthesizes NatStat-v4-shaped
//! response pages from the committed CSV exports (`data/natstat_csv/{year}/`)
//! and seeds them into the sim DB's `api_cache` under exactly the keys the
//! nightly's date-range calls will look up — so `games`/`playerperfs`/
//! `teamperfs` flow through the same JSON parsing, upserts, pagination, and
//! `compute_all` as a production night. A deliberately bogus API key makes
//! any fixture gap fail loudly (auth error) instead of silently burning
//! budget.
//!
//! What is NOT offline: the Torvik steps and the preflight Torvik probe hit
//! barttorvik.com directly (they never go through `api_cache`). Both are
//! fail-soft in the nightly, so on a network-less machine they record as
//! failed/degraded and the serving-critical chain still completes; with
//! network they simply work. The `/forecasts` and `/elo` fixtures are seeded
//! as *empty* success pages — deliberately exercising the "empty payload
//! doesn't fail the run" edge case every window.
//!
//! Safety rails, in order: the sim DB URL must not resolve to the same
//! host/port/database as `DATABASE_URL` or `PROD_DATABASE_URL`; all outbound
//! notifications (Slack, Cloudflare purge, heartbeat) are muted process-wide
//! via `notify::set_suppressed`; and the simulated clock is advanced with
//! `set_simulated_today` (no env mutation).

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Duration, NaiveDate};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use tracing::{info, warn};

use crate::cache::ApiCache;
use crate::ingest::SeasonIngester;
use crate::ingest::bootstrap_csv;
use crate::{NatStatClient, notify, set_simulated_today};
use cstat_core::Database;
use cstat_core::invariants::{self, InvariantViolation, Severity};

/// Fixture rows are chunked into pages of this size, and `meta.results-max`
/// advertises it — it must equal the pagination stride `get_all_pages` uses
/// so seeded offsets (0, 100, 200, …) line up with the offsets requested.
const PAGE_SIZE: usize = 100;

/// Seeded fixtures never expire mid-run: ~10 years.
const FIXTURE_TTL_SECS: i64 = 10 * 365 * 24 * 3600;

#[derive(Debug, Clone)]
pub struct SimulateOptions {
    pub season: i32,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Days per simulated nightly window (1 = daily, 7 = weekly).
    pub step_days: i64,
    /// Drop + recreate the sim DB schema before replaying.
    pub reset: bool,
    /// Root of the CSV exports (season subdirectory resolved per year).
    pub csv_dir: PathBuf,
    /// The isolated sim database. Guarded against the main/prod URLs.
    pub database_url: String,
    /// Skip `compute_all` per window (ingest-only replay; invariants that
    /// depend on compute are skipped too).
    pub no_compute: bool,
}

/// Restores the process globals a sim run mutates — the simulated clock and
/// the notification mute — on drop, so every exit path (including early `?`
/// error returns) leaves the process in its real-clock, alerts-on state.
struct SimGlobalsGuard;

impl Drop for SimGlobalsGuard {
    fn drop(&mut self) {
        set_simulated_today(None);
        notify::set_suppressed(false);
    }
}

/// Per-window outcome for the final report.
struct WindowOutcome {
    start: NaiveDate,
    end: NaiveDate,
    games: u64,
    player_perfs: u64,
    team_perfs: u64,
    violations: Vec<InvariantViolation>,
}

/// Run the replay. Returns `Err` if any nightly hard-fails, any invariant is
/// violated, or the idempotency re-run drifts row counts — so the CLI exits
/// non-zero and a CI/weekly wrapper catches regressions.
pub async fn run(opts: SimulateOptions) -> Result<()> {
    if opts.from >= opts.to {
        bail!("--from {} must be before --to {}", opts.from, opts.to);
    }
    if !(1..=31).contains(&opts.step_days) {
        bail!("step must be 1..=31 days (got {})", opts.step_days);
    }
    assert_isolated(&opts.database_url)?;

    // Mute Slack / Cloudflare / heartbeat and take ownership of the simulated
    // clock for the whole process: a simulated nightly must never post to the
    // real alert channels, even with a fully configured operator .env. The
    // guard's Drop restores both globals on EVERY exit path — early `?`
    // returns included — so a failed sim can't leave a long-lived caller
    // (tests) with a pinned clock or muted alerts.
    notify::set_suppressed(true);
    let _globals = SimGlobalsGuard;

    info!(
        season = opts.season,
        from = %opts.from,
        to = %opts.to,
        step_days = opts.step_days,
        db = %opts.database_url,
        "starting offline season replay"
    );

    let db = Database::connect(&opts.database_url)
        .await
        .with_context(|| format!("connecting to sim DB {}", opts.database_url))?;
    if opts.reset {
        reset_schema(&db.pool).await?;
    }
    db.migrate().await.context("migrating sim DB")?;

    // --- season bootstrap premise: teams are reference data the nightly
    // never creates, so load them (and their seeded team_season_stats rows)
    // from Teams.csv up front, exactly like a real season bootstrap.
    let teams_csv = bootstrap_csv::find_csv(&opts.csv_dir, opts.season, "Teams")?;
    let (team_count, id_map) = bootstrap_csv::load_teams(&db.pool, opts.season, &teams_csv).await?;
    info!(count = team_count, "seeded teams from CSV");

    // --- load the box-score CSVs once; every window filters these in memory.
    let games_rows = load_dated_rows(&opts, "Games", 1)?;
    let teamperf_rows = load_dated_rows(&opts, "Team_Statlines", 0)?;
    let playerperf_rows = load_dated_rows(&opts, "Player_Statlines", 0)?;
    info!(
        games = games_rows.len(),
        team_statlines = teamperf_rows.len(),
        player_statlines = playerperf_rows.len(),
        "loaded season CSVs"
    );

    // --- season-wide fixtures ---
    let cache = ApiCache::new(db.pool.clone());
    let season_range = opts.season.to_string();
    // Preflight's NatStat probe: one non-empty teamcodes page.
    seed_page(
        &cache,
        "teamcodes",
        &season_range,
        0,
        teamcodes_payload(&id_map),
    )
    .await?;
    // Forecasts + ELO: well-formed EMPTY payloads — the run must survive them.
    seed_rows(&cache, "forecasts", &season_range, "forecasts", vec![]).await?;
    seed_rows(&cache, "elo", &season_range, "elo", vec![]).await?;

    // Bogus API key: a cache miss (fixture gap) surfaces as a loud auth
    // failure instead of a silent live call. The rate budget matches the env
    // default so the nightly's headroom check (which compares the client's
    // remaining tokens against `rate_budget_from_env`) doesn't false-trip a
    // degraded run — offline replay consumes zero tokens either way.
    let client = NatStatClient::new(
        db.pool.clone(),
        "SIM-OFFLINE-FIXTURES".into(),
        crate::rate_budget_from_env(),
    );
    let ingester = SeasonIngester::new(&client, &db.pool, opts.season);

    // --- windowed replay: each simulated nightly at date T covers
    // (T - step_days)..T, consecutive windows sharing a boundary day just
    // like the production yesterday..today window overlaps re-runs.
    let mut windows: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    {
        let mut start = opts.from;
        loop {
            let end = std::cmp::min(start + Duration::days(opts.step_days), opts.to);
            windows.push((start, end));
            if end >= opts.to {
                break;
            }
            start = end;
        }
    }

    let mut outcomes: Vec<WindowOutcome> = Vec::new();
    let mut hard_failures: Vec<String> = Vec::new();
    for &(window_start, window_end) in &windows {
        set_simulated_today(Some(window_end));

        let range = format!("{window_start},{window_end}");
        seed_window_fixtures(
            &cache,
            &range,
            &id_map,
            &games_rows,
            &teamperf_rows,
            &playerperf_rows,
            window_start,
            window_end,
        )
        .await?;

        info!(window = %range, simulated_today = %window_end, "replaying nightly");
        match ingester
            .nightly(
                &window_start.to_string(),
                &window_end.to_string(),
                !opts.no_compute,
            )
            .await
        {
            Ok(report) => {
                let violations = if opts.no_compute {
                    vec![]
                } else {
                    invariants::check_season(&db.pool, opts.season).await?
                };
                for v in &violations {
                    match v.severity {
                        Severity::Error => warn!(window = %range, "INVARIANT VIOLATED — {v}"),
                        Severity::Warning => info!(window = %range, "invariant warning — {v}"),
                    }
                }
                outcomes.push(WindowOutcome {
                    start: window_start,
                    end: window_end,
                    games: report.ingest.games,
                    player_perfs: report.ingest.player_performances,
                    team_perfs: report.ingest.team_performances,
                    violations,
                });
            }
            Err(e) => {
                warn!(window = %range, error = %e, "nightly HARD-FAILED");
                hard_failures.push(format!("{range}: {e}"));
            }
        }
    }

    // --- idempotency: re-run the final window; derived-table row counts must
    // not drift (the ON CONFLICT upsert paths are the idempotency story —
    // prove it end-to-end rather than assuming it).
    let mut idempotency_drift: Vec<String> = Vec::new();
    if let (true, Some(&(last_start, last_end))) = (hard_failures.is_empty(), windows.last()) {
        let before = table_counts(&db.pool).await?;
        set_simulated_today(Some(last_end));
        ingester
            .nightly(
                &last_start.to_string(),
                &last_end.to_string(),
                !opts.no_compute,
            )
            .await
            .map_err(|e| anyhow!("idempotency re-run hard-failed: {e}"))?;
        let after = table_counts(&db.pool).await?;
        for ((table, n_before), (_, n_after)) in before.iter().zip(after.iter()) {
            if n_before != n_after {
                idempotency_drift.push(format!("{table}: {n_before} → {n_after}"));
            }
        }
    }

    render_report(&opts, &outcomes, &hard_failures, &idempotency_drift);

    // Warnings (source-data gaps the pipeline faithfully reflects, e.g. a
    // game whose statlines the CSV export never contained) are reported but
    // don't fail the run — only Error-severity violations do.
    let violation_windows = outcomes
        .iter()
        .filter(|o| o.violations.iter().any(|v| v.severity == Severity::Error))
        .count();
    if !hard_failures.is_empty() || violation_windows > 0 || !idempotency_drift.is_empty() {
        bail!(
            "simulate FAILED: {} hard-failed window(s), {} window(s) with invariant violations, {} idempotency drift(s)",
            hard_failures.len(),
            violation_windows,
            idempotency_drift.len()
        );
    }
    info!("simulate PASSED: all windows clean");
    Ok(())
}

/// Refuse to run against the main or prod database. Compares
/// host/port/database triples parsed out of the URLs (so credential or
/// query-param differences can't sneak an alias through), with hosts
/// compared by **resolved IP overlap** — `localhost`, `127.0.0.1`, `::1`,
/// and a DNS name for the same machine must all count as the same host, or
/// a port typo plus `--reset` could `DROP SCHEMA` on the real database.
fn assert_isolated(sim_url: &str) -> Result<()> {
    let sim = PgConnectOptions::from_str(sim_url)
        .with_context(|| format!("invalid sim database URL: {sim_url}"))?;
    for (var, label) in [
        ("DATABASE_URL", "main (sync_to_prod source of truth)"),
        ("PROD_DATABASE_URL", "PROD"),
    ] {
        let Ok(other_url) = std::env::var(var) else {
            continue;
        };
        if other_url.trim().is_empty() {
            continue;
        }
        let Ok(other) = PgConnectOptions::from_str(&other_url) else {
            continue;
        };
        if urls_conflict(&sim, &other) {
            bail!(
                "refusing to simulate against the {label} database: the sim URL resolves to \
                 the same host/port/database as {var}. Point CSTAT_SIM_DATABASE_URL (or \
                 --database-url) at the isolated instance — e.g. \
                 `docker compose --profile sim up -d postgres-sim` then \
                 postgres://cstat:cstat@localhost:5433/cstat_sim"
            );
        }
    }
    Ok(())
}

/// Two connect targets collide when port and database match and the hosts
/// are the same machine (textually, or by any shared resolved IP).
fn urls_conflict(sim: &PgConnectOptions, other: &PgConnectOptions) -> bool {
    sim.get_port() == other.get_port()
        && sim.get_database() == other.get_database()
        && same_host(sim.get_host(), other.get_host())
}

/// Host equality with alias handling: exact (case-insensitive) match, or a
/// non-empty intersection of resolved IPs. Resolution failures fall back to
/// the textual comparison — the guard errs toward refusing only what it can
/// prove, but `localhost`/`127.0.0.1`/`::1` always resolve locally. IPv6
/// URL brackets (`[::1]`) are stripped before comparing/resolving.
fn same_host(a: &str, b: &str) -> bool {
    let a = a.trim_start_matches('[').trim_end_matches(']');
    let b = b.trim_start_matches('[').trim_end_matches(']');
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let ips_a = resolve_host(a);
    let ips_b = resolve_host(b);
    !ips_a.is_empty() && ips_a.iter().any(|ip| ips_b.contains(ip))
}

fn resolve_host(host: &str) -> Vec<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    (host, 0u16)
        .to_socket_addrs()
        .map(|addrs| addrs.map(|sa| sa.ip()).collect())
        .unwrap_or_default()
}

/// Drop and recreate the `public` schema — returns the sim DB to the same
/// blank state a fresh container has, so `migrate()` rebuilds everything
/// (including `_sqlx_migrations`).
async fn reset_schema(pool: &PgPool) -> Result<()> {
    info!("resetting sim DB schema");
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("CREATE SCHEMA public").execute(pool).await?;
    Ok(())
}

/// Read one CSV kind into `(game_date, record)` rows, sorted by date.
/// `date_idx` is the column holding the `YYYY-MM-DD` day (Games=1, both
/// Statlines=0). Undated rows are dropped with a warning tally.
fn load_dated_rows(
    opts: &SimulateOptions,
    kind: &str,
    date_idx: usize,
) -> Result<Vec<(NaiveDate, csv::StringRecord)>> {
    let path = bootstrap_csv::find_csv(&opts.csv_dir, opts.season, kind)?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(std::fs::File::open(&path).with_context(|| format!("open {path:?}"))?);
    let mut rows = Vec::new();
    let mut undated = 0u64;
    for rec in rdr.records() {
        let rec = rec?;
        match NaiveDate::parse_from_str(rec.get(date_idx).unwrap_or("").trim(), "%Y-%m-%d") {
            Ok(d) => rows.push((d, rec)),
            Err(_) => undated += 1,
        }
    }
    if undated > 0 {
        warn!(kind, undated, "CSV rows dropped: unparseable date");
    }
    rows.sort_by_key(|(d, _)| *d);
    Ok(rows)
}

/// Seed the three date-range endpoints for one nightly window. The range
/// string must match `ingest_*_by_date_range`'s `format!("{start},{end}")`
/// exactly — that's the cache key the client will look up.
#[allow(clippy::too_many_arguments)]
async fn seed_window_fixtures(
    cache: &ApiCache,
    range: &str,
    id_map: &HashMap<String, String>,
    games: &[(NaiveDate, csv::StringRecord)],
    teamperfs: &[(NaiveDate, csv::StringRecord)],
    playerperfs: &[(NaiveDate, csv::StringRecord)],
    start: NaiveDate,
    end: NaiveDate,
) -> Result<()> {
    let in_window = |rows: &[(NaiveDate, csv::StringRecord)]| -> Vec<csv::StringRecord> {
        rows.iter()
            .filter(|(d, _)| *d >= start && *d <= end)
            .map(|(_, r)| r.clone())
            .collect()
    };

    let game_objs: Vec<Value> = in_window(games)
        .iter()
        .map(|r| game_json(r, id_map))
        .collect();
    let teamperf_objs: Vec<Value> = in_window(teamperfs)
        .iter()
        .map(|r| teamperf_json(r, id_map))
        .collect();
    let playerperf_objs: Vec<Value> = in_window(playerperfs)
        .iter()
        .map(|r| playerperf_json(r, id_map))
        .collect();

    info!(
        range,
        games = game_objs.len(),
        teamperfs = teamperf_objs.len(),
        playerperfs = playerperf_objs.len(),
        "seeding window fixtures"
    );

    seed_rows(cache, "games", range, "games", game_objs).await?;
    seed_rows(cache, "teamperfs", range, "teamperfs", teamperf_objs).await?;
    seed_rows(cache, "playerperfs", range, "playerperfs", playerperf_objs).await?;
    Ok(())
}

/// Seed a full paginated response set: 100-row pages at offsets 0, 100, …
/// plus the **terminal empty page** — `get_all_pages` only stops on an empty
/// payload, and a missing terminal page would fall through to live HTTP.
async fn seed_rows(
    cache: &ApiCache,
    endpoint: &str,
    range: &str,
    data_key: &str,
    rows: Vec<Value>,
) -> Result<()> {
    let mut offset = 0u64;
    for chunk in rows.chunks(PAGE_SIZE) {
        let mut data = Map::new();
        for (i, obj) in chunk.iter().enumerate() {
            data.insert(format!("row_{}", offset as usize + i), obj.clone());
        }
        seed_page(cache, endpoint, range, offset, envelope(data_key, data)).await?;
        offset += PAGE_SIZE as u64;
    }
    // Terminal empty page (also the whole response for a zero-row window).
    seed_page(
        cache,
        endpoint,
        range,
        offset,
        envelope(data_key, Map::new()),
    )
    .await?;
    Ok(())
}

/// Upsert one response page into `api_cache` under the exact key
/// `NatStatClient::get_inner` computes.
async fn seed_page(
    cache: &ApiCache,
    endpoint: &str,
    range: &str,
    offset: u64,
    body: Value,
) -> Result<()> {
    let key = NatStatClient::cache_key(endpoint, Some(range), Some(offset));
    cache
        .set(&key, &key, &body, FIXTURE_TTL_SECS)
        .await
        .with_context(|| format!("seeding api_cache fixture {key}"))?;
    Ok(())
}

/// NatStat-v4 response envelope: `success` + the meta the client reads
/// (`results-max` is load-bearing — it's the pagination stride).
fn envelope(data_key: &str, data: Map<String, Value>) -> Value {
    json!({
        "success": "1",
        "meta": { "results-max": PAGE_SIZE },
        data_key: Value::Object(data),
    })
}

/// Minimal teamcodes payload for the preflight probe (it only checks the
/// call succeeds; the payload content is never parsed).
fn teamcodes_payload(id_map: &HashMap<String, String>) -> Value {
    let mut data = Map::new();
    for (numeric, abbrev) in id_map.iter().take(PAGE_SIZE) {
        data.insert(
            format!("team_{numeric}"),
            json!({ "code": abbrev, "id": numeric }),
        );
    }
    envelope("teamcodes", data)
}

// ---------------------------------------------------------------------------
// CSV row → NatStat-v4 JSON object synthesis.
//
// Field names must match what games.rs's upserts read — key divergences that
// are easy to get wrong: playerperfs carry a flat stat layout with the home
// flag at `game.loc` and the game id at `game-code`; teamperfs nest stats
// under `stats` with the home flag at `game.location` and the game id at
// `game.id`. Stat cells pass through as strings — the parse helpers accept
// string-encoded numbers, matching NatStat v3's encoding. The `cell`/`pct`
// CSV helpers are shared with `bootstrap_csv` so both consumers of these
// files parse identically.
// ---------------------------------------------------------------------------

use bootstrap_csv::{cell, parse_i32, pct};

/// Insert `key: value` only when the CSV cell is non-empty (missing keys →
/// NULL columns, same as a real API payload omitting the field).
fn put(map: &mut Map<String, Value>, key: &str, cell: &str) {
    if !cell.is_empty() {
        map.insert(key.to_string(), Value::String(cell.to_string()));
    }
}

/// (made, attempts) → 0–100-scale percentage, matching the API's
/// `fgpct`-style fields (the JSON ingest path stores them as-is and never
/// derives them from makes/attempts, so the fixture must supply them).
fn pct_cell(row: &csv::StringRecord, made_idx: usize, att_idx: usize) -> Option<f64> {
    pct(
        parse_i32(cell(row, made_idx)),
        parse_i32(cell(row, att_idx)),
    )
}

/// Games.csv: ID(0) GameDay(1) GameTime(2) Home(3) HomeID(4) Visitor(5)
/// VisitorID(6) ScoreVis(7) ScoreHome(8) GameStatus(9) Venue(10)
/// CityState(11) Neutral(12) Division(13) Conference(14) Playoffs(15).
fn game_json(row: &csv::StringRecord, id_map: &HashMap<String, String>) -> Value {
    let mut m = Map::new();
    put(&mut m, "id", cell(row, 0));
    put(&mut m, "gameday", cell(row, 1));
    if let Some(code) = id_map.get(cell(row, 4)) {
        put(&mut m, "home-code", code);
    }
    if let Some(code) = id_map.get(cell(row, 6)) {
        put(&mut m, "visitor-code", code);
    }
    put(&mut m, "score-home", cell(row, 8));
    put(&mut m, "score-vis", cell(row, 7));
    put(&mut m, "gamestatus", cell(row, 9));
    put(&mut m, "venue", cell(row, 10));
    put(&mut m, "neutral", cell(row, 12));
    put(&mut m, "postseason", cell(row, 15));
    Value::Object(m)
}

/// Team_Statlines.csv: GameDay(0) GameID(1) TeamID(2) Team(3) OpponentID(4)
/// Opponent(5) Location(6) Division(7) Conference(8) Playoffs(9)
/// WinOrLoss(10) PlayerType(11) MIN(12) PTS(13) FGM(14) FGA(15) 3FM(16)
/// 3FA(17) FTM(18) FTA(19) REB(20) AST(21) STL(22) BLK(23) OREB(24) TO(25)
/// PF(26).
fn teamperf_json(row: &csv::StringRecord, id_map: &HashMap<String, String>) -> Value {
    let mut m = Map::new();
    if let Some(code) = id_map.get(cell(row, 2)) {
        put(&mut m, "team-code", code);
    }

    let mut game = Map::new();
    put(&mut game, "id", cell(row, 1));
    put(&mut game, "location", cell(row, 6));
    put(&mut game, "winorloss", cell(row, 10));
    m.insert("game".into(), Value::Object(game));

    if let Some(code) = id_map.get(cell(row, 4)) {
        m.insert("opponent".into(), json!({ "code": code }));
    }

    let mut stats = Map::new();
    for (key, idx) in [
        ("min", 12),
        ("pts", 13),
        ("fgm", 14),
        ("fga", 15),
        ("threefm", 16),
        ("threefa", 17),
        ("ftm", 18),
        ("fta", 19),
        ("reb", 20),
        ("ast", 21),
        ("stl", 22),
        ("blk", 23),
        ("oreb", 24),
        ("to", 25),
        ("pf", 26),
    ] {
        put(&mut stats, key, cell(row, idx));
    }
    m.insert("stats".into(), Value::Object(stats));
    Value::Object(m)
}

/// Player_Statlines.csv: GameDay(0) GameID(1) Player(2) PlayerID(3)
/// PlayerCode(4) TeamID(5) Team(6) OpponentID(7) Opponent(8) Location(9)
/// Division(10) Conference(11) Playoffs(12) WinOrLoss(13) Starter(14)
/// PlayerType(15) PerfScore(16) MIN(17) PTS(18) FGM(19) FGA(20) 3FM(21)
/// 3FA(22) FTM(23) FTA(24) REB(25) AST(26) STL(27) BLK(28) OREB(29) TO(30)
/// PF(31). `player-code` is the numeric PlayerID (col 3), NOT the slug.
fn playerperf_json(row: &csv::StringRecord, id_map: &HashMap<String, String>) -> Value {
    let mut m = Map::new();
    put(&mut m, "player-code", cell(row, 3));
    put(&mut m, "game-code", cell(row, 1));
    if let Some(code) = id_map.get(cell(row, 5)) {
        put(&mut m, "team-code", code);
    }
    put(&mut m, "player", cell(row, 2));
    put(&mut m, "starter", cell(row, 14));
    put(&mut m, "perfscore", cell(row, 16));

    m.insert("game".into(), json!({ "loc": cell(row, 9) }));
    if let Some(code) = id_map.get(cell(row, 7)) {
        m.insert("opponent".into(), json!({ "code": code }));
    }

    for (key, idx) in [
        ("min", 17),
        ("pts", 18),
        ("fgm", 19),
        ("fga", 20),
        ("threefm", 21),
        ("threefa", 22),
        ("ftm", 23),
        ("fta", 24),
        ("reb", 25),
        ("ast", 26),
        ("stl", 27),
        ("blk", 28),
        ("oreb", 29),
        ("to", 30),
        ("pf", 31),
    ] {
        put(&mut m, key, cell(row, idx));
    }

    // The API path stores shooting percentages verbatim and never derives
    // them — synthesize on the 0–100 scale like NatStat serves them.
    for (key, made, att) in [("fgpct", 19, 20), ("threefgpct", 21, 22), ("ftpct", 23, 24)] {
        if let Some(p) = pct_cell(row, made, att) {
            m.insert(key.into(), json!(p));
        }
    }

    Value::Object(m)
}

/// Row counts of the tables the nightly writes — compared before/after the
/// idempotency re-run.
async fn table_counts(pool: &PgPool) -> Result<Vec<(&'static str, i64)>> {
    let mut counts = Vec::new();
    for table in [
        "teams",
        "players",
        "games",
        "team_game_stats",
        "player_game_stats",
        "team_season_stats",
        "player_season_stats",
        "game_forecasts",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await?;
        counts.push((table, n));
    }
    Ok(counts)
}

/// Human-readable end-of-run report (also the artifact a weekly wrapper can
/// grep).
fn render_report(
    opts: &SimulateOptions,
    outcomes: &[WindowOutcome],
    hard_failures: &[String],
    idempotency_drift: &[String],
) {
    println!(
        "\n=== simulate report: season {} · {} → {} · {}-day windows ===",
        opts.season, opts.from, opts.to, opts.step_days
    );
    println!(
        "{:<25} {:>7} {:>12} {:>10}  invariants",
        "window", "games", "player_perfs", "team_perfs"
    );
    for o in outcomes {
        println!(
            "{:<25} {:>7} {:>12} {:>10}  {}",
            format!("{}..{}", o.start, o.end),
            o.games,
            o.player_perfs,
            o.team_perfs,
            if o.violations.is_empty() {
                "clean".to_string()
            } else {
                o.violations
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        );
    }
    if !hard_failures.is_empty() {
        println!("\nHARD FAILURES:");
        for f in hard_failures {
            println!("  {f}");
        }
    }
    match idempotency_drift.is_empty() {
        true => println!("\nidempotency re-run: clean (no row-count drift)"),
        false => {
            println!("\nIDEMPOTENCY DRIFT (final-window re-run changed row counts):");
            for d in idempotency_drift {
                println!("  {d}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(url: &str) -> PgConnectOptions {
        PgConnectOptions::from_str(url).unwrap()
    }

    #[test]
    fn guard_catches_loopback_aliases() {
        // localhost vs 127.0.0.1 vs ::1 are the same machine — a port typo
        // must not slip past the guard on a hostname-spelling difference.
        for (a, b) in [
            ("localhost", "localhost"),
            ("localhost", "127.0.0.1"),
            ("127.0.0.1", "localhost"),
            ("localhost", "[::1]"),
        ] {
            assert!(
                urls_conflict(
                    &opts(&format!("postgres://cstat:cstat@{a}:5432/cstat")),
                    &opts(&format!("postgres://cstat:cstat@{b}:5432/cstat")),
                ),
                "{a} vs {b} on same port+db should conflict"
            );
        }
    }

    #[test]
    fn guard_allows_genuinely_isolated_targets() {
        let main = opts("postgres://cstat:cstat@localhost:5432/cstat");
        // Different port (the compose sim service).
        assert!(!urls_conflict(
            &opts("postgres://cstat:cstat@127.0.0.1:5433/cstat_sim"),
            &main
        ));
        // Same port, different database.
        assert!(!urls_conflict(
            &opts("postgres://cstat:cstat@localhost:5432/cstat_sim"),
            &main
        ));
    }

    #[test]
    fn guard_ignores_credential_and_param_differences() {
        // Same host/port/db with different creds or query params still
        // conflicts — the triple is what identifies the database.
        assert!(urls_conflict(
            &opts("postgres://other:pw@127.0.0.1:5432/cstat?sslmode=disable"),
            &opts("postgres://cstat:cstat@localhost:5432/cstat"),
        ));
    }
}
