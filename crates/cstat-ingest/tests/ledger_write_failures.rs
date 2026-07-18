//! Regression net for the July 2026 silent-ledger outage (issue #186, second
//! occurrence).
//!
//! A `sync_to_prod.sh` full sync restored a dump carrying `SEQUENCE SET` state
//! for the EXCLUDED `ingest_runs` table, rewinding prod's `ingest_runs_id_seq`
//! below `max(id)`. Every subsequent ledger INSERT collided on the primary key.
//! Because `RunLedger::record` is fail-soft, the nightly kept succeeding and
//! kept reporting OK to Slack while writing nothing — for three nights. The
//! outage was invisible to `--prod-status` (every step read "77h ago", recent
//! failures "(none)"), and it silently inverted three downstream consumers:
//! `/api/health/ingest` staleness, the M5b coverage scan, and — worst — the
//! full-sync guard, which reads a dark ledger as "prod is idle" and so unblocks
//! the very operation that causes this.
//!
//! Fail-soft on the pipeline is still correct: an audit write must never abort a
//! healthy ingest. What was missing is that the ledger never noticed its own
//! silence. These tests pin that it does now.
//!
//! Isolation: a `max_connections(1)` pool plus a TEMP `ingest_runs` that shadows
//! the real table via `search_path` (`pg_temp` precedes `public`). Every
//! `record` call therefore lands on the temp table, which dies with the
//! connection — nothing whatsoever touches whatever `DATABASE_URL` points at.
//! Skips cleanly when `DATABASE_URL` is unset, like the other DB-backed tests.

use chrono::Utc;
use cstat_ingest::run_ledger::{RunLedger, StepStatus};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Mirrors the real `ingest_runs` shape (migrations 039/043) closely enough that
/// `record`'s INSERT column list binds unchanged — including the `bigserial`
/// primary key whose sequence is the thing under test.
const TEMP_LEDGER_DDL: &str = "
    CREATE TEMP TABLE ingest_runs (
        id           bigserial PRIMARY KEY,
        run_id       uuid        NOT NULL,
        season       integer     NOT NULL,
        step         text        NOT NULL,
        status       text        NOT NULL,
        rows_touched bigint,
        api_calls    bigint,
        started_at   timestamptz NOT NULL,
        ended_at     timestamptz NOT NULL,
        error        text,
        notes        text,
        window_start date,
        window_end   date
    )";

/// Single connection so every `record` reaches the same session's temp table.
async fn temp_ledger_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::query(TEMP_LEDGER_DDL)
        .execute(&pool)
        .await
        .expect("create temp ledger");
    Some(pool)
}

async fn row_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ingest_runs")
        .fetch_one(pool)
        .await
        .expect("count")
}

/// Rewind the serial sequence so the next `nextval` returns an id that already
/// exists — the exact prod state (`last_value` 68 vs `max(id)` 170).
async fn rewind_sequence(pool: &PgPool) {
    sqlx::query(
        "SELECT setval(pg_get_serial_sequence('ingest_runs', 'id'), \
         (SELECT min(id) FROM ingest_runs), false)",
    )
    .execute(pool)
    .await
    .expect("rewind");
}

#[tokio::test]
async fn rewound_sequence_is_counted_not_swallowed() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };
    let ledger = RunLedger::start(&pool, 2026);

    // Healthy writes first: the counter must stay clean on the happy path, or it
    // would degrade every run and the signal would be worthless. These also lay
    // down the CONTIGUOUS id block (1..=4) the rewind needs — prod's ids ran
    // 22..170 with no gaps, which is why 102 consecutive inserts were doomed
    // rather than just the first. With a single occupied id, `nextval` clears
    // `max(id)` after one collision and the outage would self-heal instantly.
    for step in ["preflight", "games", "player_perfs", "team_perfs"] {
        ledger
            .record(step, StepStatus::Ok, Some(49), Utc::now(), None)
            .await;
    }
    assert_eq!(
        ledger.write_failures(),
        0,
        "a successful ledger write must not count as a failure"
    );
    assert_eq!(row_count(&pool).await, 4);

    rewind_sequence(&pool).await;

    // Three steps, as a nightly would record them. Each collides on the PK.
    for step in ["player_perfs", "team_perfs", "compute"] {
        ledger
            .record(step, StepStatus::Ok, Some(1), Utc::now(), None)
            .await;
    }

    assert_eq!(
        ledger.write_failures(),
        3,
        "every failed ledger INSERT must be counted — this is the signal that \
         turns a silent multi-night outage into a degraded run summary"
    );
    // Fail-soft still holds: the calls returned normally rather than panicking
    // or propagating, and nothing extra landed in the table.
    assert_eq!(
        row_count(&pool).await,
        4,
        "the collided inserts must not have landed"
    );
}

#[tokio::test]
async fn healthy_ledger_reports_no_write_failures() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };
    let mut ledger = RunLedger::start(&pool, 2026);
    ledger.set_window(
        chrono::NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(),
        chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
    );

    for step in ["preflight", "games", "player_perfs", "compute"] {
        ledger
            .record(step, StepStatus::Ok, Some(1), Utc::now(), None)
            .await;
    }

    assert_eq!(ledger.write_failures(), 0);
    assert_eq!(row_count(&pool).await, 4);
}
