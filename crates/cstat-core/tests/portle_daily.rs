//! Server-authoritative Portle daily-puzzle pin (issue #181).
//!
//! DB-gated (needs a local DB with a computed season):
//!   DATABASE_URL=... cargo test -p cstat-core --test portle_daily -- --ignored --nocapture
//!
//! Uses far-future sentinel dates so it never collides with a real player's pin,
//! and deletes its own rows afterward.

use cstat_core::queries::{self, PortleMode};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

const MODES: [PortleMode; 4] = [
    PortleMode::P5,
    PortleMode::Starters,
    PortleMode::Campom10,
    PortleMode::All,
];

#[tokio::test]
#[ignore = "needs local DB with a computed season (players + torvik + archetypes)"]
async fn daily_pin_is_idempotent_deterministic_and_in_pool() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Newest season that has an answerable pool (CamPom + archetype present).
    let season: i32 = sqlx::query(
        "SELECT max(pss.season)
         FROM player_season_stats pss
         JOIN torvik_player_stats tps ON tps.player_id = pss.player_id AND tps.season = pss.season
         JOIN player_archetypes pa ON pa.player_id = pss.player_id AND pa.season = pss.season
         WHERE tps.cam_gbpm_v3_psos IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get::<Option<i32>, _>(0)
    .expect("no answerable season locally — ingest a season + run compute first");

    // Sentinel dates far outside any real play window; cleaned up at the end.
    let d1 = chrono::NaiveDate::from_ymd_opt(2999, 1, 1).unwrap();
    let d2 = chrono::NaiveDate::from_ymd_opt(2999, 1, 2).unwrap();

    for mode in MODES {
        // First pin.
        let a = queries::pick_or_pin_daily_puzzle(&pool, mode, season, d1)
            .await
            .unwrap();
        let Some(natstat_id) = a else {
            // Empty pool for this mode/season is legal (e.g. a thin local DB);
            // nothing pinned, nothing to verify.
            eprintln!(
                "mode {} season {season}: empty pool, skipping",
                mode.as_str()
            );
            continue;
        };

        // Idempotent: a second call for the same day returns the SAME frozen id.
        let b = queries::pick_or_pin_daily_puzzle(&pool, mode, season, d1)
            .await
            .unwrap();
        assert_eq!(
            Some(&natstat_id),
            b.as_ref(),
            "mode {}: pin not idempotent",
            mode.as_str()
        );

        // Exactly one row was written for (mode, season, d1) — no duplicate.
        let cnt: i64 = sqlx::query(
            "SELECT count(*) FROM portle_daily_puzzle WHERE mode=$1 AND season=$2 AND puzzle_date=$3",
        )
        .bind(mode.as_str())
        .bind(season)
        .bind(d1)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert_eq!(
            cnt,
            1,
            "mode {}: expected exactly one pinned row",
            mode.as_str()
        );

        // Parity with filterPool: the pinned player must actually satisfy the
        // mode's eligibility (present, answerable, and mode-specific threshold),
        // or the client — which filters the same way — couldn't display it.
        let ok: bool = sqlx::query(&format!(
            "SELECT EXISTS (
               SELECT 1
               FROM player_season_stats pss
               JOIN players p ON p.id = pss.player_id AND p.season = pss.season
               LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
               LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = pss.season
               LEFT JOIN player_archetypes pa ON pa.player_id = pss.player_id AND pa.season = pss.season
               WHERE pss.season = $1 AND p.natstat_id = $2
                 AND pss.games_played >= 5 AND pss.minutes_per_game >= 10
                 AND tps.cam_gbpm_v3_psos IS NOT NULL AND pa.primary_class IS NOT NULL
                 {}
             )",
            mode_predicate(mode),
        ))
        .bind(season)
        .bind(&natstat_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(
            ok,
            "mode {}: pinned natstat_id {natstat_id} is not in the eligible pool (filter parity broke)",
            mode.as_str()
        );
    }

    // A different date generally yields a different pin (deterministic md5 spread).
    // Use the biggest pool (All) so the sample is large; tolerate the rare match.
    let all_d1 = queries::pick_or_pin_daily_puzzle(&pool, PortleMode::All, season, d1)
        .await
        .unwrap();
    let all_d2 = queries::pick_or_pin_daily_puzzle(&pool, PortleMode::All, season, d2)
        .await
        .unwrap();
    if let (Some(x), Some(y)) = (&all_d1, &all_d2) {
        assert_ne!(
            x, y,
            "two sentinel dates pinned the same player (suspicious for a large pool)"
        );
    }

    // Cleanup: remove only the sentinel rows this test created.
    sqlx::query("DELETE FROM portle_daily_puzzle WHERE puzzle_date IN ($1, $2)")
        .bind(d1)
        .bind(d2)
        .execute(&pool)
        .await
        .unwrap();
}

/// Mirror of `PortleMode::sql_predicate` for the parity assertion (that method is
/// private). Kept identical on purpose.
fn mode_predicate(mode: PortleMode) -> &'static str {
    match mode {
        PortleMode::P5 => {
            "AND t.conference IN ('ACC','BIG10','BIG12','SEC','BIGEAST') AND pss.minutes_per_game >= 20"
        }
        PortleMode::Starters => "AND pss.minutes_per_game >= 24",
        PortleMode::Campom10 => "AND tps.cam_gbpm_v3_psos > 10",
        PortleMode::All => "",
    }
}
