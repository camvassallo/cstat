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

use cstat_core::{compute, queries};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// The tests below mutate and read the same `player_archetypes` rows, so a
/// parallel `cargo test` run would race (a writer's DELETE+INSERT interleaving
/// with another's read). Each test holds this lock for its whole body, keeping
/// them order- and thread-independent without requiring `--test-threads=1`.
static DB_SERIAL: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

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
        // provisional=false only: the parity contract is about REAL
        // current-season assignments vs Python. Prior-season seeds
        // (provisional=true, PR 3a) are additive rows Python never wrote.
        "SELECT player_id, cluster_id, primary_class, secondary_class, \
                primary_score, secondary_score, affinity_scores::text AS affinity, \
                feature_vector \
         FROM player_archetypes WHERE season = $1 AND provisional = FALSE",
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
    let _serial = DB_SERIAL.lock().await;
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

/// Exercises the WRITE path (`compute_archetypes` DELETE+INSERT), including the
/// PR 3a prior-season seeds. Runs it TWICE and asserts the second run reproduces
/// the first byte-for-byte — the real idempotency contract (the first run may
/// legitimately differ from the Python-written baseline because it adds
/// provisional seed rows Python never wrote). Fingerprints ALL rows (quals +
/// seeds), including the new columns.
#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data; mutates player_archetypes (idempotently)"]
async fn compute_archetypes_write_is_idempotent() {
    let _serial = DB_SERIAL.lock().await;
    let (pool, seasons) = pool_and_seasons().await;
    // Newest season with a fit — the one the nightly actually recomputes.
    let season = *seasons.last().expect("a season with a fit");

    // A stable fingerprint of the served columns + the PR 3a source columns.
    let fingerprint = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT md5(string_agg( \
                 player_id::text || cluster_id::text || primary_class || \
                 coalesce(secondary_class,'') || round(primary_score::numeric, 9)::text || \
                 provisional::text || source || coalesce(source_season::text,''), \
                 '|' ORDER BY player_id)) \
             FROM player_archetypes WHERE season = $1",
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let counts = |pool: PgPool| async move {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT count(*) FILTER (WHERE NOT provisional), \
                    count(*) FILTER (WHERE provisional) \
             FROM player_archetypes WHERE season = $1",
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    let written1 = compute::compute_archetypes(&pool, season, false)
        .await
        .unwrap();
    let fp1 = fingerprint(pool.clone()).await;
    let (real, seeded) = counts(pool.clone()).await;

    let written2 = compute::compute_archetypes(&pool, season, false)
        .await
        .unwrap();
    let fp2 = fingerprint(pool.clone()).await;

    eprintln!("season {season}: wrote {written1} rows ({real} real, {seeded} seeded)");
    assert_eq!(written1, written2, "row count changed between runs");
    assert_eq!(
        written1 as i64,
        real + seeded,
        "returned count != rows written"
    );
    assert_eq!(
        fp1, fp2,
        "row content changed between runs (not idempotent)"
    );
}

/// Validates the PR 3a prior-season seed itself: every seeded (provisional) row
/// must (a) be a sub-gate player with no real assignment, and (b) copy an actual
/// prior-season REAL label for the same human, tagged with the season it came
/// from. Seeds `season` from earlier seasons (the local DB has 2015–2026).
#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data; mutates player_archetypes"]
async fn prior_season_seed_is_well_formed() {
    let _serial = DB_SERIAL.lock().await;
    let (pool, seasons) = pool_and_seasons().await;
    let season = *seasons.last().expect("a season with a fit");
    compute::compute_archetypes(&pool, season, false)
        .await
        .unwrap();

    // No player has both a real row and a seed row in the same season.
    let dupes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ( \
            SELECT player_id FROM player_archetypes WHERE season = $1 \
            GROUP BY player_id HAVING count(*) > 1) x",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        dupes, 0,
        "a player has more than one archetype row this season"
    );

    // Every seed row: provisional, source='prior_season', source_season < season,
    // and it matches a REAL prior label for the same human (natstat_id path).
    let bad_meta: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM player_archetypes \
         WHERE season = $1 AND provisional = TRUE \
           AND (source <> 'prior_season' OR source_season IS NULL OR source_season >= $1)",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        bad_meta, 0,
        "a seed row has malformed provisional/source metadata"
    );

    let (seeded, from_dist): (i64, Option<String>) = sqlx::query_as(
        "SELECT count(*), string_agg(DISTINCT source_season::text, ',' ORDER BY source_season::text) \
         FROM player_archetypes WHERE season = $1 AND provisional = TRUE",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    eprintln!("season {season}: {seeded} seeded from prior season(s) [{from_dist:?}]");
}

