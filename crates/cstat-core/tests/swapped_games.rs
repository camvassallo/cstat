//! Invariants for `compute::correct_swapped_games` (issue #119) and
//! `compute::repair_phantom_swapped_games` (issue #140).
//!
//! NatStat occasionally swaps a game's two teams onto each other's identity —
//! the 2018 Champions Classic (game 1083775) was stored as Duke 84 / Kentucky
//! 118 with Kentucky's roster under "Duke", when Duke actually won 118-84 —
//! and, in the harder variant, additionally mints a fresh per-game "phantom"
//! id for every player so the cross-tag detector sees no displacement. After
//! `compute_all` has run for every season, the invariant is simply: NO such
//! game remains.
//!
//! The detector queries live in `cstat_core::invariants` (shared with the
//! simulate replay harness and the future M5 nightly gates); these tests just
//! run them per ingested season against the local DB so the assertion and the
//! production check can never drift apart.
//!
//! Gated `#[ignore]` — needs a local DB with `compute` run for all seasons. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test swapped_games -- --ignored --nocapture

use cstat_core::invariants;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn pool_and_seasons() -> (PgPool, Vec<i32>) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let seasons: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT season FROM games ORDER BY season")
        .fetch_all(&pool)
        .await
        .unwrap();
    (pool, seasons)
}

#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn no_fully_swapped_games_remain() {
    let (pool, seasons) = pool_and_seasons().await;
    let mut violations = Vec::new();
    for season in seasons {
        if let Some(v) = invariants::fully_swapped_games_remain(&pool, season)
            .await
            .unwrap()
        {
            eprintln!("  still-swapped: season {season} — {v}");
            violations.push((season, v));
        }
    }
    assert!(
        violations.is_empty(),
        "{} season(s) with fully-swapped game(s) remaining — correct_swapped_games did not \
         run for them (or a relabel left a side cross-tagged)",
        violations.len()
    );
}

/// The precondition that makes the cross-season guards in BOTH
/// `correct_swapped_games` (issue #201) and `repair_phantom_swapped_games`
/// (issue #140) load-bearing: a single `game_id` carrying box rows in more
/// than one season (a NatStat duplicate whose typo'd date lands the same game
/// in two seasons). When that happens, a `game_id`-only self-join pairs this
/// season's row with the OTHER season's partner and corrupts its box; the
/// `t2.season = t1.season` / `opp.season = tgs.season` guards prevent it.
///
/// A non-empty result here is a data-hygiene problem (a backfill left a
/// cross-year duplicate) — investigate and clean it up. The season guards keep
/// it from corrupting the healthy season in the meantime, but the collision
/// itself should not persist.
#[tokio::test]
#[ignore = "needs local DB with box scores + compute run for all seasons"]
async fn no_cross_season_game_id_collisions() {
    let (pool, _) = pool_and_seasons().await;
    for table in ["team_game_stats", "player_game_stats"] {
        let colliding: Vec<(String, i64)> = sqlx::query_as(&format!(
            "SELECT game_id::text, count(DISTINCT season) \
             FROM {table} GROUP BY game_id HAVING count(DISTINCT season) > 1 \
             ORDER BY 2 DESC LIMIT 20"
        ))
        .fetch_all(&pool)
        .await
        .unwrap();
        for (game_id, n) in &colliding {
            eprintln!("  {table}: game_id {game_id} spans {n} seasons");
        }
        assert!(
            colliding.is_empty(),
            "{} game_id(s) in {table} carry box rows across multiple seasons — a cross-year \
             NatStat duplicate. The swap-corrector season guards prevent corruption, but the \
             colliding rows should be cleaned up.",
            colliding.len()
        );
    }
}

#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn no_phantom_swapped_games_remain() {
    let (pool, seasons) = pool_and_seasons().await;
    let mut violations = Vec::new();
    for season in seasons {
        if let Some(v) = invariants::phantom_swapped_games_remain(&pool, season)
            .await
            .unwrap()
        {
            eprintln!("  still-phantom-swapped: season {season} — {v}");
            violations.push((season, v));
        }
    }
    assert!(
        violations.is_empty(),
        "{} season(s) with phantom-swapped game(s) remaining — repair_phantom_swapped_games \
         did not run for them (or could not re-identify a side's phantoms)",
        violations.len()
    );
}
