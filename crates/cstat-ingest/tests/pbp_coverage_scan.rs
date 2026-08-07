//! Regression net for the silent play-by-play hole (issue #247).
//!
//! The self-heal gap scan certifies a date as covered on `BOX_SCORE_STEPS`
//! alone. `playbyplay` sits outside that set on purpose — it is best-effort, and
//! folding it in would re-pull a night's box scores because its PBP failed. The
//! cost of that exclusion was that a night where the box scores succeeded and
//! `playbyplay` failed was **never revisited by anything**: the date read as
//! covered forever, and `compute_pbp_lineups` (a season-scoped DELETE-then-
//! rebuild) rebuilt `lineup_aggregates` / `player_on_off` / `lineup_stints`
//! around the hole on every subsequent run.
//!
//! These tests pin the asymmetry that fixes it — the two scans disagreeing about
//! the same night is the whole point, so a future refactor that collapses them
//! into one (in either direction) fails here.
//!
//! Isolation: the temp-ledger harness from `ledger_write_failures.rs` — a pinned
//! single-connection pool plus a TEMP `ingest_runs` that shadows the real table
//! via `search_path` (`pg_temp` precedes `public`), asserted to actually have
//! resolved that way. Nothing touches whatever `DATABASE_URL` points at.

use chrono::{NaiveDate, Utc};
use cstat_ingest::run_ledger::{
    RunLedger, StepStatus, first_uncovered_ingest_date, first_uncovered_pbp_date,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// Mirrors the real `ingest_runs` shape (migrations 039/043) closely enough that
/// `record`'s INSERT binds unchanged and the coverage scan's window/step/status
/// predicates all resolve. Kept in sync with the copy in
/// `ledger_write_failures.rs` — both shadow the same production table.
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

async fn temp_ledger_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::query(TEMP_LEDGER_DDL)
        .execute(&pool)
        .await
        .expect("create temp ledger");

    let schema: String = sqlx::query_scalar(
        "SELECT n.nspname FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = 'ingest_runs'::regclass",
    )
    .fetch_one(&pool)
    .await
    .expect("resolve ingest_runs");
    assert!(
        schema.starts_with("pg_temp"),
        "refusing to run: `ingest_runs` resolved to schema `{schema}`, not a temp \
         schema — this test would write to the REAL ledger in DATABASE_URL"
    );

    Some(pool)
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// Record one nightly's worth of step rows over `[from, to]`. `pbp` is the
/// status the `playbyplay` step ended with — the only thing that differs between
/// a healthy night and the one this issue is about.
async fn record_run(pool: &PgPool, from: &str, to: &str, pbp: StepStatus) {
    let mut ledger = RunLedger::start(pool, 2027);
    ledger.set_window(d(from), d(to));
    for step in ["games", "player_perfs", "team_perfs"] {
        ledger
            .record(step, StepStatus::Ok, Some(50), Utc::now(), None)
            .await;
    }
    let err = (pbp == StepStatus::Failed).then_some("natstat 500");
    ledger
        .record("playbyplay", pbp, None, Utc::now(), err)
        .await;
    assert_eq!(
        ledger.write_failures(),
        0,
        "fixture ledger writes must all land, or the scan under test sees nothing"
    );
}

#[tokio::test]
async fn failed_pbp_night_is_uncovered_for_pbp_but_covered_for_box_scores() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };

    // Two healthy nights, then one where only `playbyplay` failed — the exact
    // shape of the hole: nothing served-critical broke, so the run posted a
    // DEGRADED line once and was never heard from again.
    record_run(&pool, "2027-11-05", "2027-11-06", StepStatus::Ok).await;
    record_run(&pool, "2027-11-07", "2027-11-08", StepStatus::Failed).await;

    let scan_from = d("2027-11-09");
    let box_gap =
        first_uncovered_ingest_date(&pool, Uuid::new_v4(), scan_from, scan_from, 30).await;
    let pbp_gap = first_uncovered_pbp_date(&pool, Uuid::new_v4(), scan_from, scan_from, 30).await;

    assert_eq!(
        box_gap, None,
        "the box-score steps all succeeded, so re-pulling them would be wasted \
         NatStat calls — the PBP failure must not drag them into a heal"
    );
    assert_eq!(
        pbp_gap,
        Some(d("2027-11-07")),
        "the first night whose `playbyplay` step failed must read as uncovered — \
         this is the date nothing used to go back for (issue #247)"
    );
}

