//! The nightly's full-season schedule refresh (#406).
//!
//! `apply_season_schedule` rewrites a whole season every night, so its update
//! rules carry the risk: it must not blank a completed game, undo what
//! `compute_all` derived, or churn rows that did not change — and its prune
//! deletes games, so each guard on it is pinned here.
//!
//! Isolation follows `transfer_resolve_step.rs`: one pinned connection and TEMP
//! tables shadowing every real table the function touches, each asserted to
//! resolve into `pg_temp*` before anything runs. Skips when `DATABASE_URL` is
//! unset. The one exception is `prune_blockers_cover_every_foreign_key`, which
//! reads the catalog of its own throwaway, freshly migrated database.

use chrono::NaiveDate;
use cstat_core::Database;
use cstat_ingest::ingest::games::GameRow;
use cstat_ingest::ingest::schedule::{PRUNE_BLOCKERS, ScheduleRefresh, apply_season_schedule};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use uuid::Uuid;

const SEASON: i32 = 2027;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 15).unwrap()
}

fn d(m: u32, day: u32) -> NaiveDate {
    let y = if m >= 7 { 2026 } else { 2027 };
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

const GAMES_DDL: &str = "
    CREATE TEMP TABLE games (
        id uuid PRIMARY KEY,
        natstat_id text NOT NULL UNIQUE,
        season integer NOT NULL,
        game_date date NOT NULL,
        home_team_id uuid,
        away_team_id uuid,
        home_score integer,
        away_score integer,
        is_neutral_site boolean NOT NULL DEFAULT false,
        is_conference boolean,
        is_postseason boolean,
        venue text,
        venue_code text,
        overtime text,
        attendance integer,
        status text,
        home_half1 integer,
        home_half2 integer,
        away_half1 integer,
        away_half2 integer,
        updated_at timestamp NOT NULL DEFAULT now()
    )";

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
    let mut ddl = vec![
        GAMES_DDL.to_string(),
        "CREATE TEMP TABLE teams (id uuid, natstat_id text, season integer)".to_string(),
        "CREATE TEMP TABLE schedules (game_id uuid)".to_string(),
    ];
    for t in PRUNE_BLOCKERS {
        ddl.push(format!("CREATE TEMP TABLE {t} (game_id uuid)"));
    }
    for stmt in &ddl {
        sqlx::query(stmt)
            .execute(&pool)
            .await
            .expect("create temp table");
    }
    let shadowed = ["games", "teams", "schedules"]
        .into_iter()
        .chain(PRUNE_BLOCKERS);
    for table in shadowed {
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
    for code in ["DUKE", "UNC"] {
        sqlx::query("INSERT INTO teams VALUES (gen_random_uuid(), $1, $2)")
            .bind(code)
            .bind(SEASON)
            .execute(&pool)
            .await
            .unwrap();
    }
    Some(pool)
}

/// A scheduled game as the feed lists it: no score, no status.
fn scheduled(id: &str, date: NaiveDate) -> GameRow {
    GameRow {
        natstat_id: id.to_string(),
        game_date: date,
        home_code: Some("DUKE".into()),
        away_code: Some("UNC".into()),
        home_score: None,
        away_score: None,
        is_neutral: false,
        is_conference: None,
        is_postseason: None,
        venue: None,
        venue_code: None,
        overtime: None,
        attendance: None,
        status: None,
        home_half1: None,
        home_half2: None,
        away_half1: None,
        away_half2: None,
    }
}

async fn apply(pool: &PgPool, rows: &[GameRow], complete: bool) -> ScheduleRefresh {
    apply_season_schedule(pool, SEASON, rows, complete, today())
        .await
        .expect("apply")
}

async fn insert_game(pool: &PgPool, id: &str, date: NaiveDate, score: Option<(i32, i32)>) -> Uuid {
    let uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO games (id, natstat_id, season, game_date, home_score, away_score)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(uuid)
    .bind(id)
    .bind(SEASON)
    .bind(date)
    .bind(score.map(|s| s.0))
    .bind(score.map(|s| s.1))
    .execute(pool)
    .await
    .unwrap();
    uuid
}

async fn exists(pool: &PgPool, id: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM games WHERE natstat_id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_unchanged_feed_writes_nothing_on_the_second_run() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let feed: Vec<GameRow> = (1..=40)
        .map(|i| scheduled(&format!("g{i}"), d(12, 1)))
        .collect();

    let first = apply(&pool, &feed, true).await;
    assert_eq!((first.inserted, first.updated), (40, 0));
    let second = apply(&pool, &feed, true).await;
    assert_eq!(second.written(), 0, "{second:?}");
}

#[tokio::test]
async fn a_duplicated_feed_row_is_written_once() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let feed = vec![scheduled("g1", d(12, 1)), scheduled("g1", d(12, 2))];
    let r = apply(&pool, &feed, true).await;
    assert_eq!(r.inserted, 1);
}

