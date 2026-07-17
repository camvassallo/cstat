//! Parity guard: the Rust archetype *assign* port
//! (`compute::assign_archetypes`) must reproduce the Python writer's
//! (`training/archetypes.py`) `player_archetypes` rows exactly, for every season
//! that already carries a fit.
//!
//! This is the safety net that lets prod drop its daily Python dependency: the
//! nightly's `compute_all` now assigns archetypes in Rust against the frozen
//! `archetype_models` fit, and if that assign ever drifts from what the annual
//! Python fit-and-assign produced, the served labels would silently change on
//! the next recompute. This test catches that drift.
//!
//! Gated `#[ignore]` — needs a local DB whose `player_archetypes` /
//! `archetype_models` were produced by the CURRENT `player_season_stats` /
//! `torvik_player_stats` (i.e. `python -m archetypes` was the last thing run,
//! not a `compute` that post-dated the fit — otherwise the raw features moved
//! under the frozen model and the divergence is real drift, not a port bug).
//! Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test archetype_assign_parity -- --ignored --nocapture

use cstat_core::compute;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

/// The stored (Python-written) archetype row we compare each assignment against.
struct StoredRow {
    cluster_id: i32,
    primary_class: String,
    secondary_class: Option<String>,
    primary_score: f64,
    secondary_score: Option<f64>,
    affinity: HashMap<String, f64>,
    feature_vector: Vec<f32>,
}

async fn pool_and_seasons() -> (PgPool, Vec<i32>) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    // Only seasons that carry BOTH a fit and assigned rows are comparable.
    let seasons: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT pa.season \
         FROM player_archetypes pa \
         JOIN archetype_models m ON m.season = pa.season \
         ORDER BY pa.season",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    (pool, seasons)
}

