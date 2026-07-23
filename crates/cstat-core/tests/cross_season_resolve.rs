//! Regression guard for the cross-season player resolution that powers
//! transfer-aware detail-page navigation.
//!
//! NatStat mints a fresh `natstat_id` per (player, team), so a pure
//! `natstat_id` join breaks the moment a player changes schools — the exact
//! failure the memory/CLAUDE notes flag ("natstat_id-only joins silently drop
//! ~25% of inbound transfers"). `queries::resolve_player_id_for_season` and
//! `get_player_available_seasons` are the two paths that must NOT break: both
//! fall back to Torvik's stable `torvik_pid`. The Python side has
//! `training/test_cross_season_joins.py`; this is the Rust analogue.
//!
//! DB-gated: uses whatever transfer case the local DB already holds (a
//! `torvik_pid` present in >=2 seasons under distinct `natstat_id`s), and
//! skips cleanly when `DATABASE_URL` is unset or no such case exists — so it
//! provides coverage on a populated dev/CI DB without being brittle.

use cstat_core::queries::{get_player_available_seasons, resolve_player_id_for_season};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "needs a populated local DB (torvik_player_stats + players across seasons); \
            run: DATABASE_URL=... cargo test -p cstat-core --test cross_season_resolve -- --ignored"]
async fn torvik_pid_resolves_a_transfer_across_seasons() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset; skipping cross-season resolver test");
        return;
    };
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Pull one real transfer case: a torvik_pid spanning >=2 seasons whose
    // `players` rows carry >=2 distinct natstat_ids (i.e. a genuine school
    // change, where the natstat-only branch cannot link the seasons). Fetch
    // every (season, player_id, natstat_id) for that pid.
    let rows = sqlx::query(
        r#"
        WITH xfer AS (
            SELECT t.torvik_pid
            FROM torvik_player_stats t
            JOIN players p ON p.id = t.player_id
            GROUP BY t.torvik_pid
            HAVING count(DISTINCT t.season) >= 2
               AND count(DISTINCT p.natstat_id) >= 2
            LIMIT 1
        )
        SELECT t.season, t.player_id, p.natstat_id
        FROM torvik_player_stats t
        JOIN players p ON p.id = t.player_id
        JOIN xfer ON xfer.torvik_pid = t.torvik_pid
        ORDER BY t.season
        "#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    if rows.len() < 2 {
        eprintln!("no cross-season transfer case in the DB; skipping");
        return;
    }

    // Pick two seasons whose natstat_ids differ — that's where a pure
    // natstat_id join would 404 and the torvik_pid fallback must carry it.
    let get = |r: &sqlx::postgres::PgRow| -> (i32, Uuid, String) {
        (r.get("season"), r.get("player_id"), r.get("natstat_id"))
    };
    let (season_a, player_a, nat_a) = get(&rows[0]);
    let Some((season_b, player_b, _nat_b)) = rows.iter().map(get).find(|(_, _, nat)| *nat != nat_a)
    else {
        eprintln!("case had no differing natstat_id after all; skipping");
        return;
    };
    assert_ne!(
        player_a, player_b,
        "distinct-season rows must be distinct player UUIDs"
    );

    // 1. resolve_player_id_for_season must map season A's row to season B's row
    //    via the torvik_pid fallback (the natstat branch cannot, natstat differs).
    let resolved = resolve_player_id_for_season(&pool, player_a, season_b)
        .await
        .unwrap();
    assert_eq!(
        resolved,
        Some(player_b),
        "torvik_pid fallback should resolve the {season_a}->{season_b} transfer \
         (player {player_a} -> {player_b}); natstat-only would have returned None"
    );

    // 2. get_player_available_seasons must surface BOTH seasons (the season
    //    picker on the detail page depends on this union).
    let seasons = get_player_available_seasons(&pool, player_a).await.unwrap();
    assert!(
        seasons.contains(&season_a) && seasons.contains(&season_b),
        "available seasons {seasons:?} must include both {season_a} and {season_b}"
    );
}
