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
use cstat_core::heldout::{ModelRouter, SeasonModel};
use cstat_core::inference::Predictor;
use cstat_core::projection::{
    BlendClock, ProjectionSummary, predict_projection, predict_projection_heldout,
};
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

fn router() -> ModelRouter {
    let dir = cstat_ingest::model_dir_from_env();
    ModelRouter::load(std::path::Path::new(&dir)).expect("read model seasons")
}

/// The one-game computation the sweep's row for `s` must equal: the live
/// path, scored by whichever models the router assigns the season (#468).
async fn independent_projection(
    pool: &PgPool,
    predictor: &Predictor,
    router: &ModelRouter,
    season: i32,
    s: &Sampled,
) -> ProjectionSummary {
    let clock = BlendClock::AsOf(s.game_date.pred_opt().expect("representable cutoff"));
    let r = match router.route(season).expect("season is routable") {
        SeasonModel::FullFit => {
            predict_projection(
                pool,
                predictor,
                s.home_team_id,
                s.away_team_id,
                season,
                s.is_neutral,
                s.is_conference,
                clock,
            )
            .await
        }
        SeasonModel::HeldOut(bundle) => {
            predict_projection_heldout(
                pool,
                predictor,
                bundle,
                s.home_team_id,
                s.away_team_id,
                season,
                s.is_neutral,
                s.is_conference,
                clock,
            )
            .await
        }
        SeasonModel::NoEarlierSeason => panic!("{season} stores no projections"),
    };
    r.unwrap_or_else(|e| panic!("projection for {} failed: {e}", s.game_id))
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
    let router = router();
    let season = season();

    let report = cstat_ingest::game_projections::run_season(&pool, &predictor, &router, season)
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
        // pre-game state. For a season the served models were trained on, the
        // same computation on the held-out bundle the sweep used.
        let live = independent_projection(&pool, &predictor, &router, season, s).await;

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
    let router = router();
    let season = season();

    cstat_ingest::game_projections::run_season(&pool, &predictor, &router, season)
        .await
        .expect("first sweep");
    let before = sample(&pool, season).await;

    cstat_ingest::game_projections::run_season(&pool, &predictor, &router, season)
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

/// The #468 walk-forward check: a stored row for a season the served models
/// were trained on comes from a model that never saw that season.
///
/// Three things have to hold for `PARITY_SEASON` (default 2026, a trained-on
/// season): the router sends it to a held-out bundle trained only on earlier
/// seasons; each stored row equals that bundle's one-game projection; and the
/// stored rows are NOT what the served models give, which is the evidence the
/// sweep did not quietly fall back to the full fit.
#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn trained_season_rows_come_from_a_model_that_never_saw_it() {
    let pool = pool().await;
    let predictor = predictor();
    let router = router();
    let season = season();

    let SeasonModel::HeldOut(bundle) = router.route(season).expect("routable") else {
        panic!("PARITY_SEASON={season} is not a trained-on season with a held-out bundle");
    };
    assert!(
        bundle.train_seasons.iter().all(|&s| s < season),
        "the bundle for {season} was trained on {:?}",
        bundle.train_seasons
    );

    cstat_ingest::game_projections::run_season(&pool, &predictor, &router, season)
        .await
        .expect("sweep the season");
    let sampled = sample(&pool, season).await;
    assert!(!sampled.is_empty(), "no stored rows to compare");

    let mut differs_from_full_fit = 0;
    for s in &sampled {
        let clock = BlendClock::AsOf(s.game_date.pred_opt().expect("representable cutoff"));
        let held = predict_projection_heldout(
            &pool,
            &predictor,
            bundle,
            s.home_team_id,
            s.away_team_id,
            season,
            s.is_neutral,
            s.is_conference,
            clock,
        )
        .await
        .unwrap_or_else(|e| panic!("held-out projection for {} failed: {e}", s.game_id));
        let full = predict_projection(
            &pool,
            &predictor,
            s.home_team_id,
            s.away_team_id,
            season,
            s.is_neutral,
            s.is_conference,
            clock,
        )
        .await
        .unwrap_or_else(|e| panic!("full-fit projection for {} failed: {e}", s.game_id));

        assert!(
            (held.margin as f64 - s.stored_margin).abs() < 1e-6,
            "{} stored {} but the held-out bundle gives {}",
            s.game_id,
            s.stored_margin,
            held.margin
        );
        if (full.margin as f64 - s.stored_margin).abs() > 1e-3 {
            differs_from_full_fit += 1;
        }
        println!(
            "{} {}: stored {:+.2}, held-out {:+.2}, full fit (has seen {season}) {:+.2}",
            s.game_date, s.game_id, s.stored_margin, held.margin, full.margin
        );
    }
    // Two independently fitted ensembles agreeing to a thousandth of a point
    // on most games would mean the sweep used the full fit after all.
    assert!(
        differs_from_full_fit * 2 > sampled.len(),
        "only {differs_from_full_fit} of {} stored rows differ from the full fit",
        sampled.len()
    );
}

