//! Offline ingest -> compute -> invariants replay tests, green in CI on every push.
//!
//! M4(c): the always-on regression net for the ingest -> compute core — seed a
//! tiny hand-authored NatStat-v4 box-score corpus into a throwaway database's
//! `api_cache`, replay it through the real by-date-range ingest fns +
//! `compute_all`, and assert the post-compute invariant gates are clean and a
//! second replay is idempotent.
//!
//! M4(d): edge-case smokes that a full-season replay doesn't isolate —
//! postponed -> final score overwrite, a cancelled game writing no phantom box
//! rows, and a conference re-classification recomputing `is_conference`.
//!
//! All tests are deliberately offline: they drive only the box-score ingest +
//! compute (not full `nightly`), so there is no live NatStat / Torvik / ELO /
//! forecasts network on every push. A bogus API key means any fixture gap
//! surfaces as a loud auth failure, not a silent live call. The seeding + ingest
//! code paths are shared with the `simulate` harness (`seed_window_objects` /
//! `ingest_box_score_window` / `table_counts`) so the two can't drift.
//!
//! Isolation & safety: each test CREATEs and DROPs its OWN uniquely-named
//! database (so the parallel test runner can't collide them, and nothing ever
//! touches whatever `DATABASE_URL` points at — a developer's real local DB or
//! CI's `cstat_test`). Every test skips cleanly when `DATABASE_URL` is unset so
//! a plain `cargo test` still passes with no Postgres available.

use cstat_core::Database;
use cstat_core::compute::compute_all;
use cstat_core::invariants::{self, Severity};
use cstat_ingest::cache::ApiCache;
use cstat_ingest::client::NatStatClient;
use cstat_ingest::simulate::{ingest_box_score_window, seed_window_objects, table_counts};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

/// Fictitious season, far from any real ingested year, so nothing about the
/// fixtures can be confused with real data even if isolation ever slipped. Each
/// test owns its database, so they can all reuse the same season number.
const SEASON: i32 = 9001;
const FROM: &str = "2027-11-03";
const TO: &str = "2027-11-04";