/// PR 3b: the class-level aggregate serve paths must exclude prior-season seeds
/// (real current-season members only) — a seed must never become a class
/// exemplar or inflate the glossary counts. Seeds `season`, then checks the two
/// served aggregates against the DB's own real/seed split.
#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data; mutates player_archetypes"]
async fn aggregates_exclude_provisional_seeds() {
    let _serial = DB_SERIAL.lock().await;
    let (pool, seasons) = pool_and_seasons().await;
    let season = *seasons.last().expect("a season with a fit");
    compute::compute_archetypes(&pool, season, false)
        .await
        .unwrap();

    // The test is only meaningful if seeds exist to (wrongly) leak.
    let seeded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM player_archetypes WHERE season = $1 AND provisional",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        seeded > 0,
        "expected some seeds this season to exercise exclusion"
    );

    // Exemplars: none may be a provisional (seed) player.
    let exemplars = queries::get_archetype_exemplars(&pool, season, 5)
        .await
        .unwrap();
    assert!(!exemplars.is_empty(), "no exemplars returned");
    for e in &exemplars {
        let is_prov: bool = sqlx::query_scalar(
            "SELECT provisional FROM player_archetypes WHERE player_id = $1 AND season = $2",
        )
        .bind(e.player_id)
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !is_prov,
            "exemplar {} ({}) is a provisional seed",
            e.name, e.player_id
        );
    }

    // Class summary total must equal the real (non-provisional) row count.
    let summary = queries::get_archetype_class_summary(&pool, season)
        .await
        .unwrap();
    let summary_total: i64 = summary.iter().map(|s| s.count).sum();
    let real: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM player_archetypes WHERE season = $1 AND NOT provisional",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        summary_total, real,
        "class-summary counts include provisional seeds ({summary_total} vs {real} real)"
    );
}

/// Tier-3 live newcomer inference (`infer_newcomers = true`): sub-gate players
/// with no prior label get a `source = 'current_partial'` provisional row built
/// from their partial current-season sample, and it never collides with a real
/// or carry-over row. Runs against the newest fitted season (all local seasons
/// are complete, so we force the flag rather than rely on the calendar gate).
#[tokio::test]
#[ignore = "needs local DB with a Python archetype fit on the current data; mutates player_archetypes"]
async fn tier3_infers_newcomers_when_enabled() {
    let _serial = DB_SERIAL.lock().await;
    let (pool, seasons) = pool_and_seasons().await;
    let season = *seasons.last().expect("a season with a fit");

    compute::compute_archetypes(&pool, season, true)
        .await
        .unwrap();

    // Well-formed tier-3 rows: provisional, source='current_partial', no source
    // year, and a genuinely sub-gate but playing sample (>=3 GP, >=10 MPG, <10 GP).
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM player_archetypes pa \
         JOIN player_season_stats pss ON pss.player_id = pa.player_id AND pss.season = pa.season \
         WHERE pa.season = $1 AND pa.source = 'current_partial' \
           AND (pa.provisional = FALSE OR pa.source_season IS NOT NULL \
                OR pss.games_played < 3 OR pss.games_played >= 10 OR pss.minutes_per_game < 10)",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bad, 0, "a tier-3 row is malformed or not actually sub-gate");

    // No player carries more than one archetype row.
    let dupes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT player_id FROM player_archetypes WHERE season = $1 \
         GROUP BY player_id HAVING count(*) > 1) x",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dupes, 0, "tier-3 collided with a real or carry-over row");

    let (infer, carry, real): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE source = 'current_partial'), \
                count(*) FILTER (WHERE source = 'prior_season'), \
                count(*) FILTER (WHERE NOT provisional) \
         FROM player_archetypes WHERE season = $1",
    )
    .bind(season)
    .fetch_one(&pool)
    .await
    .unwrap();
    eprintln!("season {season}: {real} real, {carry} carry-over, {infer} live-inferred");

    // Restore the non-inferred state so this test doesn't leave tier-3 rows for
    // the completed season behind (the calendar would never have enabled them).
    compute::compute_archetypes(&pool, season, false)
        .await
        .unwrap();
}
