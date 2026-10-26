//! The nightly's `transfer_resolve` step (#405).
//!
//! The nightly ingested 247 portal rows for months without ever linking them
//! to a player, so every move it picked up projected at the player's OLD school
//! (Trey Kaufman-Renn at Purdue after committing to Kansas). These tests drive
//! `transfer_resolve_step` — the exact function `SeasonIngester::nightly`
//! calls — and pin both halves of its contract: a resolvable row gets linked
//! and the ledger records `ok`, and a failure is recorded AND degrades the run.
//!
//! Isolation follows `ledger_write_failures.rs`: one pinned connection, and
//! TEMP tables that shadow every real table the step reads or writes
//! (`pg_temp` precedes `public` on `search_path`). Setup asserts each name
//! resolves into a `pg_temp*` schema and refuses to run otherwise, so nothing
//! touches whatever `DATABASE_URL` points at. Skips when it is unset.

use cstat_ingest::ingest::season::transfer_resolve_step;
use cstat_ingest::run_ledger::RunLedger;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const LEDGER_DDL: &str = "
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

/// Only the columns `resolve_cstat_joins` touches.
const SOURCE_DDL: [&str; 4] = [
    "CREATE TEMP TABLE teams (id uuid, season integer, short_name text, name text)",
    "CREATE TEMP TABLE players (id uuid, name text, season integer, natstat_id text)",
    "CREATE TEMP TABLE player_season_stats (player_id uuid, season integer, team_id uuid)",
    "CREATE TEMP TABLE torvik_player_stats (player_id uuid, torvik_pid integer)",
];

const TRANSFERS_DDL: &str = "CREATE TEMP TABLE transfers \
    (tfs_key bigint, year integer, full_name text, source_institution text, cstat_player_id uuid)";

/// The same table without the column the step writes, so its UPDATE fails.
const BROKEN_TRANSFERS_DDL: &str = "CREATE TEMP TABLE transfers \
    (tfs_key bigint, year integer, full_name text, source_institution text)";

const SHADOWED: [&str; 6] = [
    "ingest_runs",
    "teams",
    "players",
    "player_season_stats",
    "torvik_player_stats",
    "transfers",
];

/// A single pinned connection with every shadowing table created, or `None`
/// when `DATABASE_URL` is unset. Panics if any name fails to land in
/// `pg_temp*` — the guarantee that no real table is written.
async fn isolated_pool(transfers_ddl: &str) -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .expect("connect");
    for ddl in [LEDGER_DDL, transfers_ddl].into_iter().chain(SOURCE_DDL) {
        sqlx::query(ddl)
            .execute(&pool)
            .await
            .expect("create temp table");
    }
    for table in SHADOWED {
        let schema: String = sqlx::query_scalar(
            "SELECT n.nspname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = to_regclass($1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("resolve table");
        assert!(
            schema.starts_with("pg_temp"),
            "{table} resolved to {schema}, not a temp schema; refusing to run"
        );
    }
    Some(pool)
}

/// One LSU player whose 247 portal row spells his name differently, which is
/// the case only pass 3 can link.
async fn seed_nwoko(pool: &PgPool) -> Uuid {
    let team = Uuid::new_v4();
    let player = Uuid::new_v4();
    sqlx::query("INSERT INTO teams VALUES ($1, 2026, 'LSU', 'LSU Tigers')")
        .bind(team)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO players VALUES ($1, 'Michael Nwoko', 2026, '9001')")
        .bind(player)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO player_season_stats VALUES ($1, 2026, $2)")
        .bind(player)
        .bind(team)
        .execute(pool)
        .await
        .unwrap();
    player
}

async fn ledger_row(pool: &PgPool) -> (String, Option<i64>, Option<String>) {
    sqlx::query_as(
        "SELECT status, rows_touched, error FROM ingest_runs WHERE step = 'transfer_resolve'",
    )
    .fetch_one(pool)
    .await
    .expect("exactly one transfer_resolve ledger row")
}

#[tokio::test]
async fn links_a_committed_transfer_and_records_ok() {
    let Some(pool) = isolated_pool(TRANSFERS_DDL).await else {
        return;
    };
    let player = seed_nwoko(&pool).await;
    sqlx::query("INSERT INTO transfers VALUES (1, 2026, 'Mike Nwoko', 'LSU', NULL)")
        .execute(&pool)
        .await
        .unwrap();

    let ledger = RunLedger::start(&pool, 2026);
    let mut failures = Vec::new();
    transfer_resolve_step(&pool, 2026, &ledger, &mut failures).await;

    let linked: Option<Uuid> =
        sqlx::query_scalar("SELECT cstat_player_id FROM transfers WHERE tfs_key = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked, Some(player));
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    assert_eq!(ledger_row(&pool).await, ("ok".to_string(), Some(1), None));
}

#[tokio::test]
async fn a_resolution_failure_is_recorded_and_degrades_the_run() {
    let Some(pool) = isolated_pool(BROKEN_TRANSFERS_DDL).await else {
        return;
    };
    seed_nwoko(&pool).await;
    sqlx::query("INSERT INTO transfers VALUES (1, 2026, 'Mike Nwoko', 'LSU')")
        .execute(&pool)
        .await
        .unwrap();

    let ledger = RunLedger::start(&pool, 2026);
    let mut failures = Vec::new();
    transfer_resolve_step(&pool, 2026, &ledger, &mut failures).await;

    let (status, _, error) = ledger_row(&pool).await;
    assert_eq!(status, "failed");
    assert!(error.is_some_and(|e| e.contains("player joins")));
    assert_eq!(failures.len(), 1);
    assert!(failures[0].starts_with("transfer_resolve: "));
}
