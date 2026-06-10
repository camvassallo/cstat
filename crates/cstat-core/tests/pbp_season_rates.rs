//! Reconciliation invariants for the Tier-1 PBP season-rate rollup + their
//! percentiles (migration 036; `compute_pbp_aggregates` / `compute_player_percentiles`).
//!
//! The rollup folds the per-game tag columns on `player_game_stats` into RATE
//! forms on `player_season_stats` (paint share, rim/jumper FG%, per-40 context
//! scoring), and `player_percentiles` ranks them within the season. A handful of
//! invariants must hold for every row regardless of the (season-varying) tag
//! density the rates are meant to be robust to:
//!   * share rates (paint_rate, paint_fg_pct, perimeter_fg_pct) live in [0, 1];
//!   * per-40 context rates are non-negative;
//!   * percentiles live in [0, 1] and are NULL exactly when their rate is NULL
//!     (the no-PBP tail is badge-less, never ranked into the scale);
//!   * paint_rate recomputed from the raw per-game tag sums matches the stored
//!     rollup (the aggregate didn't drift from its source).
//!
//! Gated `#[ignore]` — needs a local DB with PBP loaded and `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test pbp_season_rates -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB with PBP loaded + compute run"]
async fn pbp_season_rates_reconcile() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let total: i64 =
        sqlx::query("SELECT count(*) FROM player_season_stats WHERE paint_rate IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(
        total > 0,
        "no PBP season rates — run `compute` for a PBP season first"
    );

    // (1) Share rates are bounded to [0, 1] — paint_rate is a share of FGA,
    // paint/perimeter FG% are makes/attempts, none can leave the unit interval.
    let bad_share: i64 = sqlx::query(
        "SELECT count(*) FROM player_season_stats \
         WHERE paint_rate       NOT BETWEEN 0 AND 1 \
            OR paint_fg_pct     NOT BETWEEN 0 AND 1 \
            OR perimeter_fg_pct NOT BETWEEN 0 AND 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        bad_share, 0,
        "{bad_share} rows with a share rate outside [0,1]"
    );

    // (2) Per-40 context-scoring rates are non-negative (counting stats / time).
    let neg_rate: i64 = sqlx::query(
        "SELECT count(*) FROM player_season_stats \
         WHERE transition_pts_per40 < 0 OR second_chance_pts_per40 < 0 \
            OR points_off_turnovers_per40 < 0 OR fouls_drawn_per40 < 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(neg_rate, 0, "{neg_rate} rows with a negative per-40 rate");

    // (3) Percentiles are bounded [0, 1].
    let bad_pct: i64 = sqlx::query(
        "SELECT count(*) FROM player_percentiles \
         WHERE paint_rate_pct                 NOT BETWEEN 0 AND 1 \
            OR paint_fg_pct_pct               NOT BETWEEN 0 AND 1 \
            OR perimeter_fg_pct_pct           NOT BETWEEN 0 AND 1 \
            OR transition_pts_per40_pct       NOT BETWEEN 0 AND 1 \
            OR second_chance_pts_per40_pct    NOT BETWEEN 0 AND 1 \
            OR points_off_turnovers_per40_pct NOT BETWEEN 0 AND 1 \
            OR fouls_drawn_per40_pct          NOT BETWEEN 0 AND 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(bad_pct, 0, "{bad_pct} percentile rows outside [0,1]");

    // (4) A ranked player must have the underlying rate — the no-PBP tail is
    // ranked over non-NULL values only, so it's badge-less rather than read as
    // the top of the scale. EXISTS, not a plain JOIN: player_season_stats has one
    // row PER TEAM, and the percentile is computed off the primary-team row
    // (DISTINCT ON games_played DESC), so a transfer's other-team row can legally
    // carry a NULL paint_rate while the ranked primary row does not.
    let orphan_pct: i64 = sqlx::query(
        "SELECT count(*) FROM player_percentiles pp \
         WHERE pp.paint_rate_pct IS NOT NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM player_season_stats pss \
             WHERE pss.player_id = pp.player_id AND pss.season = pp.season \
               AND pss.paint_rate IS NOT NULL \
           )",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        orphan_pct, 0,
        "{orphan_pct} percentile rows ranked without an underlying rate"
    );

    // (5) Reconciliation: paint_rate recomputed straight from the per-game tag
    // sums must match the stored rollup (1e-6 float tolerance). Denominator is
    // the PBP total (paint + perimeter), matching the compute — NOT box `fga`,
    // which disagrees with the tag stream. Catches drift from the source.
    let drift: i64 = sqlx::query(
        "WITH recomputed AS ( \
            SELECT player_id, team_id, season, \
                   sum(paint_fga)::double precision / nullif(sum(paint_fga + perimeter_fga), 0) AS paint_rate \
            FROM player_game_stats \
            WHERE paint_fga IS NOT NULL \
            GROUP BY player_id, team_id, season \
         ) \
         SELECT count(*) FROM player_season_stats pss \
         JOIN recomputed r \
           ON r.player_id = pss.player_id AND r.team_id = pss.team_id AND r.season = pss.season \
         WHERE pss.paint_rate IS DISTINCT FROM NULL \
           AND r.paint_rate IS DISTINCT FROM NULL \
           AND abs(pss.paint_rate - r.paint_rate) > 1e-6",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        drift, 0,
        "{drift} rows where stored paint_rate != recomputed from per-game sums"
    );

    eprintln!("pbp_season_rates: {total} rate rows, all invariants hold");
}
