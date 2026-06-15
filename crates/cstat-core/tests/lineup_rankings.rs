//! Invariants for `queries::get_lineup_rankings` — the cross-team lineup
//! combination ranking behind `GET /api/lineups` and the Lineups tab.
//!
//! Guards the contract that's easy to silently break when the schedule-
//! adjustment / explosion SQL is touched:
//!   - the opponent-adjusted margin is internally consistent
//!     (`adj_net == adj_ortg − adj_drtg`),
//!   - rows come back ranked by `adj_net` descending (NULLs last),
//!   - each combo has exactly `size` players,
//!   - the `player` filter only returns combos containing that player.
//!
//! Gated `#[ignore]` — needs a local DB with lineups ingested + `compute` run.
//!   DATABASE_URL=... cargo test -p cstat-core --test lineup_rankings -- --ignored --nocapture

use cstat_core::queries;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB with lineups + compute for some season"]
async fn lineup_rankings_invariants() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Newest season that actually has lineup aggregates — keeps the test correct
    // regardless of which years are loaded locally.
    let season: i32 = sqlx::query("SELECT max(season) FROM lineup_aggregates")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get::<Option<i32>, _>(0)
        .expect("no lineup_aggregates rows — ingest lineups + run compute first");

    for size in [5_i32, 3, 2] {
        let rows = queries::get_lineup_rankings(&pool, season, size, 100.0, 25, None, None, false)
            .await
            .unwrap();
        assert!(
            !rows.is_empty(),
            "size {size}: no lineups ranked for season {season}"
        );

        let mut prev: Option<f64> = None;
        for r in &rows {
            // Combo arity matches the requested size, and the parallel arrays
            // stay aligned.
            assert_eq!(
                r.lineup.len(),
                size as usize,
                "size {size}: wrong combo arity"
            );
            assert_eq!(r.player_names.len(), size as usize);
            assert_eq!(r.player_classes.len(), size as usize);

            // adj_net is exactly adj_ortg − adj_drtg when all three are present.
            if let (Some(net), Some(o), Some(d)) = (r.adj_net, r.adj_ortg, r.adj_drtg) {
                assert!(
                    (o - d - net).abs() < 1e-6,
                    "size {size}: adj_net {net} != adj_ortg {o} − adj_drtg {d}"
                );
            }

            // Sorted by adj_net descending, NULLs last.
            if let Some(net) = r.adj_net {
                if let Some(p) = prev {
                    assert!(p >= net - 1e-9, "size {size}: not sorted by adj_net desc");
                }
                prev = Some(net);
            } else {
                // A NULL must not be followed by a non-NULL (NULLs sort last).
                prev = Some(f64::NEG_INFINITY);
            }
        }
    }

    // Most-used ordering (team-page panels): rows come back by minutes desc.
    let by_min = queries::get_lineup_rankings(&pool, season, 2, 0.0, 10, None, None, true)
        .await
        .unwrap();
    let mut prev_min = f64::INFINITY;
    for r in &by_min {
        assert!(r.minutes <= prev_min + 1e-9, "not sorted by minutes desc");
        prev_min = r.minutes;
    }

    // `player` filter: every returned 5-man combo must contain the filter player.
    let top = queries::get_lineup_rankings(&pool, season, 5, 100.0, 1, None, None, false)
        .await
        .unwrap();
    let pid = top[0].lineup[0];
    let filtered =
        queries::get_lineup_rankings(&pool, season, 5, 100.0, 25, Some(pid), None, false)
            .await
            .unwrap();
    assert!(!filtered.is_empty(), "player filter returned nothing");
    for r in &filtered {
        assert!(
            r.lineup.contains(&pid),
            "player-filtered combo is missing the filter player"
        );
    }
}