#[tokio::test]
async fn a_completed_game_keeps_its_result_date_and_derived_flags() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    insert_game(&pool, "g1", d(11, 10), Some((80, 70))).await;
    // What compute_all and the window ingest left on it.
    sqlx::query(
        "UPDATE games SET is_neutral_site = true, is_conference = false, status = 'Final',
                          venue = 'T-Mobile Arena',
                          home_team_id = (SELECT id FROM teams WHERE natstat_id = 'DUKE'),
                          away_team_id = (SELECT id FROM teams WHERE natstat_id = 'UNC')
          WHERE natstat_id = 'g1'",
    )
    .execute(&pool)
    .await
    .unwrap();

    // The refresh sees a bare row with a different date and no flags.
    let mut row = scheduled("g1", d(11, 12));
    row.is_conference = Some(true);
    let r = apply(&pool, &[row], true).await;
    assert_eq!(r.written(), 0, "{r:?}");

    let got: (NaiveDate, Option<i32>, bool, Option<bool>, Option<String>) = sqlx::query_as(
        "SELECT game_date, home_score, is_neutral_site, is_conference, status
           FROM games WHERE natstat_id = 'g1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        got,
        (d(11, 10), Some(80), true, Some(false), Some("Final".into()))
    );
}

#[tokio::test]
async fn an_unplayed_game_follows_a_reschedule_and_fills_new_fields() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    insert_game(&pool, "g1", d(12, 1), None).await;
    let mut row = scheduled("g1", d(12, 3));
    row.is_neutral = true;
    row.venue = Some("Barclays Center".into());
    let r = apply(&pool, &[row], true).await;
    assert_eq!(r.updated, 1);

    let got: (NaiveDate, bool, Option<String>, Option<Uuid>) = sqlx::query_as(
        "SELECT game_date, is_neutral_site, venue, home_team_id FROM games WHERE natstat_id = 'g1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (got.0, got.1, got.2.as_deref()),
        (d(12, 3), true, Some("Barclays Center"))
    );
    assert!(
        got.3.is_some(),
        "team code resolved to the season's teams row"
    );
}

#[tokio::test]
async fn a_game_stored_under_another_season_is_left_alone() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    sqlx::query(
        "INSERT INTO games (id, natstat_id, season, game_date) VALUES (gen_random_uuid(), 'g1', 2026, '2026-01-05')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let r = apply(&pool, &[scheduled("g1", d(1, 5))], true).await;
    assert_eq!(r.written(), 0);
    let season: i32 = sqlx::query_scalar("SELECT season FROM games WHERE natstat_id = 'g1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(season, 2026);
}

#[tokio::test]
async fn the_prune_deletes_only_unplayed_future_games_without_dependents() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let gone = insert_game(&pool, "gone", d(12, 1), None).await;
    insert_game(&pool, "past_unscored", d(11, 1), None).await;
    insert_game(&pool, "future_scored", d(12, 1), Some((1, 0))).await;
    let forecast = insert_game(&pool, "has_forecast", d(12, 1), None).await;
    sqlx::query("INSERT INTO schedules VALUES ($1)")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO game_forecasts VALUES ($1)")
        .bind(forecast)
        .execute(&pool)
        .await
        .unwrap();

    // The feed still lists a game, so the fetch is non-empty, and none of the above.
    let r = apply(&pool, &[scheduled("listed", d(12, 5))], true).await;

    assert_eq!(r.pruned, 1);
    assert!(!exists(&pool, "gone").await);
    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM schedules WHERE game_id = $1")
        .bind(gone)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(orphans, 0, "the vanished game's schedules row goes with it");
    for kept in ["past_unscored", "future_scored", "has_forecast"] {
        assert!(exists(&pool, kept).await, "{kept} must survive the prune");
    }
    assert!(r.prune_note.unwrap().contains("kept 1"));
}

