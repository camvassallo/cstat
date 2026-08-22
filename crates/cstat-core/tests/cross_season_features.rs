//! The cross-season feature build works, and each side's season stays welded
//! to its own team.
//!
//! `features::build_all_features` takes two [`TeamSeason`]s so a matchup can
//! draw its sides from different years (ROADMAP §7c). The predict route is
//! the caller that does it, via `home_season` / `away_season` — but it reaches
//! this code through several layers, and nothing else in the suite would
//! notice if a season stopped travelling with its team.
//!
//! That failure mode is worth a dedicated test because of how it presents.
//! `projection::predict_with_venue` swaps the two sides for the Away path and
//! runs both orderings for Neutral. If the ids swapped while the seasons did
//! not, each team's row would be read in the *other* team's year — and the
//! result is not an error, it is a confidently-served wrong number with a 200.
//! Every existing test passes one season for both sides, so every one of them
//! is blind to it by construction.
//!
//! Gated `#[ignore]` — needs a local DB with 2015 and 2026 ingested and
//! `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test cross_season_features -- --ignored --nocapture

use cstat_core::features::{TeamSeason, build_all_features};
use cstat_core::inference::FEATURE_NAMES;
use cstat_core::projection::is_flag_feature;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

/// Two ingested seasons far enough apart that the league baselines genuinely
/// differ — the same pair the cross-era work is scoped against.
const OLD: i32 = 2015;
const NEW: i32 = 2026;

/// Index of `diff_adj_offense` in the 49-feature vector. Asserted against
/// `FEATURE_NAMES` below rather than trusted, since the order is wire-locked
/// to `model_meta.json` and a reorder must fail loudly here.
const DIFF_ADJ_OFFENSE: usize = 3;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap()
}

/// Resolve exactly one team by name prefix.
///
/// Deliberately NOT `LIMIT 1`: with a prefix match that would leave the row
/// choice to the planner, and the failure would be invisible rather than loud.
/// Every assertion below re-derives its expected value from whichever team was
/// bound, so a second `Kentucky%` program appearing in a later ingest would not
/// fail the test — it would keep passing while quietly no longer exercising the
/// era gap it exists for. Same latent class as #228. Requiring a unique match
/// turns that into an immediate, legible failure.
async fn team(pool: &PgPool, name_like: &str, season: i32) -> TeamSeason {
    let rows = sqlx::query("SELECT id, name FROM teams WHERE name LIKE $1 AND season = $2")
        .bind(name_like)
        .bind(season)
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| panic!("query for {name_like:?} in {season} failed: {e}"));
    let names: Vec<String> = rows.iter().map(|r| r.get("name")).collect();
    assert_eq!(
        rows.len(),
        1,
        "{name_like:?} must match exactly one team in {season}; matched {names:?}"
    );
    TeamSeason::new(rows[0].get("id"), season)
}

async fn adj_offense(pool: &PgPool, t: TeamSeason) -> f64 {
    sqlx::query("SELECT adj_offense FROM team_season_stats WHERE team_id = $1 AND season = $2")
        .bind(t.id)
        .bind(t.season)
        .fetch_one(pool)
        .await
        .unwrap()
        .get("adj_offense")
}

/// Each side is read from its OWN season, not from one shared season.
#[tokio::test]
#[ignore = "requires a local DB with 2015 and 2026 ingested and computed"]
async fn each_side_is_read_from_its_own_season() {
    let p = pool().await;
    assert_eq!(
        FEATURE_NAMES[DIFF_ADJ_OFFENSE], "diff_adj_offense",
        "feature order moved; this test indexes the vector directly"
    );

    let old_kentucky = team(&p, "Kentucky%", OLD).await;
    let new_duke = team(&p, "Duke%", NEW).await;
    let new_kentucky = team(&p, "Kentucky%", NEW).await;

    let cross = build_all_features(&p, old_kentucky, new_duke, true, false)
        .await
        .expect("cross-season build must succeed");

    // The headline check: diff_adj_offense must be the OLD team's AdjO minus
    // the NEW team's, each pulled from its own `team_season_stats` row. If the
    // builder collapsed to a single season, one of these two operands would be
    // the wrong row and the arithmetic would not reconcile.
    let expected = (adj_offense(&p, old_kentucky).await - adj_offense(&p, new_duke).await) as f32;
    assert!(
        (cross.diff[DIFF_ADJ_OFFENSE] - expected).abs() < 1e-3,
        "diff_adj_offense {} != {expected} built from each side's own season",
        cross.diff[DIFF_ADJ_OFFENSE]
    );

    // And the same program in a different year is genuinely a different input,
    // so the cross-era vector cannot be silently equal to a same-season one.
    let same = build_all_features(&p, new_kentucky, new_duke, true, false)
        .await
        .expect("same-season build must succeed");
    assert_ne!(
        cross.diff, same.diff,
        "{OLD} and {NEW} Kentucky must not produce the same feature vector"
    );
}

/// Swapping the two sides negates every non-flag diff — the property the
/// Away and Neutral venue paths depend on, and the one that breaks first if a
/// season ever comes unstuck from its team.
#[tokio::test]
#[ignore = "requires a local DB with 2015 and 2026 ingested and computed"]
async fn swapping_sides_negates_every_diff_across_eras() {
    let p = pool().await;
    let old_kentucky = team(&p, "Kentucky%", OLD).await;
    let new_duke = team(&p, "Duke%", NEW).await;

    let fwd = build_all_features(&p, old_kentucky, new_duke, true, false)
        .await
        .expect("forward build must succeed");
    let rev = build_all_features(&p, new_duke, old_kentucky, true, false)
        .await
        .expect("reversed build must succeed");

    for (i, name) in FEATURE_NAMES.iter().enumerate() {
        // Venue and conference are 0/1 indicators, not diffs — they do not
        // reverse sign when the teams swap.
        if is_flag_feature(i) {
            assert_eq!(
                fwd.diff[i], rev.diff[i],
                "flag feature {name} must be swap-invariant"
            );
            continue;
        }
        let (a, b) = (fwd.diff[i], rev.diff[i]);
        assert!(
            (a + b).abs() < 1e-4,
            "feature {i} ({name}) did not negate under swap: {a} vs {b} — \
             a season likely did not travel with its team"
        );
    }
}
