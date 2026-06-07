//! Possession parity check for the P3 stint possession derivation.
//!
//! The per-stint possession estimate (`(FGA + 3FA) - ORB + TOV + 0.44*FTA`,
//! summed over a team's stints in a game) should land close to the box-score
//! possession estimate for the same team-game — they use the same formula, so
//! the only gap is plays that fall outside a tracked stint window (off-five
//! replay drift, NULL-team marker rows). A small undercount is expected; a large
//! one means the stint windows or tag attribution drifted.
//!
//! Gated `#[ignore]` — needs a local DB with PBP loaded and `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test pbp_possession_parity -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

/// Tolerance: mean absolute error must stay under 8% of box-score possessions.
/// Observed 1.9-7.8% across 2015-2026 (ex-2019); the headroom absorbs
/// season-to-season replay-quality variation without letting a real regression
/// through.
const MAX_MAE_PCT: f64 = 8.0;

/// Seasons whose **source** PBP tag stream is corrupt, so possession parity
/// can't be expected to hold (and the derived rate stats shouldn't be trusted).
/// 2019: NatStat's 2019 Play-by-Play export mis-encodes made field goals as
/// free throws — "made layup" rows carry `Points=1` + `FTA|FTM` tags instead of
/// `FGA|FGM` (verified in the raw CSV; the 2020 export tags the same play
/// correctly). This halves 2019's FGA/FGM and inflates FTA, so its possessions
/// run ~36% low. The box scores (a separate CSV) are unaffected. See ROADMAP
/// "2019 PBP tag corruption".
const CORRUPT_PBP_SEASONS: &[i32] = &[2019];

#[tokio::test]
#[ignore = "needs local DB with PBP loaded + compute run"]
async fn stint_possessions_track_box_score() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Seasons whose stints have been re-derived with P3 possessions. A season
    // with all-zero possessions predates the P3 recompute (the migration's
    // default) — not "drift", just not rederived yet, so it's out of scope here.
    let seasons: Vec<i32> = sqlx::query(
        "SELECT season FROM lineup_stints GROUP BY season \
         HAVING sum(possessions_for) > 0 ORDER BY season",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.get::<i32, _>("season"))
    .filter(|s| !CORRUPT_PBP_SEASONS.contains(s))
    .collect();
    assert!(
        !seasons.is_empty(),
        "no season has P3 possessions populated — run `compute` (the derivation may be broken)"
    );

    let mut evaluated = 0;
    for season in seasons {
        let row = sqlx::query(
            "WITH pbp AS (
                 SELECT game_id, team_id, sum(possessions_for) poss
                 FROM lineup_stints WHERE season = $1 GROUP BY game_id, team_id
             ),
             box AS (
                 SELECT game_id, team_id, fga - off_rebounds + turnovers + 0.44 * fta poss
                 FROM team_game_stats WHERE season = $1 AND fga IS NOT NULL
             )
             SELECT count(*) n,
                    avg(box.poss)::float8 avg_box,
                    avg(abs(pbp.poss - box.poss))::float8 mae,
                    avg(pbp.poss - box.poss)::float8 mean_err
             FROM pbp JOIN box USING (game_id, team_id)",
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap();

        let n: i64 = row.get("n");
        if n == 0 {
            continue;
        }
        let avg_box: f64 = row.get("avg_box");
        let mae: f64 = row.get("mae");
        let mean_err: f64 = row.get("mean_err");
        let mae_pct = mae / avg_box * 100.0;
        println!(
            "season {season}: n={n} avg_box={avg_box:.1} MAE={mae:.2} ({mae_pct:.1}%) mean_err={mean_err:.2}"
        );
        assert!(
            mae_pct < MAX_MAE_PCT,
            "season {season}: possession MAE {mae_pct:.1}% exceeds {MAX_MAE_PCT}% — stint windows or tag attribution drifted"
        );
        evaluated += 1;
    }
    assert!(
        evaluated > 0,
        "no season had matching team-games to evaluate"
    );

    // The corruption gate (compute_pbp_lineups) must leave a known-corrupt
    // season with no served aggregates — better an absent surface than a wrong
    // one. Guards against the gate silently regressing and re-publishing 2019.
    for &season in CORRUPT_PBP_SEASONS {
        let n: i64 = sqlx::query("SELECT count(*) FROM lineup_aggregates WHERE season = $1")
            .bind(season)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            n, 0,
            "corrupt season {season} still has {n} served lineup_aggregates rows — the coverage gate didn't clear it"
        );
        println!("corrupt season {season}: lineup_aggregates cleared (0 rows) ✓");
    }
}
