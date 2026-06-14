//! Invariant for the surgical corruption gate in `compute_pbp_lineups`
//! (issue #119 dig — 2019 lineup recovery).
//!
//! 2019's NatStat PBP export mis-tagged made FGs as FTs, so tagged FGA covers
//! only ~56% of box FGA and `pbp_source_is_corrupt` fires. The OLD behavior
//! discarded all SUB-replay for such a season, collapsing the lineup waffle to
//! near-nothing (Duke 2019: a single coherent game, 9-min top lineup, no Zion).
//!
//! The fix keeps replay for corrupt seasons — membership (SUB events), minutes
//! (clock), and points/plus-minus (the running score field) are all immune to the
//! tag corruption — and repairs the one bad dimension (possessions) by rescaling
//! each stint to the clean box-possession total per (game, team). So for any
//! corrupt season with PBP loaded:
//!   * `lineup_aggregates` is NON-empty with real minutes (replay ran, not cleared), AND
//!   * lineups with logged possessions carry a SANE per-100 rating (rescaled, not
//!     the ~1.5x-inflated raw-possession garbage, and not NULL), AND
//!   * `plus_minus` is populated (clean, score-derived).
//!
//! A break means the gate regressed to clear-everything, or the possession
//! rescale stopped firing (ratings would be absent or implausibly large).
//!
//! Gated `#[ignore]` — needs a local DB with a corrupt season's PBP loaded + compute
//! run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test corrupt_season_lineups -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

const FGA_COVERAGE_CORRUPT_THRESHOLD: f64 = 0.80;

#[tokio::test]
#[ignore = "needs local DB with a corrupt season's PBP loaded + compute run"]
async fn corrupt_seasons_recover_lineups_with_rescaled_ratings() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Seasons whose tagged FGA covers < 80% of box FGA (the same signal
    // `pbp_source_is_corrupt` keys on) AND that have play-by-play loaded.
    let corrupt_seasons: Vec<i32> = sqlx::query(
        "WITH pbp AS (
             SELECT season, count(*) FILTER (WHERE 'FGA' = ANY(tags) OR '3FA' = ANY(tags)) AS pbp_fga
             FROM play_by_play GROUP BY season
         ),
         box AS (SELECT season, sum(fga) AS box_fga FROM team_game_stats GROUP BY season)
         SELECT b.season
         FROM box b JOIN pbp p ON p.season = b.season
         WHERE b.box_fga > 0 AND p.pbp_fga::float8 / b.box_fga < $1",
    )
    .bind(FGA_COVERAGE_CORRUPT_THRESHOLD)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.get::<i32, _>("season"))
    .collect();

    if corrupt_seasons.is_empty() {
        eprintln!("no corrupt-PBP season loaded — nothing to assert (skipping)");
        return;
    }

    for season in corrupt_seasons {
        let row = sqlx::query(
            "SELECT
                count(*) AS total,
                count(*) FILTER (WHERE possessions_for > 0 AND possessions_against > 0) AS ratable,
                count(*) FILTER (WHERE possessions_for > 0 AND possessions_against > 0
                                   AND net_rtg IS NOT NULL) AS with_rating,
                -- Only DISPLAYED lineups must be sane: the waffle shows top-by-minutes
                -- per team. Deep-bench sub-20-min lineups carry small-sample rating
                -- noise in EVERY season (non-corrupt 2020 has ~8k >200 ratings too),
                -- so gating on all lineups would be a false signal — gate on >=20 min.
                count(*) FILTER (WHERE minutes >= 20 AND (ortg > 200 OR drtg > 200)) AS implausible_rating,
                count(*) FILTER (WHERE plus_minus <> 0) AS with_pm,
                COALESCE(max(minutes), 0) AS max_minutes
             FROM lineup_aggregates WHERE season = $1",
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap();

        let total: i64 = row.get("total");
        let ratable: i64 = row.get("ratable");
        let with_rating: i64 = row.get("with_rating");
        let implausible: i64 = row.get("implausible_rating");
        let with_pm: i64 = row.get("with_pm");
        let max_minutes: f64 = row.get("max_minutes");

        // Replay ran (not cleared): a real season has many lineups and a top
        // lineup well past the old 1-game ~9-minute collapse.
        assert!(
            total > 0 && max_minutes > 30.0,
            "corrupt season {season}: lineup_aggregates collapsed ({total} lineups, \
             {max_minutes:.1}-min top) — replay was discarded instead of recovered"
        );
        // Possessions were rescaled to box, so lineups with a two-sided possession
        // sample carry a real rating (not NULL).
        assert_eq!(
            with_rating,
            ratable,
            "corrupt season {season}: {} of {ratable} ratable lineups lack a net_rtg — \
             the box-possession rescale didn't run",
            ratable - with_rating
        );
        // …and the rescaled ratings are physically plausible for DISPLAYED (>=20
        // min) lineups — raw tag-undercounted possessions would inflate these well
        // past 200 (pre-rescale 2019 had 235 such; the seconds-proportional
        // estimate drops it to 0).
        assert_eq!(
            implausible, 0,
            "corrupt season {season}: {implausible} display-grade (>=20 min) lineup(s) have a \
             >200 per-100 rating — possessions look un-rescaled (raw tag-undercounted)"
        );
        // Clean score-derived +/- preserved.
        assert!(
            with_pm > 0,
            "corrupt season {season}: no lineup has a non-zero plus_minus — the clean \
             score-derived +/- was lost"
        );
    }
}
