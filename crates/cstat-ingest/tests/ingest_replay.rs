//! M4(c) — offline ingest -> compute -> invariants replay, green in CI on every push.
//!
//! Seeds a tiny hand-authored NatStat-v4 box-score corpus into a THROWAWAY
//! database's `api_cache`, replays it through the real by-date-range ingest
//! functions + `compute_all`, and asserts the post-compute invariant gates are
//! clean and a second replay is idempotent. This is the always-on regression
//! net for the ingest -> compute core, complementing the local-only `simulate`
//! harness (which replays full real seasons from the 7.9 GB uncommitted CSVs).
//!
//! Deliberately offline: it drives only the box-score ingest + `compute_all`,
//! not the full `nightly` orchestrator, so there is no live NatStat / Torvik /
//! ELO / forecasts network on every push (those feeds are the operational
//! nightly's concern; here we prove the deterministic ingest core). A bogus API
//! key means any fixture gap surfaces as a loud auth failure, not a silent live
//! call. The seeding + ingest code paths are shared with the `simulate` harness
//! (`seed_window_objects` / `ingest_box_score_window`) so the two can't drift.
//!
//! Isolation & safety: the test CREATEs and DROPs its OWN database, so it never
//! touches whatever `DATABASE_URL` points at — a developer's real local DB or
//! CI's `cstat_test`. It skips cleanly when `DATABASE_URL` is unset so a plain
//! `cargo test` still passes with no Postgres available (matching the repo's
//! convention that DB-backed tests never fail a no-DB run).

use cstat_core::Database;
use cstat_core::compute::compute_all;
use cstat_core::invariants::{self, Severity};
use cstat_ingest::cache::ApiCache;
use cstat_ingest::client::NatStatClient;
use cstat_ingest::simulate::{ingest_box_score_window, seed_window_objects, table_counts};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

/// Fictitious season, far from any real ingested year, so nothing about the
/// fixtures can be confused with real data even if isolation ever slipped.
const SEASON: i32 = 9001;
const FROM: &str = "2027-11-03";
const TO: &str = "2027-11-04";
const TEST_DB: &str = "cstat_ingest_replay_m4c";

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

#[tokio::test]
async fn offline_ingest_replay_is_clean_and_idempotent() {
    let Ok(base_url) = std::env::var("DATABASE_URL") else {
        eprintln!("SKIP offline_ingest_replay: DATABASE_URL unset (no Postgres available)");
        return;
    };

    // --- isolation: build a throwaway DB on the same server, never touching
    // the database `DATABASE_URL` names. ---
    let base_opts = PgConnectOptions::from_str(&base_url).expect("valid DATABASE_URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(base_opts.clone())
        .await
        .expect("connect to base database");
    // WITH (FORCE) terminates any leftover connection from a prior panicked run
    // (PG13+). Ignore the error if the DB doesn't exist.
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {TEST_DB} WITH (FORCE)"))
        .execute(&admin)
        .await;
    sqlx::query(&format!("CREATE DATABASE {TEST_DB}"))
        .execute(&admin)
        .await
        .expect("create throwaway test database");

    // Run the body; capture the outcome so cleanup (DROP DATABASE) always runs
    // before any assertion panics.
    let outcome = run_replay(&base_opts).await;

    // --- cleanup: drop the throwaway DB regardless of pass/fail. ---
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {TEST_DB} WITH (FORCE)"))
        .execute(&admin)
        .await;
    admin.close().await;

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

struct ReplayReport {
    ingest_counts: (u64, u64, u64),
    error_violations: Vec<String>,
    idempotency_drift: Vec<String>,
}

async fn run_replay(base_opts: &PgConnectOptions) -> anyhow::Result<ReplayReport> {
    let test_opts = base_opts.clone().database(TEST_DB);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(test_opts)
        .await?;
    let db = Database { pool: pool.clone() };
    db.migrate().await?;

    // Teams are reference data the box-score ingest never creates (it only
    // resolves them), exactly like a real season bootstrap seeds them first.
    for (code, name) in TEAMS {
        sqlx::query(
            "INSERT INTO teams (natstat_id, name, short_name, season) VALUES ($1, $2, $2, $3)",
        )
        .bind(code)
        .bind(name)
        .bind(SEASON)
        .execute(&pool)
        .await?;
    }

    // Build the NatStat-v4 fixture objects and seed them under the exact
    // date-range cache key the ingest will look up.
    let (games, teamperfs, playerperfs) = build_fixtures();
    let range = format!("{FROM},{TO}");
    let cache = ApiCache::new(pool.clone());
    seed_window_objects(&cache, &range, games, teamperfs, playerperfs).await?;

    // Bogus key: any un-seeded fetch fails loudly rather than going live.
    let client = NatStatClient::new(pool.clone(), "TEST-OFFLINE-M4C".into(), 2500);

    // --- first replay: ingest + compute + invariants ---
    let ingest_counts = ingest_box_score_window(&client, &pool, SEASON, FROM, TO).await?;
    compute_all(&pool, SEASON).await?;

    let error_violations: Vec<String> = invariants::check_season(&pool, SEASON)
        .await?
        .into_iter()
        .filter(|v| v.severity == Severity::Error)
        .map(|v| v.to_string())
        .collect();

    // --- idempotency: re-run the same window; derived-table counts must hold ---
    let before = table_counts(&pool).await?;
    ingest_box_score_window(&client, &pool, SEASON, FROM, TO).await?;
    compute_all(&pool, SEASON).await?;
    let after = table_counts(&pool).await?;
    let idempotency_drift: Vec<String> = before
        .iter()
        .zip(after.iter())
        .filter(|((_, b), (_, a))| b != a)
        .map(|((t, b), (_, a))| format!("{t}: {b} -> {a}"))
        .collect();

    pool.close().await;

    Ok(ReplayReport {
        ingest_counts,
        error_violations,
        idempotency_drift,
    })
}

/// Build `(games, teamperfs, playerperfs)` NatStat-v4 objects for the whole
/// window. Field shapes mirror `simulate::{game_json, teamperf_json,
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

        // Three players per side, drawn from a fixed 3-man roster per team so
        // each player accumulates multiple games (exercises season aggregation
        // + team reconciliation), splitting the team's points.
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