/// A heal that worked must not run again. The PBP step covers a wider range
/// than the rest of the run on a heal night, so its ledger row carries its own
/// window (`record_windowed`). Stamped with the run's narrower box-score window
/// instead, the next scan finds the identical gap, heals it again, and the
/// nightly re-fetches the same days — hundreds of API calls a night, forever,
/// with the coverage it actually achieved never recorded.
#[tokio::test]
async fn a_healed_pbp_pull_records_the_range_it_actually_covered() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };

    // A stretch of healthy history FIRST, and it is load-bearing rather than
    // scene-setting: the scan's `MIN(window_start)` floor never looks back past
    // the earliest successful window, so without an earlier good run the floor
    // alone would return `None` and this test would pass no matter what the heal
    // recorded. (Prod has months of history, so the floor sits far behind the
    // 30-day horizon and never masks a real gap.)
    record_run(&pool, "2027-11-01", "2027-11-04", StepStatus::Ok).await;
    // Then a night where box scores were fine and PBP failed.
    record_run(&pool, "2027-11-05", "2027-11-06", StepStatus::Failed).await;

    // Tonight, as the nightly runs it: the box window is the plain default,
    // while the PBP step reaches back over the gap it just found.
    let mut ledger = RunLedger::start(&pool, 2027);
    ledger.set_window(d("2027-11-07"), d("2027-11-08"));
    for step in ["games", "player_perfs", "team_perfs"] {
        ledger
            .record(step, StepStatus::Ok, Some(50), Utc::now(), None)
            .await;
    }
    ledger
        .record_windowed(
            "playbyplay",
            StepStatus::Ok,
            Some(30_000),
            Utc::now(),
            None,
            Some((d("2027-11-05"), d("2027-11-08"))),
        )
        .await;

    let scan_from = d("2027-11-09");
    assert_eq!(
        first_uncovered_pbp_date(&pool, Uuid::new_v4(), scan_from, scan_from, 30).await,
        None,
        "the heal re-pulled 11-05..11-08 and its ledger row says so, so there is \
         nothing left to heal — a gap here means the step recorded the run's \
         narrow window and the nightly will re-fetch these days every night"
    );
}

#[tokio::test]
async fn healthy_nights_leave_no_pbp_gap() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };

    record_run(&pool, "2027-11-05", "2027-11-06", StepStatus::Ok).await;
    record_run(&pool, "2027-11-07", "2027-11-08", StepStatus::Ok).await;

    let scan_from = d("2027-11-09");
    assert_eq!(
        first_uncovered_pbp_date(&pool, Uuid::new_v4(), scan_from, scan_from, 30).await,
        None,
        "an unbroken run of successful `playbyplay` steps must not widen the \
         window — a spurious heal is a few hundred wasted API calls a night"
    );
}

#[tokio::test]
async fn a_ledger_with_no_pbp_history_reports_no_gap() {
    let Some(pool) = temp_ledger_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };

    // Box scores only — prod's ledger before the S2 deploy, when the nightly did
    // not yet ingest PBP at all. The `MIN(window_start)` floor is what keeps this
    // from reading the entire lookback as uncovered and pulling 30 days of
    // play-by-play on the first run after the deploy.
    let mut ledger = RunLedger::start(&pool, 2027);
    ledger.set_window(d("2027-11-05"), d("2027-11-06"));
    for step in ["games", "player_perfs", "team_perfs"] {
        ledger
            .record(step, StepStatus::Ok, Some(50), Utc::now(), None)
            .await;
    }

    let scan_from = d("2027-11-09");
    assert_eq!(
        first_uncovered_pbp_date(&pool, Uuid::new_v4(), scan_from, scan_from, 30).await,
        None,
        "with no `playbyplay` step ever recorded the scan must stay silent, not \
         declare the whole lookback a gap"
    );
}
