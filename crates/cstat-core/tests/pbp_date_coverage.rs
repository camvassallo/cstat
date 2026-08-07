//! Calibration guard for the play-by-play date-coverage invariant (issue #247).
//!
//! `pbp_date_coverage_gap` exists to make a lost `playbyplay` night visible, and
//! its whole value rests on the thresholds being right. PBP coverage is never
//! complete — NatStat simply never publishes some games, at 94–97% of a recent
//! season — so a per-*game* check would report hundreds of violations a night
//! and be worth nothing. Per *date* there is real separation: across 2021–2026
//! no game date sits below 66% coverage and not one is at zero, while the
//! failure this catches lands at 0%.
//!
//! That separation is a property of the data, not of the code, so it is asserted
//! here rather than assumed. If a future season's feed degrades to the point
//! where honest nights dip under the floor, this fails and the constant gets
//! revisited deliberately — instead of the nightly quietly crying wolf until
//! everyone learns to skim past the warnings line.
//!
//! **2019 and 2020 are excluded on purpose.** Those seasons were bootstrapped
//! from the CSV export, whose opening weeks genuinely arrive at 39–60% coverage
//! (2018-11-06/07/08, 2019-11-07). The check would fire on them, correctly — but
//! the nightly only ever runs it against the current season, so they are not
//! what the floor is calibrated for.
//!
//! Gated `#[ignore]` — needs a local DB with PBP ingested. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test pbp_date_coverage -- --ignored --nocapture

use cstat_core::invariants::{self, PBP_DATE_MIN_COVERAGE, PBP_DATE_MIN_GAMES};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// First season ingested through the API path rather than the CSV bootstrap.
const FIRST_API_ERA_SEASON: i32 = 2021;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    PgPoolOptions::new().connect(&url).await.unwrap()
}

#[tokio::test]
#[ignore = "needs local DB with play-by-play ingested"]
async fn api_era_seasons_have_no_pbp_date_gaps() {
    let pool = pool().await;
    let seasons: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT g.season FROM games g \
          WHERE g.season >= $1 \
            AND EXISTS (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id) \
          ORDER BY 1",
    )
    .bind(FIRST_API_ERA_SEASON)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        !seasons.is_empty(),
        "no play-by-play ingested — run `cstat-ingest playbyplay` first"
    );

    let mut violations = Vec::new();
    for season in seasons {
        if let Some(v) = invariants::pbp_date_coverage_gap(&pool, season)
            .await
            .unwrap()
        {
            eprintln!("  season {season} — {v}");
            violations.push(v);
        }
    }
    assert!(
        violations.is_empty(),
        "{} season(s) contain a game date under the {:.0}% PBP-coverage floor. \
         Either a `playbyplay` night was lost and needs \
         `cstat-ingest playbyplay --from X --to Y`, or the feed's honest coverage \
         has drifted down into the floor and PBP_DATE_MIN_COVERAGE needs \
         re-deriving. Inspect with:\n  \
         SELECT g.game_date, count(*) AS games, count(*) FILTER (WHERE EXISTS \
         (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id)) AS with_pbp\n   \
         FROM games g WHERE g.season = ? AND g.home_score IS NOT NULL\n   \
         GROUP BY 1 ORDER BY 3::float8 / 2 ASC LIMIT 10;",
        violations.len(),
        PBP_DATE_MIN_COVERAGE * 100.0,
    );
}

/// The floor is only meaningful with headroom under it. Assert the margin
/// directly: a check that passes because real coverage sits at 51% is one bad
/// week from firing every night, and nobody would know until it did.
#[tokio::test]
#[ignore = "needs local DB with play-by-play ingested"]
async fn worst_honest_game_date_clears_the_floor_with_margin() {
    let pool = pool().await;

    // Worst per-date coverage across the API era, over dates with a real slate
    // (the min-games floor exists because a one-game date's share is a coin
    // flip — the only 0% date in twelve seasons had exactly one game on it).
    let worst: Option<f64> = sqlx::query_scalar(
        "WITH d AS ( \
             SELECT g.game_date, count(*) AS games, \
                    count(*) FILTER ( \
                        WHERE EXISTS (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id) \
                    ) AS with_pbp \
             FROM games g \
             WHERE g.season >= $1 AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL \
             GROUP BY g.game_date \
         ) \
         SELECT min(with_pbp::float8 / games) FROM d WHERE games >= $2",
    )
    .bind(FIRST_API_ERA_SEASON)
    .bind(PBP_DATE_MIN_GAMES)
    .fetch_one(&pool)
    .await
    .unwrap();

    let worst = worst.expect("no completed games with a full slate in the API era");
    eprintln!(
        "worst API-era game-date PBP coverage: {:.1}% (floor {:.0}%)",
        worst * 100.0,
        PBP_DATE_MIN_COVERAGE * 100.0
    );

    // 10 points of separation. Measured at 66% against a 50% floor when the
    // check was written, so this has room to absorb normal feed drift while
    // still failing before the constant becomes a false-positive generator.
    const MIN_MARGIN: f64 = 0.10;
    assert!(
        worst >= PBP_DATE_MIN_COVERAGE + MIN_MARGIN,
        "worst honest game date is at {:.1}% PBP coverage, within {:.0} points of \
         the {:.0}% floor — the invariant is about to start firing on real data. \
         Re-derive PBP_DATE_MIN_COVERAGE from the current distribution rather \
         than letting the nightly warn every night.",
        worst * 100.0,
        MIN_MARGIN * 100.0,
        PBP_DATE_MIN_COVERAGE * 100.0,
    );
}