/// The first trained-on season has no earlier season to train a held-out
/// model on, so the sweep stores nothing for it and removes what was there.
#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn first_trained_season_stores_no_projections() {
    let pool = pool().await;
    let predictor = predictor();
    let router = router();
    let first = *router
        .full_fit_seasons()
        .iter()
        .min()
        .expect("trained seasons");
    assert!(matches!(
        router.route(first),
        Ok(SeasonModel::NoEarlierSeason)
    ));

    let report = cstat_ingest::game_projections::run_season(&pool, &predictor, &router, first)
        .await
        .expect("sweep the first season");
    assert_eq!(report.written, 0);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM game_projections WHERE season = $1")
        .bind(first)
        .fetch_one(&pool)
        .await
        .expect("count rows");
    assert_eq!(left, 0, "{first} still has stored projections");
}

/// The upcoming-game sweep (#429) against the live path, the same exactness
/// check as above on the other regime: season-aggregate features, the prod
/// bundle, and the LIVE blend clock.
///
/// A played season has no unplayed games, so this inserts two synthetic ones
/// between real teams — one home, one neutral — sweeps, compares, and deletes
/// them (the projection rows cascade). The blend date sits inside the decay
/// window so the preseason leg is exercised too.
#[tokio::test]
#[ignore = "requires a local DB with a played season and the ONNX bundles"]
async fn upcoming_projections_match_the_live_path() {
    let pool = pool().await;
    let predictor = predictor();
    let season = season();
    let blend_date = NaiveDate::from_ymd_opt(season - 1, 11, 20).unwrap();

    // Four distinct teams that played, so both synthetic games qualify.
    let teams: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT home_team_id FROM games \
         WHERE season = $1 AND home_score IS NOT NULL AND home_team_id IS NOT NULL \
         ORDER BY home_team_id LIMIT 4",
    )
    .bind(season)
    .fetch_all(&pool)
    .await
    .expect("pick teams");
    assert_eq!(teams.len(), 4, "season {season} needs played teams");

    let tag = Uuid::new_v4();
    let fixtures = [(teams[0], teams[1], false), (teams[2], teams[3], true)];
    let mut ids = Vec::new();
    for (i, (home, away, neutral)) in fixtures.iter().enumerate() {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO games (natstat_id, season, game_date, home_team_id, away_team_id, \
                                is_neutral_site, is_conference) \
             VALUES ($1, $2, $3, $4, $5, $6, false) RETURNING id",
        )
        .bind(format!("parity-upcoming-{tag}-{i}"))
        .bind(season)
        .bind(NaiveDate::from_ymd_opt(season, 4, 30).unwrap())
        .bind(home)
        .bind(away)
        .bind(neutral)
        .fetch_one(&pool)
        .await
        .expect("insert synthetic game");
        ids.push(id);
    }

    let task_pool = pool.clone();
    let outcome = async move {
        let pool = task_pool;
        let report = cstat_ingest::game_projections::run_upcoming_season(
            &pool, &predictor, season, blend_date,
        )
        .await
        .expect("sweep upcoming games");
        assert!(report.written >= fixtures.len(), "sweep wrote {report:?}");

        for (id, (home, away, neutral)) in ids.iter().zip(fixtures) {
            let row = sqlx::query(
                "SELECT projected_margin, home_win_prob, projected_home_score, \
                        projected_away_score \
                 FROM upcoming_game_projections WHERE game_id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("stored upcoming row");
            let live = predict_projection(
                &pool,
                &predictor,
                home,
                away,
                season,
                neutral,
                false,
                BlendClock::Live(blend_date),
            )
            .await
            .expect("live projection");
            let margin: f64 = row.get("projected_margin");
            let p: f64 = row.get("home_win_prob");
            assert!(
                (live.margin as f64 - margin).abs() < 1e-6,
                "margin drift (neutral={neutral}): stored {margin} vs live {}",
                live.margin
            );
            assert!((live.home_win_prob - p).abs() < 1e-9, "win-prob drift");
            assert_eq!(
                (live.home_score, live.away_score),
                (
                    row.get::<i32, _>("projected_home_score"),
                    row.get::<i32, _>("projected_away_score")
                ),
                "score drift"
            );
        }
    };
    // Clean up even when an assertion fails, so a red run does not leave
    // synthetic games in the season.
    let result = tokio::spawn(outcome).await;
    sqlx::query("DELETE FROM games WHERE natstat_id LIKE $1")
        .bind(format!("parity-upcoming-{tag}-%"))
        .execute(&pool)
        .await
        .expect("remove synthetic games");
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}
