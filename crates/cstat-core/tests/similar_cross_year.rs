//! Regression guards for cross-season neighbour search
//! (`queries::get_similar_players`, `?cross_year=true`).
//!
//! Dropping the `pa.season = $2` scope on the candidate side is legitimate
//! rather than an approximation — `archetype_models` carries one distinct
//! `feature_means` / `feature_stds` / `centroids` across all of its season
//! rows, so every season's `feature_vector` is standardized against the same
//! scaler. The first test asserts that premise directly, because the whole
//! feature is only sound while it holds: a future per-season refit would make
//! cross-era distances incommensurable, and it would do so silently.
//!
//! The rest guard the three things the wider candidate pool introduced:
//!   * the default (single-season) path is untouched — same ranked list as the
//!     pre-change SQL, which this file keeps a verbatim copy of;
//!   * one slot per human, so a five-season career can't fill the top 10;
//!   * the target's own other seasons come back labelled `is_self` rather than
//!     dropped or passed off as somebody else.
//!
//! DB-gated: uses whatever the local DB already holds and skips cleanly when
//! `DATABASE_URL` is unset.

use cstat_core::queries::{SimilarPlayerRow, get_similar_players};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::collections::HashSet;
use uuid::Uuid;

const SKIP: &str = "DATABASE_URL unset; skipping cross-year similar-player test";

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("{SKIP}");
        return None;
    };
    Some(PgPoolOptions::new().connect(&url).await.unwrap())
}

/// The cross-era identity of one `(player_id, season)` archetype row, mirroring
/// the key the query dedupes on: `torvik_pid` when present (stable across
/// schools), `natstat_id` otherwise (NatStat mints a fresh one per team).
///
/// The `min(torvik_pid)` is load-bearing and must stay in step with the query.
/// `torvik_player_stats` is unique on `(torvik_pid, season)`, not on
/// `(player_id, season)`, so a bare join here would return whichever of a
/// player's two or three Torvik profiles the planner emitted first and turn the
/// `is_self` assertion into a coin flip.
async fn identity(pool: &PgPool, player_id: Uuid, season: i32) -> String {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT COALESCE('t' || tp.torvik_pid::text, 'n' || p.natstat_id)
        FROM players p
        LEFT JOIN (
            SELECT player_id, season, min(torvik_pid) AS torvik_pid
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            GROUP BY player_id, season
        ) tp ON tp.player_id = p.id AND tp.season = $2
        WHERE p.id = $1
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Archetype targets whose `(player_id, season)` carries more than one Torvik
/// profile — the cohort that fans a naive join out. Empty on a DB that happens
/// to hold no duplicates, in which case the caller skips.
async fn duplicate_torvik_targets(pool: &PgPool, n: i64) -> Vec<(Uuid, i32)> {
    sqlx::query(
        r#"
        SELECT pa.player_id, pa.season
        FROM player_archetypes pa
        JOIN (
            SELECT player_id, season
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            GROUP BY player_id, season
            HAVING count(*) > 1
        ) d ON d.player_id = pa.player_id AND d.season = pa.season
        ORDER BY pa.player_id
        LIMIT $1
        "#,
    )
    .bind(n)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get("player_id"), r.get("season")))
    .collect()
}

/// Archetype targets whose human owns rows in >= 2 seasons — the only ones that
/// can exercise dedupe or `is_self` at all.
async fn multi_season_targets(pool: &PgPool, n: i64) -> Vec<(Uuid, i32)> {
    sqlx::query(
        r#"
        SELECT pa.player_id, pa.season
        FROM player_archetypes pa
        JOIN players p ON p.id = pa.player_id
        JOIN torvik_player_stats tp
          ON tp.player_id = pa.player_id AND tp.season = pa.season
        WHERE tp.torvik_pid IN (
            SELECT t2.torvik_pid
            FROM torvik_player_stats t2
            JOIN player_archetypes pa2 ON pa2.player_id = t2.player_id
                                      AND pa2.season = t2.season
            GROUP BY t2.torvik_pid
            HAVING count(*) >= 2
        )
        ORDER BY pa.player_id
        LIMIT $1
        "#,
    )
    .bind(n)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get("player_id"), r.get("season")))
    .collect()
}