/// A throwaway database that drops itself. Bootstraps a fresh DB on the same
/// server `DATABASE_URL` names, migrates it, and drops it on [`Self::cleanup`].
/// `name` must be UNIQUE per test — the test runner executes `#[tokio::test]`s
/// on parallel threads, so a shared name would race on CREATE/DROP.
struct IsolatedDb {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl IsolatedDb {
    /// Returns `None` when `DATABASE_URL` is unset (no Postgres) so the caller
    /// can skip; otherwise CREATEs + migrates a fresh database.
    async fn setup(name: &str) -> Option<Self> {
        let base_url = std::env::var("DATABASE_URL").ok()?;
        let base_opts = PgConnectOptions::from_str(&base_url).expect("valid DATABASE_URL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(base_opts.clone())
            .await
            .expect("connect to base database");
        // WITH (FORCE) terminates any leftover connection from a prior panicked
        // run (PG13+); ignore the error when the DB doesn't exist yet.
        let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .execute(&admin)
            .await;
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .expect("create throwaway test database");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(base_opts.database(name))
            .await
            .expect("connect to throwaway database");
        Database { pool: pool.clone() }
            .migrate()
            .await
            .expect("migrate throwaway database");
        Some(Self {
            admin,
            name: name.to_string(),
            pool,
        })
    }

    /// Drop the throwaway database. Call after capturing results but before any
    /// assertion, so a failing assertion can't leak the database.
    async fn cleanup(self) {
        self.pool.close().await;
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&self.admin)
        .await;
        self.admin.close().await;
    }
}

/// Insert reference teams (the box-score ingest resolves teams, never creates
/// them — a real season bootstrap seeds them first). `conference` NULL unless
/// the test needs it.
async fn seed_teams(pool: &PgPool, teams: &[(&str, &str)]) -> anyhow::Result<()> {
    for (code, name) in teams {
        sqlx::query(
            "INSERT INTO teams (natstat_id, name, short_name, season) VALUES ($1, $2, $2, $3)",
        )
        .bind(code)
        .bind(name)
        .bind(SEASON)
        .execute(pool)
        .await?;
    }
    Ok(())
}

fn test_client(pool: &PgPool) -> NatStatClient {
    // Bogus key: any un-seeded fetch fails loudly rather than going live.
    NatStatClient::new(pool.clone(), "TEST-OFFLINE-M4".into(), 2500)
}

// ===========================================================================
// M4(c) — full ingest -> compute -> invariants replay + idempotency.
// ===========================================================================

/// 4 teams in a full round-robin (6 games) so the AdjEM solver sees a fully
/// connected schedule and rates every team. A NULL AdjEM on a solver-eligible
/// team is an Error-severity invariant, so a sparse/disconnected schedule would
/// (correctly) fail the gate — the round-robin is what makes every team
/// solver-eligible AND rateable.
const TEAMS: [(&str, &str); 4] = [
    ("TA", "Team Alpha"),
    ("TB", "Team Bravo"),
    ("TC", "Team Charlie"),
    ("TD", "Team Delta"),
];

/// (home_code, away_code, home_score, away_score). Every pair plays once.
const GAMES: [(&str, &str, i32, i32); 6] = [
    ("TA", "TB", 80, 72),
    ("TA", "TC", 75, 68),
    ("TA", "TD", 90, 61),
    ("TB", "TC", 70, 66),
    ("TB", "TD", 64, 77),
    ("TC", "TD", 55, 59),
];

struct ReplayReport {
    ingest_counts: (u64, u64, u64),
    error_violations: Vec<String>,
    idempotency_drift: Vec<String>,
}

#[tokio::test]
async fn offline_ingest_replay_is_clean_and_idempotent() {
    let Some(db) = IsolatedDb::setup("cstat_ingest_replay_m4c").await else {
        eprintln!("SKIP offline_ingest_replay: DATABASE_URL unset (no Postgres available)");
        return;
    };
    let outcome = replay_clean_body(&db.pool).await;
    db.cleanup().await;

    let report = outcome.expect("replay body failed");
    assert_eq!(
        report.ingest_counts,
        (6, 36, 12),
        "expected 6 games / 36 player perfs / 12 team perfs ingested — a zero or \
         wrong count means the fixtures never reached the ingest path"
    );
    assert!(
        report.error_violations.is_empty(),
        "post-compute invariant gate found Error-severity violations: {:?}",
        report.error_violations
    );
    assert!(
        report.idempotency_drift.is_empty(),
        "second replay was not idempotent — row-count drift: {:?}",
        report.idempotency_drift
    );
}

async fn replay_clean_body(pool: &PgPool) -> anyhow::Result<ReplayReport> {
    seed_teams(pool, &TEAMS).await?;

    let (games, teamperfs, playerperfs) = build_fixtures();
    let range = format!("{FROM},{TO}");
    let cache = ApiCache::new(pool.clone());
    seed_window_objects(&cache, &range, games, teamperfs, playerperfs).await?;

    let client = test_client(pool);

    // --- first replay: ingest + compute + invariants ---
    let ingest_counts = ingest_box_score_window(&client, pool, SEASON, FROM, TO).await?;
    compute_all(pool, SEASON).await?;

    let error_violations = error_violations(pool).await?;

    // --- idempotency: re-run the same window; derived-table counts must hold ---
    let before = table_counts(pool).await?;
    ingest_box_score_window(&client, pool, SEASON, FROM, TO).await?;
    compute_all(pool, SEASON).await?;
    let after = table_counts(pool).await?;
    let idempotency_drift: Vec<String> = before
        .iter()
        .zip(after.iter())
        .filter(|((_, b), (_, a))| b != a)
        .map(|((t, b), (_, a))| format!("{t}: {b} -> {a}"))
        .collect();

    Ok(ReplayReport {
        ingest_counts,
        error_violations,
        idempotency_drift,
    })
}

// ===========================================================================
// M4(d) — edge-case smokes.
// ===========================================================================

/// A game first seen postponed (no score) must pick up its final score when a
/// later nightly window re-ingests it — the `COALESCE(EXCLUDED.home_score,
/// games.home_score)` upsert has to overwrite the NULL, and the same natstat_id
/// must not duplicate the row.
#[tokio::test]
async fn postponed_game_gets_final_score_on_re_ingest() {
    let Some(db) = IsolatedDb::setup("cstat_ingest_replay_m4d_postponed").await else {
        eprintln!("SKIP postponed_game: DATABASE_URL unset");
        return;
    };
    let outcome = postponed_body(&db.pool).await;
    db.cleanup().await;

    let (postponed_score, final_score, row_count) = outcome.expect("postponed body failed");
    assert_eq!(
        postponed_score, None,
        "a postponed game must ingest with a NULL score"
    );
    assert_eq!(
        final_score,
        Some(80),
        "re-ingesting the finalized game must overwrite the NULL score"
    );
    assert_eq!(
        row_count, 1,
        "the same natstat_id must not create a second row"
    );
}

async fn postponed_body(pool: &PgPool) -> anyhow::Result<(Option<i32>, Option<i32>, i64)> {
    seed_teams(pool, &[("TA", "Team Alpha"), ("TB", "Team Bravo")]).await?;
    let client = test_client(pool);
    let cache = ApiCache::new(pool.clone());

    // Window 1: the game is postponed — present in the feed but with no score.
    let postponed = json!({
        "id": "PG1", "gameday": "2027-11-03",
        "home-code": "TA", "visitor-code": "TB",
        "gamestatus": "Postponed", "neutral": "N", "postseason": "N",
    });
    seed_window_objects(
        &cache,
        "2027-11-03,2027-11-03",
        vec![postponed],
        vec![],
        vec![],
    )
    .await?;
    ingest_box_score_window(&client, pool, SEASON, "2027-11-03", "2027-11-03").await?;
    let postponed_score: Option<i32> =
        sqlx::query_scalar("SELECT home_score FROM games WHERE natstat_id = 'PG1'")
            .fetch_one(pool)
            .await?;

    // Window 2 (a later nightly): the rescheduled game is now final with a score.
    let finalized = game_obj("PG1", "TA", "TB", 80, 72);
    seed_window_objects(
        &cache,
        "2027-11-10,2027-11-10",
        vec![finalized],
        vec![],
        vec![],
    )
    .await?;
    ingest_box_score_window(&client, pool, SEASON, "2027-11-10", "2027-11-10").await?;
    let final_score: Option<i32> =
        sqlx::query_scalar("SELECT home_score FROM games WHERE natstat_id = 'PG1'")
            .fetch_one(pool)
            .await?;
    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM games WHERE natstat_id = 'PG1'")
        .fetch_one(pool)
        .await?;

    Ok((postponed_score, final_score, row_count))
}

/// A cancelled game (present in the feed, no score, no box statlines) must not
/// fabricate any `team_game_stats`, and — because it never completed — must not
/// trip the `completed_game_missing_team_stats` gate or any other invariant.
#[tokio::test]
async fn cancelled_game_writes_no_phantom_row() {
    let Some(db) = IsolatedDb::setup("cstat_ingest_replay_m4d_cancelled").await else {
        eprintln!("SKIP cancelled_game: DATABASE_URL unset");
        return;
    };
    let outcome = cancelled_body(&db.pool).await;
    db.cleanup().await;

    let (team_stat_rows, error_violations) = outcome.expect("cancelled body failed");
    assert_eq!(
        team_stat_rows, 0,
        "a cancelled game with no statlines must not write any team_game_stats"
    );
    assert!(
        error_violations.is_empty(),
        "a cancelled game must not trip an invariant gate: {error_violations:?}"
    );
}

async fn cancelled_body(pool: &PgPool) -> anyhow::Result<(i64, Vec<String>)> {
    seed_teams(pool, &[("TA", "Team Alpha"), ("TB", "Team Bravo")]).await?;
    let client = test_client(pool);
    let cache = ApiCache::new(pool.clone());

    // Cancelled game: no score, and crucially no team statlines seeded for it.
    let cancelled = json!({
        "id": "CG1", "gameday": "2027-11-03",
        "home-code": "TA", "visitor-code": "TB",
        "gamestatus": "Cancelled", "neutral": "N", "postseason": "N",
    });
    seed_window_objects(
        &cache,
        "2027-11-03,2027-11-03",
        vec![cancelled],
        vec![],
        vec![],
    )
    .await?;
    ingest_box_score_window(&client, pool, SEASON, "2027-11-03", "2027-11-03").await?;
    compute_all(pool, SEASON).await?;

    let team_stat_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_game_stats")
        .fetch_one(pool)
        .await?;
    Ok((team_stat_rows, error_violations(pool).await?))
}

/// `compute_derived_game_fields` derives `is_conference` unconditionally from
/// the two teams' (Torvik-corrected) conferences, so a conference realignment
/// must flip the flag on the next compute — the exact case that mislabeled
/// Utah's post-2025 Big 12 games until this became unconditional.
#[tokio::test]
async fn conference_reclass_recomputes_is_conference() {
    let Some(db) = IsolatedDb::setup("cstat_ingest_replay_m4d_conference").await else {
        eprintln!("SKIP conference_reclass: DATABASE_URL unset");
        return;
    };
    let outcome = conference_body(&db.pool).await;
    db.cleanup().await;

    let (same_conf, after_reclass) = outcome.expect("conference body failed");
    assert_eq!(
        same_conf,
        Some(true),
        "a game between two same-conference teams must be flagged is_conference"
    );
    assert_eq!(
        after_reclass,
        Some(false),
        "moving one team to a different conference must recompute is_conference to false"
    );
}

async fn conference_body(pool: &PgPool) -> anyhow::Result<(Option<bool>, Option<bool>)> {
    // Both teams start in the same conference.
    sqlx::query(
        "INSERT INTO teams (natstat_id, name, short_name, season, conference) VALUES \
         ('TA', 'Team Alpha', 'Team Alpha', $1, 'TestConf'), \
         ('TB', 'Team Bravo', 'Team Bravo', $1, 'TestConf')",
    )
    .bind(SEASON)
    .execute(pool)
    .await?;

    let client = test_client(pool);
    let cache = ApiCache::new(pool.clone());
    let game = game_obj("CFG1", "TA", "TB", 80, 72);
    let home = teamperf_obj("CFG1", "TA", "TB", "H", true, 80);
    let away = teamperf_obj("CFG1", "TB", "TA", "A", false, 72);
    seed_window_objects(
        &cache,
        "2027-11-03,2027-11-03",
        vec![game],
        vec![home, away],
        vec![],
    )
    .await?;
    ingest_box_score_window(&client, pool, SEASON, "2027-11-03", "2027-11-03").await?;
    compute_all(pool, SEASON).await?;
    let same_conf: Option<bool> =
        sqlx::query_scalar("SELECT is_conference FROM games WHERE natstat_id = 'CFG1'")
            .fetch_one(pool)
            .await?;

    // Realign one team, then recompute — is_conference must follow.
    sqlx::query(
        "UPDATE teams SET conference = 'OtherConf' WHERE natstat_id = 'TB' AND season = $1",
    )
    .bind(SEASON)
    .execute(pool)
    .await?;
    compute_all(pool, SEASON).await?;
    let after_reclass: Option<bool> =
        sqlx::query_scalar("SELECT is_conference FROM games WHERE natstat_id = 'CFG1'")
            .fetch_one(pool)
            .await?;

    Ok((same_conf, after_reclass))
}

// ===========================================================================
// Shared helpers.
// ===========================================================================

/// Error-severity invariant violations for the fictitious season, as strings.
async fn error_violations(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    Ok(invariants::check_season(pool, SEASON)
        .await?
        .into_iter()
        .filter(|v| v.severity == Severity::Error)
        .map(|v| v.to_string())
        .collect())
}

/// Build `(games, teamperfs, playerperfs)` NatStat-v4 objects for the M4(c)
/// round-robin. Field shapes mirror `simulate::{game_json, teamperf_json,
/// playerperf_json}` — the same fields the ingest upserts read.
fn build_fixtures() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut games = Vec::new();
    let mut teamperfs = Vec::new();
    let mut playerperfs = Vec::new();

