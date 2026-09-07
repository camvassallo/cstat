//! Guards on `invariants::UPSTREAM_TEAM_STATS_GAPS` — the hand-verified list of
//! games NatStat never published team box scores for (#232).
//!
//! An exclusion list is only safe while every entry is still earning its place.
//! Two ways it goes wrong, and one test each:
//!
//! - **It rots.** NatStat backfills a game, or an ingest fix fills it, and the
//!   entry now silences nothing while still claiming a permanent upstream hole.
//!   Harmless today, misleading the next time someone reads it as evidence.
//! - **It over-reaches.** Someone adds an id to quiet a nightly without running
//!   the feed check, and a real pipeline drop is deleted from the only channel
//!   that would have reported it. This cannot be tested from the DB — it is why
//!   the const's doc comment spells out the three-path verification — but the
//!   list staying *small and shrinking* is the observable proxy.
//!
//! Gated `#[ignore]` — needs a local DB with all seasons ingested. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test invariants_known_gaps -- --ignored --nocapture

use cstat_core::invariants::{self, UPSTREAM_TEAM_STATS_GAPS};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    PgPoolOptions::new().connect(&url).await.unwrap()
}

/// Every listed game must still exist, still be completed with both teams
/// resolved, and still be short of its two `team_game_stats` rows — i.e. it
/// would still trip the check if it weren't excluded. An entry that no longer
/// would is dead weight and should be deleted.
#[tokio::test]
#[ignore = "needs local DB with all seasons ingested"]
async fn every_known_gap_is_still_needed() {
    let pool = pool().await;
    let mut stale = Vec::new();

    for (season, natstat_id) in UPSTREAM_TEAM_STATS_GAPS {
        let row: Option<(i64,)> = sqlx::query_as(
            r#"
            SELECT (SELECT COUNT(*) FROM team_game_stats tgs WHERE tgs.game_id = g.id)
            FROM games g
            WHERE g.season = $1
              AND g.natstat_id = $2
              AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
              AND g.home_team_id IS NOT NULL AND g.away_team_id IS NOT NULL
            "#,
        )
        .bind(season)
        .bind(natstat_id)
        .fetch_optional(&pool)
        .await
        .unwrap();

        match row {
            None => stale.push(format!(
                "{season}/{natstat_id}: no longer a completed game with two resolved teams \
                 (deleted, unresolved, or never ingested) — the exclusion does nothing"
            )),
            Some((n,)) if n >= 2 => stale.push(format!(
                "{season}/{natstat_id}: now has {n} team_game_stats rows — upstream backfilled \
                 it, so drop this entry"
            )),
            Some(_) => {}
        }
    }

    assert!(
        stale.is_empty(),
        "UPSTREAM_TEAM_STATS_GAPS has {} stale entr(y/ies):\n  {}",
        stale.len(),
        stale.join("\n  ")
    );
}

/// The point of the list: with it applied, the check reads clean on every
/// ingested season. If this fails, either a genuinely new gap appeared (triage
/// it against the feed — see the const's doc comment) or the pipeline started
/// dropping team box scores. Both want a human, which is exactly what a clean
/// baseline buys.
#[tokio::test]
#[ignore = "needs local DB with all seasons ingested"]
async fn missing_team_stats_baseline_is_clean() {
    let pool = pool().await;
    let seasons: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT season FROM games ORDER BY season")
        .fetch_all(&pool)
        .await
        .unwrap();

    let mut found = Vec::new();
    for season in seasons {
        if let Some(v) = invariants::completed_game_missing_team_stats(&pool, season)
            .await
            .unwrap()
        {
            found.push(format!("season {season}: {v}"));
        }
    }

    assert!(
        found.is_empty(),
        "completed_game_missing_team_stats is no longer clean:\n  {}",
        found.join("\n  ")
    );
}
