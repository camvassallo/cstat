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

use chrono::NaiveDate;
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
         `cstat-ingest playbyplay --year YYYY --from X --to Y`, or the feed's \
         honest coverage has drifted down into the floor and \
         PBP_DATE_MIN_COVERAGE needs re-deriving. Inspect with:\n  \
         SELECT g.game_date, count(*) AS games, count(*) FILTER (WHERE EXISTS \
         (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id)) AS with_pbp\n   \
         FROM games g WHERE g.season = ? AND g.home_score IS NOT NULL\n   \
         GROUP BY 1 ORDER BY 3::float8 / 2 ASC LIMIT 10;",
        violations.len(),
        PBP_DATE_MIN_COVERAGE * 100.0,
    );
}

/// The nightly's self-heal scans this same function, so the `since` bound it
/// passes must narrow the *reported dates* without narrowing the season-wide
/// "does this deployment have PBP at all" gate. Getting that backwards would
/// make a lookback whose every date is missing suppress itself — silencing the
/// heal in exactly the case it exists for.
///
/// A healthy DB reports no deficient dates at all, so asserting over live data
/// proves nothing (the first version of this test did exactly that and passed
/// vacuously). It therefore *creates* a deficiency — wiping one settled date's
/// play-by-play inside a transaction it rolls back — on a single pinned
/// connection so the function under test sees the uncommitted state.
#[tokio::test]
#[ignore = "needs local DB with play-by-play ingested"]
async fn the_since_bound_narrows_dates_without_narrowing_the_gate() {
    // One connection, so BEGIN / the queries / ROLLBACK all share a session.
    // Nothing here can commit: the rollback is explicit, and a panic before it
    // drops the pool, which rolls back too.
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .unwrap();
    let season = 2026;

    let (first, last): (NaiveDate, NaiveDate) = sqlx::query_as(
        "SELECT min(game_date), max(game_date) FROM games \
          WHERE season = $1 AND home_score IS NOT NULL",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(first < last, "season {season} has no game span to bound");

    sqlx::query("BEGIN").execute(&pool).await.unwrap();

    // A settled date with a real slate, late enough that a mid-season cutoff
    // sits before it — so the bound has something to keep and something to drop.
    let victim: NaiveDate = sqlx::query_scalar(
        "SELECT g.game_date FROM games g \
          WHERE g.season = $1 AND g.home_score IS NOT NULL \
            AND g.game_date <= CURRENT_DATE - 2 \
          GROUP BY g.game_date HAVING count(*) >= 3 \
          ORDER BY g.game_date DESC LIMIT 1",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM play_by_play p USING games g \
          WHERE p.game_id = g.id AND g.season = $1 AND g.game_date = $2",
    )
    .bind(season)
    .bind(victim)
    .execute(&pool)
    .await
    .unwrap();

    let all = invariants::pbp_deficient_dates(&pool, season, None)
        .await
        .unwrap();
    assert!(
        all.iter().any(|d| d.date == victim),
        "the wiped date must be reported, or the rest of this test is vacuous"
    );

    // A `since` at the season's first game must change nothing — the assertion
    // that fails if the bound is ever moved inside the CTE feeding the
    // season-wide gate.
    let wide = invariants::pbp_deficient_dates(&pool, season, Some(first))
        .await
        .unwrap();
    assert_eq!(
        wide, all,
        "a `since` at the season's first game must match no bound at all"
    );

    // A cutoff after the wiped date must drop it, and the gate must still hold
    // (the season has PBP elsewhere), so the result is a strict filter.
    let after = victim + chrono::Duration::days(1);
    let bounded = invariants::pbp_deficient_dates(&pool, season, Some(after))
        .await
        .unwrap();
    assert!(
        !bounded.iter().any(|d| d.date == victim),
        "`since` must exclude dates before it"
    );
    assert_eq!(
        bounded,
        all.iter()
            .filter(|d| d.date >= after)
            .cloned()
            .collect::<Vec<_>>(),
        "the bounded result must be exactly the unbounded one filtered by date"
    );

    sqlx::query("ROLLBACK").execute(&pool).await.unwrap();

    // Belt and braces: the wipe is gone.
    let restored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM play_by_play p JOIN games g ON g.id = p.game_id \
          WHERE g.season = $1 AND g.game_date = $2",
    )
    .bind(season)
    .bind(victim)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(restored > 0, "ROLLBACK did not restore {victim}");
}

/// The zero-coverage arm reports a date at ANY slate size, with no min-games
/// floor under it — that is what makes a lost `playbyplay` night visible on a
/// light slate (early November, Dec 24–25, the day before the Final Four), where
/// the share test's `games >= 3` gate would drop it entirely.
///
/// It carries no floor because it needs none: across the API era there is not a
/// single game date at zero coverage. That is a property of the data, so it is
/// asserted rather than assumed — if a season ever lands one honestly, this arm
/// starts crying wolf and the design needs revisiting, not silencing.
#[tokio::test]
#[ignore = "needs local DB with play-by-play ingested"]
async fn no_api_era_game_date_sits_at_zero_pbp_coverage() {
    let pool = pool().await;
    let zero_dates: Vec<(chrono::NaiveDate, i64)> = sqlx::query_as(
        "WITH d AS ( \
             SELECT g.game_date, count(*) AS games, \
                    count(*) FILTER ( \
                        WHERE EXISTS (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id) \
                    ) AS with_pbp \
             FROM games g \
             WHERE g.season >= $1 AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL \
             GROUP BY g.game_date \
         ) \
         SELECT game_date, games FROM d WHERE with_pbp = 0 ORDER BY game_date",
    )
    .bind(FIRST_API_ERA_SEASON)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert!(
        zero_dates.is_empty(),
        "{} API-era game date(s) have zero PBP coverage: {:?}. The zero-coverage arm of \
         pbp_date_coverage_gap assumes this never happens honestly — either these are real \
         holes to backfill, or the arm needs a floor after all.",
        zero_dates.len(),
        zero_dates.iter().take(5).collect::<Vec<_>>(),
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