async fn stored_rows(pool: &PgPool, season: i32) -> HashMap<Uuid, StoredRow> {
    let rows = sqlx::query(
        "SELECT player_id, cluster_id, primary_class, secondary_class, \
                primary_score, secondary_score, affinity_scores::text AS affinity, \
                feature_vector \
         FROM player_archetypes WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .unwrap();

    rows.into_iter()
        .map(|row| {
            let player_id: Uuid = row.get("player_id");
            let affinity_str: String = row.get("affinity");
            let affinity: HashMap<String, f64> = serde_json::from_str(&affinity_str).unwrap();
            (
                player_id,
                StoredRow {
                    cluster_id: row.get("cluster_id"),
                    primary_class: row.get("primary_class"),
                    secondary_class: row.get("secondary_class"),
                    primary_score: row.get("primary_score"),
                    secondary_score: row.get("secondary_score"),
                    affinity,
                    feature_vector: row.get("feature_vector"),
                },
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data"]
async fn rust_assign_matches_python_rows() {
    let (pool, seasons) = pool_and_seasons().await;
    assert!(
        !seasons.is_empty(),
        "no season has both player_archetypes and archetype_models — run `python -m archetypes` first"
    );

    // Tolerances: class labels / cluster ids must match EXACTLY (they come from
    // an argmin, no float slack). Scores are softmax outputs that can differ in
    // the last ULP of exp() between numpy and Rust; the feature_vector is stored
    // as f32.
    const SCORE_TOL: f64 = 1e-6;
    const VEC_TOL: f32 = 1e-4;

    let mut total = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for season in seasons {
        let stored = stored_rows(&pool, season).await;
        let assigned = compute::assign_archetypes(&pool, season).await.unwrap();
        total += assigned.len();

        // Same population.
        if assigned.len() != stored.len() {
            mismatches.push(format!(
                "season {season}: assigned {} players, stored {} — population differs",
                assigned.len(),
                stored.len()
            ));
        }

        for a in &assigned {
            let Some(s) = stored.get(&a.player_id) else {
                mismatches.push(format!(
                    "season {season}: player {} assigned but has no stored row",
                    a.player_id
                ));
                continue;
            };
            if a.primary_class != s.primary_class {
                mismatches.push(format!(
                    "season {season} player {}: primary {} != stored {}",
                    a.player_id, a.primary_class, s.primary_class
                ));
            }
            if Some(&a.secondary_class) != s.secondary_class.as_ref() {
                mismatches.push(format!(
                    "season {season} player {}: secondary {:?} != stored {:?}",
                    a.player_id,
                    Some(&a.secondary_class),
                    s.secondary_class
                ));
            }
            if a.cluster_id != s.cluster_id {
                mismatches.push(format!(
                    "season {season} player {}: cluster {} != stored {}",
                    a.player_id, a.cluster_id, s.cluster_id
                ));
            }
            if (a.primary_score - s.primary_score).abs() > SCORE_TOL {
                mismatches.push(format!(
                    "season {season} player {}: primary_score {} vs {}",
                    a.player_id, a.primary_score, s.primary_score
                ));
            }
            if let Some(ss) = s.secondary_score
                && (a.secondary_score - ss).abs() > SCORE_TOL
            {
                mismatches.push(format!(
                    "season {season} player {}: secondary_score {} vs {}",
                    a.player_id, a.secondary_score, ss
                ));
            }
            // Affinity per class.
            let a_aff = a.affinity_scores.as_object().unwrap();
            for (class, v) in a_aff {
                let av = v.as_f64().unwrap();
                let sv = s.affinity.get(class).copied().unwrap_or(f64::NAN);
                if (av - sv).abs() > SCORE_TOL {
                    mismatches.push(format!(
                        "season {season} player {}: affinity[{class}] {av} vs {sv}",
                        a.player_id
                    ));
                }
            }
            // Standardized feature vector (stored f32).
            if a.feature_vector.len() != s.feature_vector.len() {
                mismatches.push(format!(
                    "season {season} player {}: feature_vector len {} vs {}",
                    a.player_id,
                    a.feature_vector.len(),
                    s.feature_vector.len()
                ));
            } else {
                for (i, (av, sv)) in a.feature_vector.iter().zip(&s.feature_vector).enumerate() {
                    if (av - sv).abs() > VEC_TOL {
                        mismatches.push(format!(
                            "season {season} player {}: feature_vector[{i}] {av} vs {sv}",
                            a.player_id
                        ));
                    }
                }
            }
        }
    }

    eprintln!("checked {total} assignments");
    // Cap the noise if something is badly wrong.
    for m in mismatches.iter().take(40) {
        eprintln!("  {m}");
    }
    assert!(
        mismatches.is_empty(),
        "{} archetype assign parity mismatch(es) vs Python — see above",
        mismatches.len()
    );
}

/// Exercises the WRITE path (`compute_archetypes` DELETE+INSERT), not just the
/// dry `assign_archetypes` compute: running it must leave `player_archetypes`
/// byte-identical (idempotent), since the frozen model + unchanged inputs assign
/// the same rows the DB already holds. Fingerprints the season's rows, rewrites,
/// and re-fingerprints.
#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data; mutates player_archetypes (idempotently)"]
async fn compute_archetypes_write_is_idempotent() {
    let (pool, seasons) = pool_and_seasons().await;
    // Newest season with a fit — the one the nightly actually recomputes.
    let season = *seasons.last().expect("a season with a fit");

    // A stable fingerprint of the served columns, ordered by player.
    let fingerprint = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT md5(string_agg( \
                 player_id::text || cluster_id::text || primary_class || \
                 coalesce(secondary_class,'') || round(primary_score::numeric, 9)::text, \
                 '|' ORDER BY player_id)) \
             FROM player_archetypes WHERE season = $1",
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    let before = fingerprint(pool.clone()).await;
    let count_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM player_archetypes WHERE season = $1")
            .bind(season)
            .fetch_one(&pool)
            .await
            .unwrap();

    let written = compute::compute_archetypes(&pool, season).await.unwrap();

    let after = fingerprint(pool.clone()).await;
    let count_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM player_archetypes WHERE season = $1")
            .bind(season)
            .fetch_one(&pool)
            .await
            .unwrap();

    eprintln!("season {season}: wrote {written} rows, {count_before} -> {count_after}");
    assert_eq!(
        written as i64, count_after,
        "returned count must equal rows written"
    );
    assert_eq!(count_before, count_after, "row count changed after rewrite");
    assert_eq!(
        before, after,
        "row content changed after rewrite (not idempotent)"
    );
}
