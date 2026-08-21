//! Parity guard: a row the nightly `game_projections` sweep writes must equal
//! what the live request path would have returned for the same matchup.
//!
//! This is the invariant the whole precompute rests on (#266). `team_detail`
//! now reads the table for completed games and only projects the rest live, so
//! if the batch writer's arithmetic drifts from `projection::predict_projection`
//! the team page would show one number tonight and a different one tomorrow,
//! with nothing to say which is right. The two paths share feature assembly,
//! neutral symmetrisation, and the preseason blend precisely so this holds —
//! what they do NOT share is the fetch orchestration (the writer caches team
//! stats per season and the point-in-time cohort per cutoff date), and a
//! caching bug there is exactly what this catches.
//!
//! Gated `#[ignore]` — needs a local DB with a played season ingested and the
//! ONNX bundles in `MODEL_DIR`. It runs a full season sweep, so give it a
//! minute. Run:
//!   DATABASE_URL=... cargo test -p cstat-ingest --test game_projection_parity -- --ignored --nocapture

use chrono::NaiveDate;
use cstat_core::inference::Predictor;
use cstat_core::projection::{BlendClock, predict_projection};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// How many games to re-project through the live path. Each one costs a full
/// point-in-time rebuild, so this is a sample, not a sweep — but it is drawn
/// across the whole calendar and forced to include neutral-site games, which
/// are the case with a distinct code path (two orderings, averaged).
const SAMPLE_SIZE: usize = 12;

/// Season to check. Any fully-ingested played season works.
fn season() -> i32 {
    std::env::var("PARITY_SEASON")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2026)
}

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .max_connections(10)
        .connect(&url)
        .await
        .expect("connect")
}

fn predictor() -> Predictor {
    let dir = cstat_ingest::model_dir_from_env();
    Predictor::load(std::path::Path::new(&dir)).expect("load ONNX bundles")
}

struct Sampled {
    game_id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    is_neutral: bool,
    is_conference: bool,
    stored_margin: f64,
    stored_win_prob: f64,
    stored_home_score: i32,
    stored_away_score: i32,
}

/// Pull `SAMPLE_SIZE` stored rows spread across the season, guaranteeing at
/// least a few neutral-site games — the ordering-sensitive case.
async fn sample(pool: &PgPool, season: i32) -> Vec<Sampled> {
    let rows = sqlx::query(
        r#"
        (SELECT game_id, game_date, home_team_id, away_team_id, is_neutral, is_conference,
                projected_margin, home_win_prob, projected_home_score, projected_away_score
         FROM game_projections WHERE season = $1 AND is_neutral
         ORDER BY game_date, game_id LIMIT 4)
        UNION ALL
        (SELECT game_id, game_date, home_team_id, away_team_id, is_neutral, is_conference,
                projected_margin, home_win_prob, projected_home_score, projected_away_score
         FROM game_projections WHERE season = $1 AND NOT is_neutral
         ORDER BY game_date, game_id LIMIT 4)
        UNION ALL
        (SELECT game_id, game_date, home_team_id, away_team_id, is_neutral, is_conference,
                projected_margin, home_win_prob, projected_home_score, projected_away_score
         FROM game_projections WHERE season = $1
         ORDER BY game_date DESC, game_id LIMIT 4)
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .expect("sample stored projections");

    rows.into_iter()
        .map(|r| Sampled {
            game_id: r.get("game_id"),
            game_date: r.get("game_date"),
            home_team_id: r.get("home_team_id"),
            away_team_id: r.get("away_team_id"),
            is_neutral: r.get("is_neutral"),
            is_conference: r.get("is_conference"),
            stored_margin: r.get("projected_margin"),
            stored_win_prob: r.get("home_win_prob"),
            stored_home_score: r.get("projected_home_score"),
            stored_away_score: r.get("projected_away_score"),
        })
        .take(SAMPLE_SIZE)
        .collect()
}

#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn batch_written_projections_match_the_live_path() {
    let pool = pool().await;
    let predictor = predictor();
    let season = season();

    let report = cstat_ingest::game_projections::run_season(&pool, &predictor, season)
        .await
        .expect("sweep the season");
    assert!(
        report.written > 0,
        "sweep wrote nothing for season {season} — is it ingested?"
    );
    println!(
        "swept {} games across {} cutoff dates ({} skipped, {} pruned)",
        report.written, report.dates, report.skipped, report.pruned
    );

    let sampled = sample(&pool, season).await;
    assert!(!sampled.is_empty(), "no stored rows to compare");
    assert!(
        sampled.iter().any(|s| s.is_neutral),
        "sample must include a neutral-site game — that path predicts both \
         orderings and averages, and is the one most likely to drift"
    );

    for s in &sampled {
        // The writer's cutoff rule: the day before the game, so the model sees
        // pre-game state.
        let as_of = s.game_date.pred_opt().expect("representable cutoff");
        let live = predict_projection(
            &pool,
            &predictor,
            s.home_team_id,
            s.away_team_id,
            season,
            s.is_neutral,
            s.is_conference,
            BlendClock::AsOf(as_of),
        )
        .await
        .unwrap_or_else(|e| panic!("live projection for {} failed: {e}", s.game_id));

        // f32 model output widened to f64 on write, so this is an exactness
        // check with room only for the widening — not a tolerance on the model.
        assert!(
            (live.margin as f64 - s.stored_margin).abs() < 1e-6,
            "margin drift on {} ({}): stored {} vs live {}",
            s.game_id,
            s.game_date,
            s.stored_margin,
            live.margin
        );
        assert!(
            (live.home_win_prob - s.stored_win_prob).abs() < 1e-9,
            "win-prob drift on {}: stored {} vs live {}",
            s.game_id,
            s.stored_win_prob,
            live.home_win_prob
        );
        assert_eq!(
            (live.home_score, live.away_score),
            (s.stored_home_score, s.stored_away_score),
            "score drift on {}",
            s.game_id
        );
    }
    println!(
        "{} sampled games match the live path exactly",
        sampled.len()
    );
}

