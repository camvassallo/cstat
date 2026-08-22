//! Regression guards for the roster-aggregate Torvik fan-out (issue #311).
//!
//! `torvik_player_stats` is UNIQUE on `(torvik_pid, season)`, not on
//! `(player_id, season)`. #307 fixed the three `queries.rs` read paths that
//! joined it as though the pair were unique; the same shape reached the
//! FEATURE BUILDERS, where the symptom is a wrong number rather than a
//! repeated row. `features::get_roster_agg` takes `COUNT(*)` and eighteen
//! `SUM(x * total_minutes) / SUM(total_minutes)` averages over the join, so a
//! duplicated player was counted twice and weighted twice in the 49-feature
//! vector behind every game prediction — 213 of 4,255 team-seasons (5.0%),
//! `roster_size` 10 for 9 players on Mercyhurst 2026.
//!
//! What makes this different from #307 is that **the training frames fanned
//! out identically**: `training/features.py::load_torvik_stats` selected one
//! row per `torvik_pid` and merged on `(player_id, season)`, and
//! `weighted_agg` counted the duplicate the same way. The distortion was
//! symmetric, so fixing serving alone would have INTRODUCED ~5% train/serve
//! skew where none existed. Both halves collapse now, and the models were
//! retrained — which is why `train_serve_collapse_agrees` matters more than
//! it looks: it is the guard that the two halves still pick the same profile.
//!
//! `trajectory.rs` was the exception and the one genuinely one-sided case —
//! `train_trajectory_model.py` keys its base frame on `torvik_pid` and never
//! fanned out, while serving keyed on `player_id` and did.
//!
//! DB-gated: uses whatever the local DB already holds and skips cleanly when
//! `DATABASE_URL` is unset or the data holds no duplicates.
//!
//!   DATABASE_URL=... cargo test -p cstat-core --test roster_agg_fanout -- --ignored --nocapture

use cstat_core::features::{TeamSeason, build_all_features};
use cstat_core::inference::FEATURE_NAMES;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset; skipping roster-aggregate fan-out test");
        return None;
    };
    Some(PgPoolOptions::new().connect(&url).await.unwrap())
}

/// Team/seasons whose qualified roster (the `>= 5 GP`, `>= 10 MPG` gate both
/// `get_roster_agg` and `fetch_roster` use) holds a player carrying more than
/// one Torvik profile. Exactly the cohort that double-counted.
async fn affected(pool: &PgPool) -> Vec<(Uuid, i32)> {
    sqlx::query(
        r#"
        SELECT pss.team_id, pss.season
        FROM player_season_stats pss
        JOIN (
            SELECT player_id, season
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            GROUP BY player_id, season
            HAVING count(*) > 1
        ) d ON d.player_id = pss.player_id AND d.season = pss.season
        WHERE pss.games_played >= 5
          AND pss.minutes_per_game >= 10
          AND pss.team_id IS NOT NULL
        GROUP BY 1, 2
        ORDER BY pss.season DESC, pss.team_id
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get::<Uuid, _>(0), r.get::<i32, _>(1)))
    .collect()
}

/// How many DISTINCT players clear the qualified gate — what `roster_size`
/// must equal.
async fn qualified_players(pool: &PgPool, team_id: Uuid, season: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT count(DISTINCT player_id) FROM player_season_stats
         WHERE team_id = $1 AND season = $2
           AND games_played >= 5 AND minutes_per_game >= 10",
    )
    .bind(team_id)
    .bind(season)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A team/season with no duplicated profile, to pair an affected team against