    for (i, &(home, away, hs, as_score)) in GAMES.iter().enumerate() {
        let gid = format!("G{i}");
        games.push(game_obj(&gid, home, away, hs, as_score));

        // Both sides' team box rows (required, else `completed_game_missing_
        // team_stats` flags and four-factors/AdjEM/W-L skew).
        teamperfs.push(teamperf_obj(&gid, home, away, "H", hs > as_score, hs));
        teamperfs.push(teamperf_obj(&gid, away, home, "A", as_score > hs, as_score));

        // Three players per side, from a fixed 3-man roster per team so each
        // player accumulates multiple games (exercises season aggregation +
        // team reconciliation), splitting the team's points.
        for (team, opp, loc, pts) in [(home, away, "H", hs), (away, home, "A", as_score)] {
            let split = [pts / 2, pts / 3, pts - pts / 2 - pts / 3];
            for (slot, &slot_pts) in split.iter().enumerate() {
                playerperfs.push(playerperf_obj(
                    &gid,
                    team,
                    opp,
                    loc,
                    &format!("{team}{slot}"),
                    &format!("{team} Player {slot}"),
                    slot_pts,
                ));
            }
        }
    }

    (games, teamperfs, playerperfs)
}

fn game_obj(gid: &str, home: &str, away: &str, hs: i32, as_score: i32) -> Value {
    json!({
        "id": gid,
        "gameday": FROM,
        "home-code": home,
        "visitor-code": away,
        "score-home": hs.to_string(),
        "score-vis": as_score.to_string(),
        "gamestatus": "Final",
        "neutral": "N",
        "postseason": "N",
    })
}

