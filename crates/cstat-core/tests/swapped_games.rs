//! Invariant for `compute::correct_swapped_games` (issue #119).
//!
//! NatStat occasionally swaps a game's two teams onto each other's identity —
//! the 2018 Champions Classic (game 1083775) was stored as Duke 84 / Kentucky 118
//! with Kentucky's roster under "Duke", when Duke actually won 118-84. The fix
//! detects fully-swapped 2-team games (each side >=80% the OTHER team's players by
//! reconciled `players.team_id`) and relabels them. After `compute_all` has run
//! for every season, the invariant is simply: NO such game remains — the detector
//! finds an empty set. A break means a swap slipped through (or a relabel created
//! a fresh inconsistency).
//!
//! Gated `#[ignore]` — needs a local DB with `compute` run for all seasons. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test swapped_games -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

const MIN_CROSS_SHARE: f64 = 0.80;

#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn no_fully_swapped_games_remain() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // The same detector `correct_swapped_games` uses: 2-team games where BOTH
    // sides are mostly the other team's players (by reconciled season team).
    let rows = sqlx::query(
        r#"
        WITH gt AS (
            SELECT pgs.game_id, pgs.team_id AS labeled, pl.team_id AS real_team, COUNT(*) AS n
            FROM player_game_stats pgs
            JOIN players pl ON pl.id = pgs.player_id
            GROUP BY pgs.game_id, pgs.team_id, pl.team_id
        ),
        two_team AS (
            SELECT game_id FROM team_game_stats GROUP BY game_id HAVING COUNT(DISTINCT team_id) = 2
        ),
        sides AS (
            SELECT game_id, labeled,
                   SUM(n) AS tot,
                   SUM(n) FILTER (WHERE real_team IS DISTINCT FROM labeled) AS mis
            FROM gt
            WHERE game_id IN (SELECT game_id FROM two_team)
            GROUP BY game_id, labeled
        )
        SELECT s.game_id, g.season, g.natstat_id
        FROM sides s
        JOIN games g ON g.id = s.game_id
        GROUP BY s.game_id, g.season, g.natstat_id
        HAVING COUNT(*) = 2 AND MIN(s.mis::float8 / NULLIF(s.tot, 0)) >= $1
        ORDER BY g.season
        "#,
    )
    .bind(MIN_CROSS_SHARE)
    .fetch_all(&pool)
    .await
    .unwrap();

    if !rows.is_empty() {
        for r in &rows {
            let season: i32 = r.get("season");
            let natstat_id: String = r.get("natstat_id");
            eprintln!("  still-swapped: season {season} game {natstat_id}");
        }
    }
    assert_eq!(
        rows.len(),
        0,
        "{} fully-swapped game(s) remain — correct_swapped_games did not run for their \
         season (or a relabel left a side cross-tagged)",
        rows.len()
    );
}
