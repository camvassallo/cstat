//! A player the CBBD gap-fill creates (`natstat_id` `espn:<id>`) after the
//! night's Torvik step gets his stored Torvik row from `torvik-relink`, and
//! with it the `torvik_pid` that links his seasons before a transfer.
//!
//! The case is Robbie Avila: Saint Louis through 2026, created at TCU from
//! CBBD's box in November 2026 with no NatStat id to match and no prior
//! seasons on his page. Pins that the relink links his 2027 Torvik row to the
//! created player, fills the class year and height he was created without,
//! makes `resolve_player_id_for_season` find his 2026 row, and leaves a row
//! whose match another stored row already holds unlinked.
//!
//! Isolation follows `cbbd_primary.rs`: one pinned connection and TEMP tables
//! shadowing every real table touched, each asserted to resolve into
//! `pg_temp*`. Skips when `DATABASE_URL` is unset.

use cstat_ingest::ingest::torvik::relink_stored_rows;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const DDL: [&str; 4] = [
    "CREATE TEMP TABLE teams (id uuid PRIMARY KEY, name text, short_name text, season int)",
    "CREATE TEMP TABLE players (id uuid PRIMARY KEY, natstat_id text NOT NULL, name text, \
        team_id uuid, season int, class_year text, height_inches int, \
        updated_at timestamptz DEFAULT now())",
    "CREATE TEMP TABLE player_game_stats (player_id uuid)",
    "CREATE TEMP TABLE torvik_player_stats (player_id uuid, torvik_pid int, season int, \
        player_name text, team_name text, total_minutes float8, class_year text, height text, \
        updated_at timestamptz DEFAULT now(), UNIQUE (torvik_pid, season))",
];

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
        sqlx::query(ddl).execute(&pool).await.expect("temp table");
    }
    for table in [
        "teams",
        "players",
        "player_game_stats",
        "torvik_player_stats",
    ] {
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

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[tokio::test]
async fn a_created_transfer_takes_his_torvik_row_and_with_it_his_career() {
    let Some(pool) = isolated_pool().await else {
        return;
    };
    let (slu, tcu) = (id(1), id(2));
    let (avila_26, avila_27, doe_27) = (id(10), id(11), id(12));
    sqlx::query(
        "INSERT INTO teams VALUES ($1, 'Saint Louis Billikens', 'Saint Louis', 2026), \
                                  ($2, 'TCU Horned Frogs', 'TCU', 2027)",
    )
    .bind(slu)
    .bind(tcu)
    .execute(&pool)
    .await
    .unwrap();
    // His NatStat 2026 row; the 2027 row the gap-fill created from CBBD (no
    // class year, no NatStat id that matches); a teammate already linked.
    sqlx::query(
        "INSERT INTO players (id, natstat_id, name, team_id, season, class_year) VALUES \
           ($1, '87834325', 'Robbie Avila', $4, 2026, 'Sr'), \
           ($2, 'espn:4683689', 'Robbie Avila', $5, 2027, NULL), \
           ($3, '105000001', 'Jane Doe', $5, 2027, 'Jr')",
    )
    .bind(avila_26)
    .bind(avila_27)
    .bind(doe_27)
    .bind(slu)
    .bind(tcu)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO player_game_stats VALUES ($1), ($2), ($3)")
        .bind(avila_26)
        .bind(avila_27)
        .bind(doe_27)
        .execute(&pool)
        .await
        .unwrap();
    // Torvik: his pid in both seasons, the 2027 row stored before he existed
    // (unlinked); the teammate's row linked; and a second Torvik row naming
    // the teammate, which must not give her a second profile.
    sqlx::query(
        "INSERT INTO torvik_player_stats \
           (player_id, torvik_pid, season, player_name, team_name, total_minutes, class_year, height) \
         VALUES ($1, 76175, 2026, 'Robbie Avila', 'Saint Louis', 900, 'Jr', '6-10'), \
                (NULL, 76175, 2027, 'Robbie Avila', 'TCU', 30, 'Sr', '6-10'), \
                ($2, 500, 2027, 'Jane Doe', 'TCU', 40, 'Jr', '6-2'), \
                (NULL, 501, 2027, 'Jane Doe', 'TCU', 5, 'Jr', '6-2')",
    )
    .bind(avila_26)
    .bind(doe_27)
    .execute(&pool)
    .await
    .unwrap();

    // Before: no route from the created row to his 2026 season.
    assert_eq!(
        cstat_core::queries::resolve_player_id_for_season(&pool, avila_27, 2026)
            .await
            .unwrap(),
        None
    );

    let r = relink_stored_rows(&pool, 2027).await.unwrap();
    assert_eq!((r.unlinked, r.linked), (2, 1));

    let linked: Option<Uuid> = sqlx::query_scalar(
        "SELECT player_id FROM torvik_player_stats WHERE torvik_pid = 76175 AND season = 2027",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(linked, Some(avila_27));
    let dup: Option<Uuid> = sqlx::query_scalar(
        "SELECT player_id FROM torvik_player_stats WHERE torvik_pid = 501 AND season = 2027",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        dup, None,
        "a player another row holds gets no second profile"
    );

    let (class, height): (Option<String>, Option<i32>) =
        sqlx::query_as("SELECT class_year, height_inches FROM players WHERE id = $1")
            .bind(avila_27)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((class.as_deref(), height), (Some("Sr"), Some(82)));

    // After: his page reaches Saint Louis, both ways.
    assert_eq!(
        cstat_core::queries::resolve_player_id_for_season(&pool, avila_27, 2026)
            .await
            .unwrap(),
        Some(avila_26)
    );
    assert_eq!(
        cstat_core::queries::resolve_player_id_for_season(&pool, avila_26, 2027)
            .await
            .unwrap(),
        Some(avila_27)
    );

    // A second run changes nothing.
    let again = relink_stored_rows(&pool, 2027).await.unwrap();
    assert_eq!((again.unlinked, again.linked), (1, 0));
}
