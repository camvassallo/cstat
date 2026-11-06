//! `torvik_game_first_seen` keeps a game's ratings as first scraped.
//!
//! `torvik_player_game_stats` is overwritten nightly and Torvik's ratings for a
//! finished game change on a later scrape, so this table is the only record of
//! what was knowable at the time (#466). A write that overwrites it, even once,
//! destroys that for good.
//!
//! Isolation follows `transfer_resolve_step.rs`: one pinned connection and TEMP
//! tables shadowing the real ones, each asserted to resolve into `pg_temp*`
//! before anything runs. Skips when `DATABASE_URL` is unset.

use cstat_ingest::ingest::torvik::apply_persist_torvik_game_stats;
use cstat_ingest::torvik::TorkvikGameRow;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

const SEASON: i32 = 2027;

const DDL: [&str; 2] = [
    "CREATE TEMP TABLE torvik_player_game_stats
        (LIKE public.torvik_player_game_stats INCLUDING ALL)",
    "CREATE TEMP TABLE torvik_game_first_seen (
        pid integer NOT NULL,
        game_uid text NOT NULL,
        season integer NOT NULL,
        game_date date NOT NULL,
        team text,
        minutes_pct double precision,
        usage double precision,
        bpm double precision,
        obpm double precision,
        dbpm double precision,
        possessions double precision,
        first_seen_at timestamptz NOT NULL DEFAULT now(),
        PRIMARY KEY (pid, game_uid)
    )",
];

async fn isolated_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    for ddl in DDL {
        sqlx::query(ddl).execute(&pool).await.expect("shadow table");
    }
    for table in ["torvik_player_game_stats", "torvik_game_first_seen"] {
        let schema: String = sqlx::query_scalar(
            "SELECT n.nspname FROM pg_class c
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE c.oid = to_regclass($1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("resolve table");
        assert!(
            schema.starts_with("pg_temp"),
            "{table} resolved to {schema}"
        );
    }
    Some(pool)
}

fn row(pid: i32, game_uid: &str, date: &str, bpm: f64) -> TorkvikGameRow {
    TorkvikGameRow {
        date_str: date.to_string(),
        opponent: "Opponent".to_string(),
        game_uid: game_uid.to_string(),
        team: "Team".to_string(),
        player_name: "Player".to_string(),
        pid: Some(pid),
        year: Some(SEASON),
        location: None,
        class_year: None,
        height_inches: None,
        minutes_pct: Some(80.0),
        o_rtg: None,
        usage: Some(20.0),
        pts: None,
        oreb: None,
        dreb: None,
        ast: None,
        tov: None,
        stl: None,
        blk: None,
        pf: None,
        two_pm: None,
        two_pa: None,
        tpm: None,
        tpa: None,
        ftm: None,
        fta: None,
        rim_made: None,
        rim_attempted: None,
        mid_made: None,
        mid_attempted: None,
        dunks_made: None,
        dunks_attempted: None,
        bpm: Some(bpm),
        obpm: Some(bpm / 2.0),
        dbpm: Some(bpm / 2.0),
        possessions: Some(70.0),
    }
}

async fn bpm(pool: &PgPool, table: &str, game_uid: &str) -> Option<f64> {
    sqlx::query_scalar(&format!("SELECT bpm FROM {table} WHERE game_uid = $1"))
        .bind(game_uid)
        .fetch_optional(pool)
        .await
        .expect("read bpm")
}

#[tokio::test]
async fn a_rescrape_updates_the_live_row_and_never_the_first_seen_one() {
    let Some(pool) = isolated_pool().await else {
        return;
    };

    apply_persist_torvik_game_stats(&pool, &[row(1, "g1", "20261102", 4.0)], SEASON)
        .await
        .expect("first scrape");
    assert_eq!(bpm(&pool, "torvik_game_first_seen", "g1").await, Some(4.0));

    let captured: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT first_seen_at FROM torvik_game_first_seen")
            .fetch_one(&pool)
            .await
            .expect("capture time");

    apply_persist_torvik_game_stats(&pool, &[row(1, "g1", "20261102", 6.5)], SEASON)
        .await
        .expect("second scrape");

    assert_eq!(
        bpm(&pool, "torvik_player_game_stats", "g1").await,
        Some(6.5)
    );
    assert_eq!(bpm(&pool, "torvik_game_first_seen", "g1").await, Some(4.0));
    let after: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT first_seen_at FROM torvik_game_first_seen")
            .fetch_one(&pool)
            .await
            .expect("capture time");
    assert_eq!(after, captured);
}

#[tokio::test]
async fn a_new_game_on_a_later_scrape_is_captured() {
    let Some(pool) = isolated_pool().await else {
        return;
    };

    apply_persist_torvik_game_stats(&pool, &[row(1, "g1", "20261102", 4.0)], SEASON)
        .await
        .expect("first scrape");
    let second = [row(1, "g1", "20261102", 6.5), row(1, "g2", "20261105", 1.5)];
    apply_persist_torvik_game_stats(&pool, &second, SEASON)
        .await
        .expect("second scrape");

    assert_eq!(bpm(&pool, "torvik_game_first_seen", "g1").await, Some(4.0));
    assert_eq!(bpm(&pool, "torvik_game_first_seen", "g2").await, Some(1.5));
}

#[tokio::test]
async fn the_capture_keeps_every_input_campom_needs() {
    let Some(pool) = isolated_pool().await else {
        return;
    };

    apply_persist_torvik_game_stats(&pool, &[row(7, "g1", "20261102", 4.0)], SEASON)
        .await
        .expect("scrape");

    let (obpm, dbpm, usage, minutes, possessions): (f64, f64, f64, f64, f64) = sqlx::query_as(
        "SELECT obpm, dbpm, usage, minutes_pct, possessions
           FROM torvik_game_first_seen WHERE pid = 7",
    )
    .fetch_one(&pool)
    .await
    .expect("captured row");
    assert_eq!(
        (obpm, dbpm, usage, minutes, possessions),
        (2.0, 2.0, 20.0, 80.0, 70.0)
    );
}
