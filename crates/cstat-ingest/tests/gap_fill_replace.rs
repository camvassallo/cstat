//! NatStat's box replaces a CBBD gap-fill (migration 077,
//! docs/natstat_migration_plan.md §4.5 step 3).
//!
//! The gap-fill writes `source = 'cbbd'` rows for games NatStat has no box
//! for. When NatStat's lines for one of those games arrive, the perf ingest
//! calls `clear_cbbd_rows` on the page before writing, so its rows replace the
//! fill instead of landing beside it. These tests pin what that deletes: the
//! CBBD rows of exactly the game sides the page carries, only once the game's
//! status lets NatStat's lines through, and never a NatStat row.
//!
//! Isolation follows `transfer_resolve_step.rs`: one pinned connection and
//! TEMP tables shadowing every real table touched, each asserted to resolve
//! into `pg_temp*` before anything runs. Skips when `DATABASE_URL` is unset.

use cstat_ingest::ingest::games::{BoxTable, clear_cbbd_rows};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const SEASON: i32 = 2027;

const DDL: [&str; 4] = [
    "CREATE TEMP TABLE games (id uuid PRIMARY KEY, natstat_id text, status text)",
    "CREATE TEMP TABLE teams (id uuid PRIMARY KEY, natstat_id text, season integer)",
    "CREATE TEMP TABLE player_game_stats (player_id uuid, game_id uuid, team_id uuid, \
        source text NOT NULL DEFAULT 'natstat')",
    "CREATE TEMP TABLE team_game_stats (game_id uuid, team_id uuid, \
        source text NOT NULL DEFAULT 'natstat')",
];

const SHADOWED: [&str; 4] = ["games", "teams", "player_game_stats", "team_game_stats"];

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

struct Seed {
    home: Uuid,
    away: Uuid,
}

/// Game "G1" (`status`), Notre Dame (`ND`) vs Kentucky (`UK`), both sides
/// gap-filled: two CBBD player rows and one CBBD team row each.
async fn seed(pool: &PgPool, status: &str) -> Seed {
    let game = Uuid::new_v4();
    let (home, away) = (Uuid::new_v4(), Uuid::new_v4());
    sqlx::query("INSERT INTO games VALUES ($1, 'G1', $2)")
        .bind(game)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    for (team, code) in [(home, "ND"), (away, "UK")] {
        sqlx::query("INSERT INTO teams VALUES ($1, $2, $3)")
            .bind(team)
            .bind(code)
            .bind(SEASON)
            .execute(pool)
            .await
            .unwrap();
        for _ in 0..2 {
            sqlx::query("INSERT INTO player_game_stats VALUES ($1, $2, $3, 'cbbd')")
                .bind(Uuid::new_v4())
                .bind(game)
                .bind(team)
                .execute(pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO team_game_stats VALUES ($1, $2, 'cbbd')")
            .bind(game)
            .bind(team)
            .execute(pool)
            .await
            .unwrap();
    }
    // An unrelated NatStat row on the same side, which must survive.
    sqlx::query("INSERT INTO player_game_stats VALUES ($1, $2, $3, 'natstat')")
        .bind(Uuid::new_v4())
        .bind(game)
        .bind(home)
        .execute(pool)
        .await
        .unwrap();
    Seed { home, away }
}

/// A `playerperfs` page carrying lines for `teams` in game G1.
fn page(teams: &[&str]) -> serde_json::Value {
    let lines: serde_json::Map<String, serde_json::Value> = teams
        .iter()
        .enumerate()
        .map(|(i, t)| {
            (
                format!("p{i}"),
                serde_json::json!({"game-code": "G1", "team-code": t, "player-code": "1"}),
            )
        })
        .collect();
    serde_json::json!({ "meta": {}, "performances": lines })
}

async fn count(pool: &PgPool, table: &str, team: Uuid, source: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE team_id = $1 AND source = $2"
    ))
    .bind(team)
    .bind(source)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn natstat_lines_clear_the_fill_for_their_side_only() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    let s = seed(&pool, "Final").await;
    // NatStat's page carries Notre Dame's lines only.
    let n = clear_cbbd_rows(&pool, BoxTable::Players, SEASON, &[page(&["ND", "ND"])])
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(count(&pool, "player_game_stats", s.home, "cbbd").await, 0);
    assert_eq!(
        count(&pool, "player_game_stats", s.home, "natstat").await,
        1
    );
    assert_eq!(count(&pool, "player_game_stats", s.away, "cbbd").await, 2);
    // The player page leaves the team rows to the team page.
    assert_eq!(count(&pool, "team_game_stats", s.home, "cbbd").await, 1);
    let n = clear_cbbd_rows(&pool, BoxTable::Teams, SEASON, &[page(&["ND", "UK"])])
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(count(&pool, "team_game_stats", s.away, "cbbd").await, 0);
}

#[tokio::test]
async fn a_game_natstat_will_skip_keeps_its_fill() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    // NatStat's lines for a game it still lists in progress are skipped
    // (`GameInProgress`), so clearing the fill would leave the game with none.
    let s = seed(&pool, "H2 05:11").await;
    let n = clear_cbbd_rows(&pool, BoxTable::Players, SEASON, &[page(&["ND", "UK"])])
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(count(&pool, "player_game_stats", s.home, "cbbd").await, 2);
    // Another season's team with the same code is not this side.
    let n = clear_cbbd_rows(&pool, BoxTable::Players, SEASON + 1, &[page(&["ND"])])
        .await
        .unwrap();
    assert_eq!(n, 0);
}