#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn pit_predictions_are_reproducible() {
    // The point-in-time path was nondeterministic: `get_roster_agg_pit`
    // flattened a `HashMap` into the `pit` UNNEST arrays, and `HashMap`
    // iteration order varies between instances in one process, so Postgres
    // summed the minutes-weighted aggregates in a different order every call.
    // Floating-point addition is not associative, the last bits moved, and a
    // feature sitting on a LightGBM split threshold flipped branches — one
    // 2026 matchup returned 16.8 or 17.1 points from the same request
    // depending on the run.
    //
    // That is a serving bug on its own, and it is fatal to a precompute: a
    // stored row could never equal a live recomputation, and this file's
    // parity test would flake rather than catch drift. The fix is a sorted
    // flatten; this is the guard on it.
    let pool = pool().await;
    let predictor = predictor();
    let season = season();

    let sampled = sample(&pool, season).await;
    assert!(
        !sampled.is_empty(),
        "no stored rows to compare — sweep first"
    );

    const REPEATS: usize = 5;
    for s in sampled.iter().take(4) {
        let as_of = s.game_date.pred_opt().expect("representable cutoff");
        let mut seen: Vec<(f32, i32, i32)> = Vec::new();
        for _ in 0..REPEATS {
            let p = predict_projection(
                &pool,
                &predictor,
                s.home_team_id,
                s.away_team_id,
                season,
                s.is_neutral,
                s.is_conference,
                BlendClock::AsOf(as_of),
            )
            .await
            .unwrap_or_else(|e| panic!("live projection for {} failed: {e}", s.game_id));
            seen.push((p.margin, p.home_score, p.away_score));
        }
        let first = seen[0];
        assert!(
            seen.iter().all(|&v| v == first),
            "{} produced {REPEATS} runs that disagree: {seen:?} — the pit feature \
             path has become order-dependent again",
            s.game_id
        );
    }
}

#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn sweep_is_idempotent() {
    // The nightly re-runs this every night over the same played games. A second
    // pass must produce identical values, or the team page's Projected column
    // would flicker night to night with no input having changed.
    let pool = pool().await;
    let predictor = predictor();
    let season = season();

    cstat_ingest::game_projections::run_season(&pool, &predictor, season)
        .await
        .expect("first sweep");
    let before = sample(&pool, season).await;

    cstat_ingest::game_projections::run_season(&pool, &predictor, season)
        .await
        .expect("second sweep");
    let after = sample(&pool, season).await;

    assert_eq!(
        before.len(),
        after.len(),
        "sample size changed between sweeps"
    );
    // Keyed on game_id rather than compared position by position: `sample`
    // builds its result from three UNION ALL branches with no top-level ORDER
    // BY, and Postgres does not promise a stable row order for that. A
    // positional zip would turn a plan change into a spurious failure that
    // says nothing about idempotence.
    let after_by_id: std::collections::HashMap<Uuid, &Sampled> =
        after.iter().map(|s| (s.game_id, s)).collect();
    for b in &before {
        let a = after_by_id
            .get(&b.game_id)
            .unwrap_or_else(|| panic!("{} vanished from the sample on a re-sweep", b.game_id));
        assert_eq!(
            b.stored_margin, a.stored_margin,
            "margin moved on a re-sweep for {}",
            b.game_id
        );
        assert_eq!(
            b.stored_win_prob, a.stored_win_prob,
            "win probability moved on a re-sweep for {}",
            b.game_id
        );
        assert_eq!(
            (b.stored_home_score, b.stored_away_score),
            (a.stored_home_score, a.stored_away_score),
            "scores moved on a re-sweep for {}",
            b.game_id
        );
    }
}