fn teamperf_obj(gid: &str, team: &str, opp: &str, loc: &str, win: bool, pts: i32) -> Value {
    // Internally consistent box: possessions = fga - oreb + to + 0.44*fta > 0
    // (so the team is AdjEM-solver-eligible), reb > oreb (so total_rebounds
    // isn't treated as the missing-data sentinel).
    let fgm = ((pts as f64) * 0.38).round() as i32;
    json!({
        "team-code": team,
        "game": { "id": gid, "location": loc, "winorloss": if win { "W" } else { "L" } },
        "opponent": { "code": opp },
        "stats": {
            "min": "200",
            "pts": pts.to_string(),
            "fgm": fgm.to_string(),
            "fga": "60",
            "threefm": "8",
            "threefa": "22",
            "ftm": "12",
            "fta": "16",
            "reb": "34",
            "ast": "14",
            "stl": "6",
            "blk": "3",
            "oreb": "10",
            "to": "11",
            "pf": "18",
        },
    })
}

fn playerperf_obj(
    gid: &str,
    team: &str,
    opp: &str,
    loc: &str,
    pcode: &str,
    pname: &str,
    pts: i32,
) -> Value {
    let fgm = ((pts as f64) * 0.4).round() as i32;
    let fga = (fgm + 4).max(1);
    json!({
        "player-code": pcode,
        "game-code": gid,
        "team-code": team,
        "player": pname,
        "starter": "Y",
        "game": { "loc": loc },
        "opponent": { "code": opp },
        "min": "24",
        "pts": pts.to_string(),
        "fgm": fgm.to_string(),
        "fga": fga.to_string(),
        "threefm": "1",
        "threefa": "4",
        "ftm": "2",
        "fta": "2",
        "reb": "5",
        "ast": "3",
        "stl": "1",
        "blk": "0",
        "oreb": "1",
        "to": "2",
        "pf": "2",
    })
}
