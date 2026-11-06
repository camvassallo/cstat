//! The hourly refresh's reads of the database: which games it fetches lines
//! for, and when it decides there is anything to do.
//!
//! Isolation follows `transfer_resolve_step.rs`: one pinned connection, and
//! TEMP tables that shadow every real table read (`pg_temp` precedes `public`
//! on `search_path`). Setup asserts each name resolves into a `pg_temp*` schema
//! and refuses to run otherwise. Skips when `DATABASE_URL` is unset.

use chrono::{NaiveDate, NaiveDateTime};
use cstat_ingest::espn_lines::targets;
use cstat_ingest::hourly::{STEP_FREEZE, freeze_owed, slate_ahead};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const DDL: [&str; 3] = [
    "CREATE TEMP TABLE games (id uuid, season integer, game_date date, home_team_id uuid, \
     home_score integer, away_score integer, tip_time time)",
    "CREATE TEMP TABLE game_broadcasts (game_id uuid, source_event_id text, \
     source_home_is_away boolean, tip_time time, tip_date date)",
    "CREATE TEMP TABLE ingest_runs (step text, status text, ended_at timestamptz)",
];

const SHADOWED: [&str; 3] = ["games", "game_broadcasts", "ingest_runs"];

async fn isolated_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .expect("connect");
    for ddl in DDL {
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

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn at(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

/// A matched game. `feed_tip` is the schedule feed's time, `listed_tip` the
/// broadcast listing's, as `(time, Eastern date)`.
async fn seed(
    pool: &PgPool,
    game_date: &str,
    score: Option<i32>,
    feed_tip: Option<&str>,
    listed_tip: Option<(&str, &str)>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO games VALUES ($1, 2027, $2::date, $3, $4, $4, $5::time)")
        .bind(id)
        .bind(game_date)
        .bind(Uuid::new_v4())
        .bind(score)
        .bind(feed_tip)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO game_broadcasts VALUES ($1, 'evt', false, $2::time, $3::date)")
        .bind(id)
        .bind(listed_tip.map(|t| t.0))
        .bind(listed_tip.map(|t| t.1))
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn ids(pool: &PgPool, untipped_at: Option<NaiveDateTime>) -> Vec<Uuid> {
    targets(pool, date("2026-11-07"), date("2026-11-08"), untipped_at)
        .await
        .expect("targets")
        .into_iter()
        .map(|t| t.game_id)
        .collect()
}

#[tokio::test]
async fn only_games_still_to_tip_are_read() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let now = at("2026-11-07 18:00");
    let tipped = seed(&pool, "2026-11-07", None, Some("14:00"), None).await;
    let finished = seed(&pool, "2026-11-07", Some(70), None, None).await;
    let later = seed(&pool, "2026-11-07", None, Some("19:00"), None).await;
    let untimed = seed(&pool, "2026-11-07", None, None, None).await;
    let tomorrow = seed(&pool, "2026-11-08", None, Some("12:00"), None).await;
    // The listing's time beats the feed's, which says this one has tipped.
    let moved = seed(
        &pool,
        "2026-11-07",
        None,
        Some("12:00"),
        Some(("20:00", "2026-11-07")),
    )
    .await;
    // A feed time before 6 AM is the end of the night, not its start.
    let late_night = seed(&pool, "2026-11-07", None, Some("00:30"), None).await;

    let read = ids(&pool, Some(now)).await;
    for game in [later, untimed, tomorrow, moved, late_night] {
        assert!(read.contains(&game));
    }
    assert!(!read.contains(&tipped));
    assert!(!read.contains(&finished));
    assert_eq!(read.len(), 5);

    // The nightly reads closing lines, so it filters nothing.
    assert_eq!(ids(&pool, None).await.len(), 7);
}

#[tokio::test]
async fn a_window_with_only_finished_games_has_no_slate() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let (from, to) = (date("2026-11-07"), date("2026-11-08"));
    seed(&pool, "2026-11-07", Some(70), None, None).await;
    assert!(!slate_ahead(&pool, 2027, from, to).await.unwrap());

    seed(&pool, "2026-11-08", None, None, None).await;
    assert!(slate_ahead(&pool, 2027, from, to).await.unwrap());
    assert!(!slate_ahead(&pool, 2026, from, to).await.unwrap());
}

async fn ledger(pool: &PgPool, step: &str, status: &str, ended_at: &str) {
    sqlx::query("INSERT INTO ingest_runs VALUES ($1, $2, $3::timestamptz)")
        .bind(step)
        .bind(status)
        .bind(ended_at)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_freeze_is_owed_only_after_an_unfrozen_sweep() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    assert!(!freeze_owed(&pool).await.unwrap(), "no sweep yet");

    ledger(&pool, "upcoming_projections", "ok", "2026-11-07 09:40+00").await;
    assert!(freeze_owed(&pool).await.unwrap(), "never frozen");

    ledger(&pool, "pregame_freeze", "failed", "2026-11-07 09:41+00").await;
    assert!(freeze_owed(&pool).await.unwrap(), "the freeze failed");

    ledger(&pool, STEP_FREEZE, "ok", "2026-11-07 14:45+00").await;
    assert!(!freeze_owed(&pool).await.unwrap(), "caught up");

    ledger(&pool, "upcoming_projections", "ok", "2026-11-08 09:40+00").await;
    ledger(&pool, "pregame_freeze", "ok", "2026-11-08 09:41+00").await;
    assert!(!freeze_owed(&pool).await.unwrap(), "the nightly froze it");
}
