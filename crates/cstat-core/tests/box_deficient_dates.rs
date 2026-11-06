//! `box_deficient_dates` against real rows: the nightly's per-game box heal
//! re-pulls exactly what this reports, so a hole it misses is never repaired.
//!
//! Each test opens its hole inside a transaction it rolls back, on a pool pinned
//! to one connection so the function under test sees the uncommitted change.
//!
//! Gated `#[ignore]` — needs a local DB with a played season. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test box_deficient_dates -- --ignored

use chrono::NaiveDate;
use cstat_core::invariants::box_deficient_dates;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn pinned_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .unwrap()
}

/// The newest completed game with a full box, two-sided or against a team we
/// do not carry.
async fn healthy_game(pool: &PgPool, two_sided: bool) -> (Uuid, i32, NaiveDate) {
    sqlx::query_as(
        "SELECT g.id, g.season, g.game_date FROM games g
          WHERE g.home_score IS NOT NULL AND g.away_score IS NOT NULL
            AND g.home_team_id IS NOT NULL
            AND (g.away_team_id IS NOT NULL) = $1
            AND (SELECT count(*) FROM team_game_stats t WHERE t.game_id = g.id)
                = CASE WHEN $1 THEN 2 ELSE 1 END
            AND EXISTS (SELECT 1 FROM player_game_stats p WHERE p.game_id = g.id)
          ORDER BY g.game_date DESC LIMIT 1",
    )
    .bind(two_sided)
    .fetch_one(pool)
    .await
    .expect("a completed game with a full box")
}

async fn short_games(pool: &PgPool, season: i32, date: NaiveDate) -> i64 {
    box_deficient_dates(pool, season, date)
        .await
        .unwrap()
        .iter()
        .find(|d| d.date == date)
        .map_or(0, |d| d.games)
}

/// Run `sql` against `game` inside a transaction and report how many games on
/// its date read as short before and after, rolling the change back.
async fn short_before_and_after(two_sided: bool, sql: &str) -> (i64, i64) {
    let pool = pinned_pool().await;
    let (game, season, date) = healthy_game(&pool, two_sided).await;
    let before = short_games(&pool, season, date).await;
    sqlx::query("BEGIN").execute(&pool).await.unwrap();
    sqlx::query(sql).bind(game).execute(&pool).await.unwrap();
    let after = short_games(&pool, season, date).await;
    sqlx::query("ROLLBACK").execute(&pool).await.unwrap();
    (before, after)
}

/// How many games read as short on the sample game's date with nothing changed.
async fn short_after_rollback(two_sided: bool) -> i64 {
    let pool = pinned_pool().await;
    let (_, season, date) = healthy_game(&pool, two_sided).await;
    short_games(&pool, season, date).await
}

#[tokio::test]
#[ignore = "needs local DB with a played season"]
async fn a_game_with_no_team_rows_is_short() {
    let (before, after) =
        short_before_and_after(true, "DELETE FROM team_game_stats WHERE game_id = $1").await;
    assert_eq!(after, before + 1);
}

#[tokio::test]
#[ignore = "needs local DB with a played season"]
async fn a_game_missing_one_side_is_short() {
    let (before, after) = short_before_and_after(
        true,
        "DELETE FROM team_game_stats WHERE ctid = \
           (SELECT ctid FROM team_game_stats WHERE game_id = $1 LIMIT 1)",
    )
    .await;
    assert_eq!(after, before + 1);
}

#[tokio::test]
#[ignore = "needs local DB with a played season"]
async fn a_game_with_no_player_rows_is_short() {
    let (before, after) =
        short_before_and_after(true, "DELETE FROM player_game_stats WHERE game_id = $1").await;
    assert_eq!(after, before + 1);
}

/// The case `completed_game_missing_team_stats` cannot see: the opponent is not
/// a team we carry, so the game only ever has one side.
#[tokio::test]
#[ignore = "needs local DB with a played season"]
async fn a_one_sided_game_is_short_only_when_its_side_is_missing() {
    let (before, after) =
        short_before_and_after(false, "DELETE FROM team_game_stats WHERE game_id = $1").await;
    assert_eq!(after, before + 1);

    assert_eq!(before, short_after_rollback(false).await);
}

#[tokio::test]
#[ignore = "needs local DB with a played season"]
async fn since_is_inclusive_and_bounds_the_scan() {
    let pool = pinned_pool().await;
    let (game, season, date) = healthy_game(&pool, true).await;
    sqlx::query("BEGIN").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM team_game_stats WHERE game_id = $1")
        .bind(game)
        .execute(&pool)
        .await
        .unwrap();
    let at = box_deficient_dates(&pool, season, date).await.unwrap();
    let after = box_deficient_dates(&pool, season, date.succ_opt().unwrap())
        .await
        .unwrap();
    sqlx::query("ROLLBACK").execute(&pool).await.unwrap();
    assert!(at.iter().any(|d| d.date == date));
    assert!(!after.iter().any(|d| d.date == date));
}
