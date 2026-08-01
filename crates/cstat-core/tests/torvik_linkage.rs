//! Invariant for Torvik → cstat player linkage (issue #243).
//!
//! `torvik_player_stats.player_id` is how a player's advanced metrics reach
//! the site: the player-SOS step in `compute_campom` resolves through it, so
//! an unlinked row ends with a NULL `cam_gbpm_v3_psos` — the column the
//! leaderboard sorts on. Before the matcher's fallback passes, 1,814 rows
//! (1,305 at rotation minutes) sat unlinked, taking Obi Toppin's 2020 AP
//! Player of the Year season, Ja Morant's 2019 and Johnny Davis's 2022 off
//! the board without a single error being raised.
//!
//! The residual after the fix is upstream coverage — players NatStat never
//! ingested at all, e.g. Anthony Barber's 2015 N.C. State season — so the
//! assertion is a *share* ceiling rather than "zero unlinked". The detector
//! lives in `cstat_core::invariants` so this test and the production check
//! can't drift apart.
//!
//! Gated `#[ignore]` — needs a local DB with Torvik ingested for every
//! season. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test torvik_linkage -- --ignored --nocapture

use cstat_core::invariants::{self, TORVIK_UNLINKED_ROTATION_MAX_SHARE};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn pool_and_seasons() -> (PgPool, Vec<i32>) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let seasons: Vec<i32> =
        sqlx::query_scalar("SELECT DISTINCT season FROM torvik_player_stats ORDER BY season")
            .fetch_all(&pool)
            .await
            .unwrap();
    (pool, seasons)
}

#[tokio::test]
#[ignore = "needs local DB with Torvik ingested for all seasons"]
async fn unlinked_torvik_rotation_players_stay_under_ceiling() {
    let (pool, seasons) = pool_and_seasons().await;
    assert!(
        !seasons.is_empty(),
        "no torvik_player_stats rows — run `cstat-ingest torvik --year YYYY` first"
    );

    let mut violations = Vec::new();
    for season in seasons {
        if let Some(v) = invariants::torvik_rows_unlinked(&pool, season)
            .await
            .unwrap()
        {
            eprintln!("  season {season} — {v}");
            violations.push((season, v));
        }
    }
    assert!(
        violations.is_empty(),
        "{} season(s) over the {:.0}% unlinked-rotation ceiling — the Torvik name matcher \
regressed, or a season's roster ingest is incomplete. Inspect with:\n  \
SELECT player_name, team_name, gbpm FROM torvik_player_stats\n   \
WHERE season = ? AND player_id IS NULL AND total_minutes >= 10 ORDER BY gbpm DESC;",
        violations.len(),
        TORVIK_UNLINKED_ROTATION_MAX_SHARE * 100.0,
    );
}

/// The bug's headline casualties, asserted by name. A share ceiling can drift
/// upward without anyone noticing which players it let through; these four
/// are the ones that made the failure visible in the first place.
#[tokio::test]
#[ignore = "needs local DB with Torvik ingested for all seasons"]
async fn award_winning_seasons_are_linked() {
    let (pool, _) = pool_and_seasons().await;

    // (season, torvik team, cstat name) — the cstat name is the legal one
    // NatStat stores, which is exactly why the exact-name pass missed them.
    let expected = [
        (2020, "Dayton", "Obadiah Toppin"),
        (2019, "Murray St.", "Temetrius Morant"),
        (2022, "Wisconsin", "Jonathan Davis"),
        (2015, "Ohio St.", "D'Angelo Russell"),
    ];

    let mut missing = Vec::new();
    for (season, team, name) in expected {
        let linked: Option<String> = sqlx::query_scalar(
            "SELECT p.name FROM torvik_player_stats t \
               JOIN players p ON p.id = t.player_id \
              WHERE t.season = $1 AND t.team_name = $2 AND p.name = $3",
        )
        .bind(season)
        .bind(team)
        .bind(name)
        .fetch_optional(&pool)
        .await
        .unwrap();
        if linked.is_none() {
            missing.push(format!("{name} ({team} {season})"));
        }
    }
    assert!(
        missing.is_empty(),
        "Torvik rows not linked to their cstat player: {}",
        missing.join(", ")
    );
}