#[tokio::test]
async fn an_incomplete_fetch_never_prunes() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    insert_game(&pool, "gone", d(12, 1), None).await;
    let r = apply(&pool, &[scheduled("listed", d(12, 5))], false).await;
    assert_eq!(r.pruned, 0);
    assert!(exists(&pool, "gone").await);
    assert!(r.prune_note.unwrap().contains("incomplete"));
}

#[tokio::test]
async fn an_empty_feed_never_prunes() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    insert_game(&pool, "gone", d(12, 1), None).await;
    let r = apply(&pool, &[], true).await;
    assert_eq!(r.pruned, 0);
    assert!(exists(&pool, "gone").await);
}

#[tokio::test]
async fn a_prune_past_the_cap_is_refused_whole() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    // 300 unplayed games, 40 of them missing from the feed: past the 10% / 25 cap.
    let mut feed = Vec::new();
    for i in 0..300 {
        let id = format!("g{i}");
        insert_game(&pool, &id, d(12, 1), None).await;
        if i >= 40 {
            feed.push(scheduled(&id, d(12, 1)));
        }
    }
    let r = apply(&pool, &feed, true).await;
    assert_eq!(r.pruned, 0);
    assert!(r.prune_note.unwrap().contains("exceeds the cap"));
    assert!(exists(&pool, "g0").await);
}

#[tokio::test]
async fn double_booked_team_days_are_counted() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let r = apply(
        &pool,
        &[scheduled("a", d(12, 1)), scheduled("b", d(12, 1))],
        true,
    )
    .await;
    // DUKE and UNC each play twice that day.
    assert_eq!(r.double_booked, 2);
}

/// `PRUNE_BLOCKERS` plus `schedules` (cleared by the prune) and the cascading
/// `game_projections` must be exactly the tables with a foreign key onto
/// `games`. A new one would otherwise make the prune's DELETE fail every night
/// — or, if it cascaded, silently delete its rows.
///
/// Reads the catalog of a throwaway database migrated from `migrations/`
/// (the `ingest_replay.rs` pattern): CI's database is empty, and a dev
/// database may lag the branch, so neither is the schema this code ships with.
#[tokio::test]
async fn prune_blockers_cover_every_foreign_key() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let base = PgConnectOptions::from_str(&url).expect("valid DATABASE_URL");
    let admin = PgPool::connect_with(base.clone()).await.expect("connect");
    let name = "cstat_test_prune_blockers";
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("create throwaway database");
    let pool = PgPool::connect_with(base.database(name))
        .await
        .expect("connect to throwaway database");
    Database { pool: pool.clone() }
        .migrate()
        .await
        .expect("migrate throwaway database");
    let referencing: Result<Vec<String>, _> = sqlx::query_scalar(
        "SELECT DISTINCT c.conrelid::regclass::text FROM pg_constraint c
          WHERE c.contype = 'f' AND c.confrelid = 'public.games'::regclass",
    )
    .fetch_all(&pool)
    .await;
    // Drop before asserting, so a failure cannot leak the database.
    pool.close().await;
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await;
    let mut referencing = referencing.expect("read catalog");
    referencing.sort();
    let mut expected: Vec<String> = PRUNE_BLOCKERS
        .iter()
        .map(|s| s.to_string())
        .chain(["schedules".to_string(), "game_projections".to_string()])
        .collect();
    expected.sort();
    assert_eq!(referencing, expected);
}