/// Euclidean distance between two archetype rows, computed from the vectors
/// themselves with no joins that could fan out. The independent check on what
/// the query serves.
async fn euclidean(pool: &PgPool, a: (Uuid, i32), b: (Uuid, i32)) -> f64 {
    sqlx::query_scalar::<_, f64>(
        r#"
        WITH va AS (
            SELECT feature_vector AS v FROM player_archetypes
            WHERE player_id = $1 AND season = $2
        ),
        vb AS (
            SELECT feature_vector AS v FROM player_archetypes
            WHERE player_id = $3 AND season = $4
        )
        SELECT sqrt(SUM(POWER(a_v::double precision - b_v::double precision, 2)))
        FROM va, vb, LATERAL unnest(va.v, vb.v) AS u(a_v, b_v)
        "#,
    )
    .bind(a.0)
    .bind(a.1)
    .bind(b.0)
    .bind(b.1)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// One slot per human, asserted two ways. Identity catches the same person
/// under two `player_id`s (different seasons); `player_id` catches the same row
/// under two identities, which is what a fan-out on the Torvik join produces
/// and what an ident-only check cannot see.
async fn assert_one_slot_per_human(pool: &PgPool, rows: &[SimilarPlayerRow]) {
    let mut seen_ident = HashSet::new();
    let mut seen_row = HashSet::new();
    for row in rows {
        let ident = identity(pool, row.player_id, row.season).await;
        assert!(
            seen_ident.insert(ident.clone()),
            "{} appears twice in the neighbour list ({ident}) — one human is \
             supposed to collapse to their single nearest season",
            row.name,
        );
        assert!(
            seen_row.insert((row.player_id, row.season)),
            "{} ({}) appears twice under the same player_id — a join fanned out",
            row.name,
            row.season,
        );
    }
}

/// The premise the whole feature rests on: one combined-cohort fit shared by
/// every season, so a 2015 vector and a 2026 vector live in the same space.
#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test similar_cross_year -- --ignored"]
async fn archetype_feature_space_is_shared_across_seasons() {
    let Some(pool) = pool().await else { return };

    let row = sqlx::query(
        r#"
        SELECT count(DISTINCT feature_means::text) AS means,
               count(DISTINCT feature_stds::text)  AS stds,
               count(DISTINCT centroids::text)     AS centroids,
               count(*)                            AS rows
        FROM archetype_models
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    if row.get::<i64, _>("rows") == 0 {
        eprintln!("archetype_models is empty; skipping");
        return;
    }

    assert_eq!(
        (
            row.get::<i64, _>("means"),
            row.get::<i64, _>("stds"),
            row.get::<i64, _>("centroids"),
        ),
        (1, 1, 1),
        "archetype_models no longer holds a single shared scaler/centroid set — \
         cross-season neighbour distances are not commensurable and \
         ?cross_year=true is unsound until this is a combined-cohort fit again",
    );
}

/// `cross_year` absent or false must return exactly what the endpoint returned
/// before the flag existed. The baseline here is the pre-change SQL, copied
/// verbatim, so this compares against the old behaviour rather than against the
/// new code's own opinion of it.
#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test similar_cross_year -- --ignored"]
async fn single_season_path_matches_the_pre_change_query() {
    let Some(pool) = pool().await else { return };
    let targets = multi_season_targets(&pool, 3).await;
    if targets.is_empty() {
        eprintln!("no multi-season archetype players in the DB; skipping");
        return;
    }

    for (player_id, season) in targets {
        let baseline: Vec<(Uuid, f64)> = sqlx::query_as(
            r#"
            WITH target AS (
                SELECT feature_vector AS fv
                FROM player_archetypes
                WHERE player_id = $1 AND season = $2
            ),
            candidates AS (
                SELECT
                    pa.player_id,
                    sqrt(SUM(POWER(pa_v::double precision - tg_v::double precision, 2))) AS distance
                FROM player_archetypes pa
                CROSS JOIN target
                CROSS JOIN LATERAL unnest(pa.feature_vector, target.fv) AS u(pa_v, tg_v)
                WHERE pa.season = $2 AND pa.player_id <> $1
                GROUP BY pa.player_id
            )
            SELECT c.player_id, c.distance
            FROM candidates c
            JOIN players p ON p.id = c.player_id
            ORDER BY c.distance ASC
            LIMIT $3
            "#,
        )
        .bind(player_id)
        .bind(season)
        .bind(10_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

        let got = get_similar_players(&pool, player_id, season, 10, false)
            .await
            .unwrap();

        assert_eq!(
            got.iter().map(|r| r.player_id).collect::<Vec<_>>(),
            baseline.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "single-season neighbour list changed for {player_id} / {season}",
        );
        for (row, (_, dist)) in got.iter().zip(&baseline) {
            assert!(
                (row.distance - dist).abs() < 1e-9,
                "single-season distance changed for {player_id} / {season}",
            );
            assert_eq!(
                row.season, season,
                "single-season rows must carry the requested season",
            );
            assert!(
                !row.is_self,
                "the single-season path cannot surface the target's own row",
            );
        }
    }
}

/// The cross-year path has to actually cross years, give every row its own
/// season, and never spend two of the caller's `k` slots on one human.
#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test similar_cross_year -- --ignored"]
async fn cross_year_spans_seasons_and_holds_one_slot_per_human() {
    let Some(pool) = pool().await else { return };

    // Deliberately an EARLY-season target: 2015 is the far end of the era skew
    // the issue documents, so if any target were going to come back all
    // same-season it would be this one.
    let earliest: Option<(Uuid, i32)> = sqlx::query_as(
        r#"
        SELECT pa.player_id, pa.season
        FROM player_archetypes pa
        WHERE pa.season = (SELECT min(season) FROM player_archetypes)
        ORDER BY pa.primary_score DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let Some((player_id, season)) = earliest else {
        eprintln!("no archetype rows in the DB; skipping");
        return;
    };

    let rows = get_similar_players(&pool, player_id, season, 25, true)
        .await
        .unwrap();
    assert!(!rows.is_empty(), "cross-year search returned nothing");

    let seasons: HashSet<i32> = rows.iter().map(|r| r.season).collect();
    assert!(
        seasons.len() > 1,
        "cross-year neighbours for a {season} target came back from one season \
         ({seasons:?}) — the candidate-side season scope is still in effect",
    );

    assert_one_slot_per_human(&pool, &rows).await;

    // Distance order is what the caller ranks on; the dedupe must not disturb it.
    assert!(
        rows.windows(2).all(|w| w[0].distance <= w[1].distance),
        "neighbours are not in ascending distance order",
    );
}

/// "Your closest comp is yourself, a year later" is a real result and is kept —
/// but it must be labelled. `is_self` is true exactly when the row is the
/// target human in another season, never merely a look-alike.
#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test similar_cross_year -- --ignored"]
async fn cross_year_labels_the_targets_own_other_seasons() {
    let Some(pool) = pool().await else { return };
    let targets = multi_season_targets(&pool, 5).await;
    if targets.is_empty() {
        eprintln!("no multi-season archetype players in the DB; skipping");
        return;
    }

    let mut saw_self = false;
    for (player_id, season) in &targets {
        let target_ident = identity(&pool, *player_id, *season).await;
        let rows = get_similar_players(&pool, *player_id, *season, 25, true)
            .await
            .unwrap();

        for row in &rows {
            assert_ne!(
                (row.player_id, row.season),
                (*player_id, *season),
                "the target's own row must not be its own neighbour",
            );
            let ident = identity(&pool, row.player_id, row.season).await;
            assert_eq!(
                row.is_self,
                ident == target_ident,
                "is_self mislabelled for {} ({}) against target {player_id}",
                row.name,
                row.season,
            );
            saw_self |= row.is_self;
        }
    }

    assert!(
        saw_self,
        "no multi-season target surfaced its own other season across {} tries — \
         a player's adjacent year is normally among his nearest comps, so this \
         means the target's other seasons are being filtered out",
        targets.len(),
    );
}

/// The case the rest of the suite structurally could not see: a target, and
/// candidates, whose `(player_id, season)` carries more than one Torvik profile.
///
/// `torvik_player_stats` is unique on `(torvik_pid, season)` rather than on
/// `(player_id, season)`, so a bare join fans out — the target CTE returns n
/// rows, every distance is multiplied by sqrt(n), every output row is
/// duplicated n times, and a duplicated candidate arrives wearing n distinct
/// identities that walk straight through the dedupe. All four of the other
/// tests passed while that was live.
#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test similar_cross_year -- --ignored"]
async fn cross_year_survives_duplicate_torvik_links() {
    let Some(pool) = pool().await else { return };
    let targets = duplicate_torvik_targets(&pool, 3).await;
    if targets.is_empty() {
        eprintln!("no duplicate Torvik links in the DB; skipping");
        return;
    }

    for (player_id, season) in &targets {
        let rows = get_similar_players(&pool, *player_id, *season, 10, true)
            .await
            .unwrap();
        assert!(!rows.is_empty(), "cross-year search returned nothing");

        // A fanned-out target duplicates every row, so the caller silently gets
        // k/n distinct neighbours out of a k-row response.
        assert_eq!(
            rows.len(),
            10,
            "asked for 10 neighbours of {player_id} / {season} and got {} rows",
            rows.len(),
        );
        assert_one_slot_per_human(&pool, &rows).await;

        // Row count and dedupe are not enough: the fan-out ALSO multiplies
        // every served distance by sqrt(n), and a response can be the right
        // length and the right people with silently wrong numbers. Recompute
        // each distance straight from the two feature vectors.
        for row in &rows {
            let expected =
                euclidean(&pool, (*player_id, *season), (row.player_id, row.season)).await;
            assert!(
                (row.distance - expected).abs() < 1e-9,
                "{} ({}) came back at distance {:.6} but the two feature \
                 vectors are {:.6} apart — the target vector was fanned out",
                row.name,
                row.season,
                row.distance,
                expected,
            );
        }
    }
}