/// so `diff_roster_size` has a known clean side.
async fn clean_opponent(pool: &PgPool, season: i32, avoid: Uuid) -> Option<Uuid> {
    sqlx::query_scalar(
        r#"
        SELECT pss.team_id
        FROM player_season_stats pss
        WHERE pss.season = $1
          AND pss.team_id <> $2
          AND pss.games_played >= 5
          AND pss.minutes_per_game >= 10
          AND NOT EXISTS (
              SELECT 1 FROM torvik_player_stats t
              WHERE t.player_id = pss.player_id AND t.season = pss.season
              GROUP BY t.player_id, t.season HAVING count(*) > 1
          )
        GROUP BY pss.team_id
        HAVING count(*) >= 8
        ORDER BY pss.team_id
        LIMIT 1
        "#,
    )
    .bind(season)
    .bind(avoid)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test roster_agg_fanout -- --ignored"]
async fn served_feature_vector_counts_each_player_once() {
    let Some(pool) = pool().await else { return };

    let teams = affected(&pool).await;
    if teams.is_empty() {
        eprintln!("no duplicated Torvik profiles on a qualified roster; nothing to guard");
        return;
    }
    eprintln!("{} affected team/seasons", teams.len());

    let idx = FEATURE_NAMES
        .iter()
        .position(|n| *n == "diff_roster_size")
        .expect("diff_roster_size is a wire-locked feature name");

    // Go through the real served entry point rather than the private
    // aggregate: `diff_roster_size` is home minus away, so pairing an affected
    // team against a clean one reads the affected side's count directly.
    let mut checked = 0usize;
    for (team_id, season) in teams.iter().take(40) {
        let Some(opponent) = clean_opponent(&pool, *season, *team_id).await else {
            continue;
        };
        let home_n = qualified_players(&pool, *team_id, *season).await;
        let away_n = qualified_players(&pool, opponent, *season).await;

        let feats = build_all_features(
            &pool,
            TeamSeason::new(*team_id, *season),
            TeamSeason::new(opponent, *season),
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            feats.diff[idx],
            (home_n - away_n) as f32,
            "team {team_id} season {season}: diff_roster_size is {} against \
             {home_n} - {away_n} = {} real players — the Torvik join is \
             double-counting again",
            feats.diff[idx],
            home_n - away_n,
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no affected team could be paired with a clean opponent — nothing was \
         actually checked",
    );
    eprintln!("{checked} affected team/seasons carry an honest roster_size");
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test roster_agg_fanout -- --ignored"]
async fn projection_roster_frame_holds_one_slot_per_player() {
    let Some(pool) = pool().await else { return };

    // `compose_all_projections` pulls its roster rows with the season-wide
    // query in `roster_projection.rs`, which fanned out the same way. Asserted
    // at the SQL shape rather than through the function because the function
    // needs a loaded `Predictor`, and what is under test here is the join, not
    // the model. Runs the pre-fix and post-fix joins side by side so the test
    // cannot pass by the cohort being empty.
    let row = sqlx::query(
        r#"
        WITH bare AS (
            SELECT pss.player_id, pss.season
            FROM player_season_stats pss
            LEFT JOIN torvik_player_stats tps
                   ON tps.player_id = pss.player_id AND tps.season = pss.season
            WHERE pss.season = (SELECT max(season) FROM player_season_stats)
        ),
        collapsed AS (
            SELECT pss.player_id, pss.season
            FROM player_season_stats pss
            LEFT JOIN (
                SELECT DISTINCT ON (player_id, season) *
                FROM torvik_player_stats
                WHERE player_id IS NOT NULL
                ORDER BY player_id, season, torvik_pid
            ) tps ON tps.player_id = pss.player_id AND tps.season = pss.season
            WHERE pss.season = (SELECT max(season) FROM player_season_stats)
        )
        SELECT
            (SELECT count(*) FROM bare),
            (SELECT count(*) FROM collapsed),
            (SELECT count(*) FROM (
                SELECT DISTINCT player_id, season FROM collapsed
            ) d)
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let (bare, collapsed, distinct): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    assert_eq!(
        collapsed, distinct,
        "the projection roster frame holds {collapsed} rows for {distinct} \
         (player, season) pairs",
    );
    assert!(
        bare > collapsed,
        "the pre-fix join returned {bare} rows against {collapsed} — the \
         newest season no longer exercises the fan-out, so this test is \
         passing vacuously",
    );
    eprintln!("projection frame: {bare} rows before, {collapsed} after");
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test roster_agg_fanout -- --ignored"]
async fn train_serve_collapse_agrees() {
    let Some(pool) = pool().await else { return };

    // The serving builders collapse with `LEFT JOIN LATERAL ... ORDER BY
    // torvik_pid LIMIT 1`; `training/features.py::load_torvik_stats` and the
    // roster-impact trainer use `DISTINCT ON (player_id, season) ... ORDER BY
    // player_id, season, torvik_pid`. Both mean min(torvik_pid). If they ever
    // stop meaning the same thing, the model is served a profile it was not
    // trained on and nothing else in the suite would notice — the failure is a
    // quietly wrong feature value, not an error.
    let row = sqlx::query(
        r#"
        WITH dups AS (
            SELECT player_id, season
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            GROUP BY player_id, season
            HAVING count(*) > 1
        ),
        serving AS (
            SELECT d.player_id, d.season, l.torvik_pid
            FROM dups d
            LEFT JOIN LATERAL (
                SELECT * FROM torvik_player_stats t
                WHERE t.player_id = d.player_id AND t.season = d.season
                ORDER BY t.torvik_pid
                LIMIT 1
            ) l ON TRUE
        ),
        training AS (
            SELECT d.player_id, d.season, x.torvik_pid
            FROM dups d
            LEFT JOIN (
                SELECT DISTINCT ON (player_id, season) *
                FROM torvik_player_stats
                WHERE player_id IS NOT NULL
                ORDER BY player_id, season, torvik_pid
            ) x ON x.player_id = d.player_id AND x.season = d.season
        )
        SELECT count(*),
               count(*) FILTER (
                   WHERE serving.torvik_pid IS DISTINCT FROM training.torvik_pid
               )
        FROM serving JOIN training USING (player_id, season)
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let (pairs, disagreements): (i64, i64) = (row.get(0), row.get(1));
    if pairs == 0 {
        eprintln!("no duplicated Torvik profiles locally; nothing to guard");
        return;
    }
    assert_eq!(
        disagreements, 0,
        "{disagreements} of {pairs} duplicated pairs resolve to a different \
         Torvik profile in the serving frame than in the training frame",
    );
    eprintln!("{pairs} duplicated pairs agree across train and serve");
}
