//! The matchup prediction engine — venue semantics, neutral symmetrisation,
//! win-probability calibration, and the early-season preseason blend.
//!
//! Lives in `cstat-core` rather than the API because it has **two** callers
//! that must agree exactly: the `/api/predict` handler and TeamDetail's
//! `Projected` column serve it live, and the nightly `game_projections`
//! writer materialises it for every completed game. When this logic lived in
//! `cstat-api` the batch writer could only have re-implemented it, and a
//! precomputed projection that disagrees with the live one by a few tenths is
//! worse than no precompute at all — the page would visibly change value the
//! first night after a game.
//!
//! What stayed in `cstat-api`: query-param parsing, team lookup, the TreeSHAP
//! contribution payload, and the JSON response shapes.

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::{self, GameFeatures};
use crate::inference::{NUM_FEATURES, Prediction, Predictor};

/// Where the game is being played.
///
/// `Home` = the team passed as `home` is hosting.
/// `Away` = the team passed as `away` is hosting (so we swap before feature
/// extraction and negate the resulting margin so the response stays from
/// the `home` param's perspective).
/// `Neutral` = no host. Predictions are symmetrised by averaging both team
/// orderings — see [`predict_neutral_symmetric`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Venue {
    Home,
    Away,
    Neutral,
}

/// A prediction plus the per-feature values and TreeSHAP contributions that
/// produced it, all in the caller's `home_team_id` frame.
pub struct Explained {
    pub prediction: Prediction,
    pub feature_values: [f32; NUM_FEATURES],
    pub contributions: [f32; NUM_FEATURES],
}

/// Which clock the preseason blend reads.
///
/// The two arms differ in more than the date: an explicit `AsOf` cutoff means
/// the margin leg came from the **pit** bundle (so the win-prob conversion
/// uses the pit σ) and pre-open dates are allowed for deliberate preseason
/// probing, while `Live` means the prod bundle and is hard-gated to zero
/// before the Nov 1 open. Passing the date in (rather than reading a clock
/// here) keeps `cstat-core` free of `cstat_ingest::today_utc`, which owns the
/// simulated-clock overrides the replay harness drives.
#[derive(Debug, Clone, Copy)]
pub enum BlendClock {
    /// Explicit point-in-time cutoff — the pit bundle produced the margin.
    AsOf(NaiveDate),
    /// Live request; the payload is today's date from the caller's clock.
    Live(NaiveDate),
}

impl BlendClock {
    /// The `as_of_date` to pass down the feature/model path: `Some` only on
    /// the explicit-cutoff arm, which is exactly the pit-bundle predicate.
    pub fn as_of_date(self) -> Option<NaiveDate> {
        match self {
            BlendClock::AsOf(d) => Some(d),
            BlendClock::Live(_) => None,
        }
    }

    fn is_pit(self) -> bool {
        self.as_of_date().is_some()
    }

    /// Weight on the preseason leg for this clock and season. Zero means the
    /// blend is disengaged and callers can skip the `team_preseason_projection`
    /// lookup entirely — which is most of the season, so this is the check
    /// that keeps the blend from costing two queries per projection in
    /// February.
    pub fn blend_weight(self, season: i32) -> f32 {
        match self {
            BlendClock::AsOf(d) => preseason_blend_weight(d, season),
            BlendClock::Live(today) => live_blend_weight(today, season),
        }
    }
}

/// Whether to compute the TreeSHAP attribution alongside the margin.
///
/// The Predict page's "Keys to the game" panel needs it. Nothing else does —
/// TeamDetail's `Projected` column and the nightly `game_projections` writer
/// both discard `contributions` — and TreeSHAP is a full walk of the LightGBM
/// ensemble per call, on top of the ONNX inference that produces the margin.
/// The margin comes from the same ONNX session either way, so skipping the
/// attribution changes no served number; it only stops paying for a payload
/// nobody reads. `Explained::contributions` is all-zero when skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// Compute TreeSHAP contributions (the explainability payload).
    Shap,
    /// Skip TreeSHAP; `contributions` comes back all-zero.
    Skip,
}

/// Prefix marking a "we have no prediction inputs for this team/season" error —
/// a `RowNotFound` out of feature extraction, which means one of the teams has no
/// stats row for the requested season (e.g. a program that hadn't reached D1
/// yet, like Utah Tech in 2021). That's a **client** error (a bad team/season
/// combo), not a server fault, so the route maps it to 404 rather than 500 — a
/// 500 here would page `#errors-api` on what is effectively a user typo. Any
/// other sqlx error is a genuine failure and keeps its 500.
pub const NO_PREDICTION_DATA_PREFIX: &str = "no prediction data";

/// Turn a feature-extraction sqlx error into the route-facing message, tagging
/// the missing-data case with [`NO_PREDICTION_DATA_PREFIX`] (see there).
fn classify_feature_error(e: sqlx::Error, season: i32, what: &str) -> String {
    match e {
        sqlx::Error::RowNotFound => format!(
            "{NO_PREDICTION_DATA_PREFIX}: one or both teams have no data for season {season}"
        ),
        other => format!("{what} failed: {other}"),
    }
}

/// Whether the feature at `i` is a 0/1 indicator (venue, conference
/// game) rather than a `home − away` diff. Flag features don't reverse
/// sign when the teams swap, so they need special handling in venue
/// transforms.
pub fn is_flag_feature(i: usize) -> bool {
    matches!(
        crate::inference::FEATURE_NAMES[i],
        "venue" | "is_conference_game"
    )
}

/// Run both model heads over an already-built feature vector.
///
/// The split from [`predict_matchup`] is what lets the nightly batch writer
/// assemble features from cached parts and still share this crate's inference
/// + calibration path exactly.
pub fn predict_from_features(
    predictor: &Predictor,
    f: &GameFeatures,
    is_pit: bool,
    attribution: Attribution,
) -> Result<Explained, String> {
    // Margin (+ optional TreeSHAP) from the diff vector; totals from the
    // diff+sum vector. Pit and end-of-season paths use distinct model bundles
    // — see `Predictor::predict_*` doc comments.
    let (predicted_margin, contributions, predicted_total) = match (is_pit, attribution) {
        (true, Attribution::Shap) => {
            let a = predictor
                .predict_pit_with_contributions(&f.diff)
                .map_err(|e| format!("pit prediction failed: {e}"))?;
            let t = predictor
                .predict_pit_total(&f.diff_and_sum)
                .map_err(|e| format!("pit totals prediction failed: {e}"))?;
            (a.predicted_margin, a.contributions, t)
        }
        (true, Attribution::Skip) => {
            let m = predictor
                .predict_pit_margin(&f.diff)
                .map_err(|e| format!("pit prediction failed: {e}"))?;
            let t = predictor
                .predict_pit_total(&f.diff_and_sum)
                .map_err(|e| format!("pit totals prediction failed: {e}"))?;
            (m, [0.0; NUM_FEATURES], t)
        }
        (false, Attribution::Shap) => {
            let a = predictor
                .predict_with_contributions(&f.diff)
                .map_err(|e| format!("prediction failed: {e}"))?;
            let t = predictor
                .predict_total(&f.diff_and_sum)
                .map_err(|e| format!("totals prediction failed: {e}"))?;
            (a.predicted_margin, a.contributions, t)
        }
        (false, Attribution::Skip) => {
            let m = predictor
                .predict_margin(&f.diff)
                .map_err(|e| format!("prediction failed: {e}"))?;
            let t = predictor
                .predict_total(&f.diff_and_sum)
                .map_err(|e| format!("totals prediction failed: {e}"))?;
            (m, [0.0; NUM_FEATURES], t)
        }
    };

    // Override the standalone win-classifier output with a margin-derived
    // win probability. The two LightGBM models (margin + win) are trained
    // independently, so near the boundary their answers can disagree by a
    // few points and produce the user-visible contradiction of "predicted
    // winner = X" alongside "X has 49% win probability". Tying the win
    // probability to margin via a calibrated logistic guarantees the two
    // signals always agree on direction.
    Ok(Explained {
        prediction: Prediction {
            predicted_margin,
            home_win_probability: margin_to_win_prob(predicted_margin, is_pit),
            predicted_total,
        },
        feature_values: f.diff,
        contributions,
    })
}

/// Fetch features for a matchup and run both model heads.
///
/// When `as_of_date` is set, we route through the pit feature builder
/// (CamPom v3 aggregated from torvik_player_game_stats up to the
/// cutoff) and serve the pit model bundle — train/serve parity is the
/// load-bearing invariant: feeding pit features to the end-of-season
/// model (or vice versa) would reintroduce the ~3 AUC points of
/// lookahead inflation the predict-honesty audit caught.
#[allow(clippy::too_many_arguments)] // cohesive matchup inputs; a param
// struct here would only rename the same eight values at every call site.
pub async fn predict_matchup(
    pool: &PgPool,
    predictor: &Predictor,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
    attribution: Attribution,
) -> Result<Explained, String> {
    // Single DB-fetch pass produces both the 49-element diff vector
    // (margin/win input) and the 58-element diff+sum vector (totals
    // input). The feature extraction is the expensive step.
    let f = match as_of_date {
        Some(d) => features::build_all_features_pit(
            pool,
            home_team_id,
            away_team_id,
            season,
            is_neutral,
            is_conference,
            d,
        )
        .await
        .map_err(|e| classify_feature_error(e, season, "pit feature extraction"))?,
        None => features::build_all_features(
            pool,
            home_team_id,
            away_team_id,
            season,
            is_neutral,
            is_conference,
        )
        .await
        .map_err(|e| classify_feature_error(e, season, "feature extraction"))?,
    };

    predict_from_features(predictor, &f, as_of_date.is_some(), attribution)
}

/// Run the predictor with explicit venue semantics, including symmetric
/// averaging for neutral games. All fields in the returned [`Explained`]
/// are from the caller's `home_team_id` perspective (positive margin /
/// contribution = pushed toward home_team).
#[allow(clippy::too_many_arguments)] // cohesive matchup inputs; a param
// struct here would only rename the same eight values at every call site.
pub async fn predict_with_venue(
    pool: &PgPool,
    predictor: &Predictor,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    venue: Venue,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
    attribution: Attribution,
) -> Result<Explained, String> {
    match venue {
        Venue::Home => {
            predict_matchup(
                pool,
                predictor,
                home_team_id,
                away_team_id,
                season,
                false,
                is_conference,
                as_of_date,
                attribution,
            )
            .await
        }
        Venue::Away => {
            // Caller's "home" param is actually the visitor. Swap before
            // feature extraction (so the model sees the true host as home),
            // then flip the result back to the caller's home perspective.
            //   - margin negates (m_home = -m_swap)
            //   - win prob mirrors around 0.5
            //   - contributions all negate (the entire margin frame flipped,
            //     so "pushed toward swap-home" becomes "pushed toward
            //     caller-away" with a sign flip — applies to flag features
            //     too, since their contribution is measured against the
            //     same margin)
            //   - feature_values for diff_* features negate (the diff
            //     reverses direction when teams swap), but the two flag
            //     features stay (someone is still hosting; conference
            //     match is symmetric).
            let swapped = predict_matchup(
                pool,
                predictor,
                away_team_id,
                home_team_id,
                season,
                false,
                is_conference,
                as_of_date,
                attribution,
            )
            .await?;
            let mut feature_values = swapped.feature_values;
            let mut contributions = swapped.contributions;
            for (i, v) in feature_values.iter_mut().enumerate() {
                if !is_flag_feature(i) {
                    *v = -*v;
                }
            }
            for c in &mut contributions {
                *c = -*c;
            }
            Ok(Explained {
                prediction: Prediction {
                    predicted_margin: -swapped.prediction.predicted_margin,
                    home_win_probability: 1.0 - swapped.prediction.home_win_probability,
                    // Totals are invariant under team swap (home + away
                    // = away + home), so no flip — the model output
                    // travels through unchanged.
                    predicted_total: swapped.prediction.predicted_total,
                },
                feature_values,
                contributions,
            })
        }
        Venue::Neutral => {
            let (fwd, rev) = tokio::try_join!(
                predict_matchup(
                    pool,
                    predictor,
                    home_team_id,
                    away_team_id,
                    season,
                    true,
                    is_conference,
                    as_of_date,
                    attribution,
                ),
                predict_matchup(
                    pool,
                    predictor,
                    away_team_id,
                    home_team_id,
                    season,
                    true,
                    is_conference,
                    as_of_date,
                    attribution,
                ),
            )?;
            Ok(combine_neutral(fwd, rev, as_of_date.is_some()))
        }
    }
}

/// Average forward + reverse predictions so neutral-site results are
/// invariant to argument order.
///
/// LightGBM tree ensembles aren't antisymmetric in diff features — even when
/// venue=0, `predict(diff(A,B))` and `-predict(diff(B,A))` will disagree by
/// a few tenths of a point. Some upstream features (rolling form, star
/// player, NULL-coalesced fields) also don't perfectly negate when the
/// teams swap. Averaging the two margins forces
/// `margin(A,B,neutral) == -margin(B,A,neutral)` exactly; the win
/// probability is then derived from the symmetric margin (in
/// [`predict_from_features`]'s output we already replace the win-classifier
/// with [`margin_to_win_prob`], so re-deriving here keeps the two perfectly
/// in step) which gives `p_home(A,B,neutral) + p_home(B,A,neutral) == 1.0`
/// exactly.
///
/// Split from the fetch so the batch writer, which builds both orderings from
/// one cached set of parts, symmetrises through the same arithmetic.
pub fn combine_neutral(fwd: Explained, rev: Explained, is_pit: bool) -> Explained {
    let symmetric_margin =
        0.5 * (fwd.prediction.predicted_margin - rev.prediction.predicted_margin);
    // Totals symmetrize *additively* — total(A,B) and total(B,A) should
    // agree (the same game's combined points, regardless of how we
    // labelled "home"). LightGBM tree ensembles aren't perfectly
    // symmetric in features though, so even at venue=0 the two calls
    // disagree by a few tenths. Average them to force exact equality.
    let symmetric_total = 0.5 * (fwd.prediction.predicted_total + rev.prediction.predicted_total);

    // Symmetrise feature values and contributions the same way: each is
    // averaged against its sign-flipped counterpart from the reverse
    // call, except flag features (venue, is_conference_game) whose
    // values stay the same regardless of team order. Contributions
    // always flip uniformly because the margin frame flips.
    let mut feature_values = [0.0_f32; NUM_FEATURES];
    let mut contributions = [0.0_f32; NUM_FEATURES];
    for i in 0..NUM_FEATURES {
        let fv_rev_in_home_frame = if is_flag_feature(i) {
            rev.feature_values[i]
        } else {
            -rev.feature_values[i]
        };
        feature_values[i] = 0.5 * (fwd.feature_values[i] + fv_rev_in_home_frame);
        contributions[i] = 0.5 * (fwd.contributions[i] - rev.contributions[i]);
    }

    Explained {
        prediction: Prediction {
            predicted_margin: symmetric_margin,
            home_win_probability: margin_to_win_prob(symmetric_margin, is_pit),
            predicted_total: symmetric_total,
        },
        feature_values,
        contributions,
    }
}

/// Per-matchup projection summary for surfaces that don't need the full
/// explainability payload — the score-ticker upcoming-games strip and
/// the TeamDetail schedule's Projected column. All values are from
/// `home_team_id`'s perspective.
///
/// Score derivation: `home + away` is the model's `predicted_total`,
/// `home - away` is the model's `predicted_margin`. Rounded once at
/// the end so the two integers reconcile (`home + away ==
/// round(total)` exactly).
#[derive(Debug, Clone, Copy)]
pub struct ProjectionSummary {
    pub margin: f32,
    pub home_win_prob: f64,
    pub home_score: i32,
    pub away_score: i32,
}

/// Blend an already-computed matchup prediction with an already-resolved
/// preseason margin and reduce it to the served [`ProjectionSummary`].
///
/// Pure, and split from [`summarize_projection`] for the nightly
/// `game_projections` writer: it holds every team's projected AdjEM in memory
/// (one query for the season) and would otherwise re-read
/// `team_preseason_projection` twice per game, ~12,000 times a sweep. Sharing
/// this function is what makes a precomputed row equal the live one rather
/// than merely close to it.
///
/// `pre_margin` is the preseason AdjEM difference plus venue HCA, home
/// perspective — `None` when either team has no projection row, which
/// disengages the blend the same way a zero weight does.
pub fn summarize_with_preseason(
    clock: BlendClock,
    season: i32,
    pre_margin: Option<f32>,
    explained: &Explained,
) -> ProjectionSummary {
    // Same early-season preseason × pit blend as the `/api/predict` handler
    // (shared [`blend_margins`]), so TeamDetail's Projected column and the
    // ScoreTicker tiles agree with the Predict page on the same matchup
    // (ROADMAP §6). The blend is a scalar mix of the venue-resolved margin,
    // preserving neutral symmetry.
    let pit_margin = explained.prediction.predicted_margin;
    let blend = pre_margin
        .and_then(|pre| blend_margins(clock.blend_weight(season), pre, pit_margin, clock.is_pit()));
    let blended_margin = blend.map(|b| b.margin).unwrap_or(pit_margin);
    let home_win_prob = match blend {
        Some(b) => b.win_prob,
        None => explained.prediction.home_win_probability,
    };

    let total = explained.prediction.predicted_total as f64;
    let margin = blended_margin as f64;
    ProjectionSummary {
        margin: blended_margin,
        home_win_prob,
        home_score: ((total + margin) / 2.0).round() as i32,
        away_score: ((total - margin) / 2.0).round() as i32,
    }
}

/// [`summarize_with_preseason`] with the preseason leg fetched from the
/// database. `home_team_id` is always the host here, so the venue is Home (or
/// Neutral).
pub async fn summarize_projection(
    pool: &PgPool,
    season: i32,
    home_team_id: Uuid,
    away_team_id: Uuid,
    is_neutral: bool,
    clock: BlendClock,
    explained: &Explained,
) -> ProjectionSummary {
    // Resolve the weight BEFORE the lookup: outside the ~6-week decay window
    // the blend is off and the two `team_preseason_projection` reads would be
    // pure overhead on every projection the site serves.
    let pre_margin = if clock.blend_weight(season) > 0.0 {
        let venue = if is_neutral {
            Venue::Neutral
        } else {
            Venue::Home
        };
        fetch_preseason_margin(pool, season, home_team_id, away_team_id, venue).await
    } else {
        None
    };
    summarize_with_preseason(clock, season, pre_margin, explained)
}

/// Full live projection for one matchup: fetch features, run the models,
/// blend, and reduce to a [`ProjectionSummary`].
#[allow(clippy::too_many_arguments)]
pub async fn predict_projection(
    pool: &PgPool,
    predictor: &Predictor,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    clock: BlendClock,
) -> Result<ProjectionSummary, String> {
    let as_of_date = clock.as_of_date();
    let venue = if is_neutral {
        Venue::Neutral
    } else {
        Venue::Home
    };
    let explained = predict_with_venue(
        pool,
        predictor,
        home_team_id,
        away_team_id,
        season,
        venue,
        is_conference,
        as_of_date,
        // The Projected column and the ScoreTicker discard contributions, so
        // don't pay for the tree walk that builds them.
        Attribution::Skip,
    )
    .await?;
    Ok(summarize_projection(
        pool,
        season,
        home_team_id,
        away_team_id,
        is_neutral,
        clock,
        &explained,
    )
    .await)
}

/// Standard deviation of college basketball game-margin residuals, by
/// bundle. Sourced from each model's `backtest_margin.rmse` in
/// `training/models/{,pit_}model_meta.json` — re-measure and update
/// whenever the bundle is retrained; the value materially affects how
/// aggressively `home_win_probability` moves away from 0.5 per point of
/// predicted margin.
///
/// Current values from the 12-season retrain artifacts:
///   - Prod (end-of-season): 10.46, fit on 2014–2026 cohort.
///   - Pit (`pit_cam_v3`):   11.03 — point-in-time features carry more
///     residual variance, so the win-prob calibration is correspondingly
///     less sharp. Reusing the prod σ for pit margins (as the prior
///     single-constant code did) over-confidence-ed honest predictions
///     by ~0.5pp near the 50/50 boundary.
const PREDICT_SIGMA_PROD: f64 = 10.46;
const PREDICT_SIGMA_PIT: f64 = 11.03;

/// Logistic approximation of `Φ(margin / σ)` — the probability that the
/// actual margin exceeds zero given a predicted margin and a residual
/// stddev `σ`. The 1.6 scaling constant matches the logistic CDF to the
/// standard normal CDF; the two agree to ≤1pp across the realistic
/// prediction range. We use logistic instead of erf to avoid pulling in a
/// numerics dependency for a single call site.
///
/// `is_pit` picks the matching bundle's σ — feeding a pit margin through
/// the prod σ (or vice versa) is the same flavor of train/serve skew the
/// audit caught for features, just on the calibration side.
pub fn margin_to_win_prob(margin: f32, is_pit: bool) -> f64 {
    const LOGISTIC_GAUSSIAN_SCALE: f64 = 1.6;
    let sigma = if is_pit {
        PREDICT_SIGMA_PIT
    } else {
        PREDICT_SIGMA_PROD
    };
    let z = LOGISTIC_GAUSSIAN_SCALE * (margin as f64) / sigma;
    1.0 / (1.0 + (-z).exp())
}

/// Home-court advantage in points, added to the preseason AdjEM-diff margin
/// for home games. The preseason projection is a *neutral* team-strength
/// delta (the pit/predict model bakes HCA into its margin via the venue
/// flag; the AdjEM diff does not), so the blend's preseason leg must add it
/// explicitly. ~3.5 is the college-basketball consensus; the blend backtest
/// (`measure-blend-accuracy`) can retune it.
const PRESEASON_HOME_COURT_ADVANTAGE: f32 = 3.5;

/// Peak weight on the PRESEASON leg, at the season open (Nov 1). Calibrated by
/// `measure-blend-accuracy` pooled over 2024–2026: a 0.70/0.30 preseason/pit
/// mix at tip-off beats pure preseason — the two imperfect, partly-uncorrelated
/// legs ensemble (opening-week blended MAE 9.84 vs preseason-only 10.91).
const PRESEASON_PEAK_WEIGHT: f32 = 0.70;

/// Days after Nov 1 over which the preseason weight decays linearly to 0.
/// Calibrated to 42 (≈ mid-December): pit overtakes preseason ~2 weeks into the
/// season, so the old Jan-15 (75-day) endpoint kept weight on a stale prior for
/// a month too long. The 0.70/42-day schedule lands pooled blended MAE 8.80 vs
/// 9.01 for the old 1.0/75-day curve — within 0.03 of the per-week oracle.
const PRESEASON_DECAY_DAYS: i64 = 42;

/// Weight on the PRESEASON projection in the early-season blend: `PEAK` at the
/// Nov 1 open, linear decay to 0.0 over `DECAY_DAYS`, then 0.0 (pure pit).
/// cstat-season `S` runs Nov (S−1) → Apr S, so the open is `(S−1)-11-01`.
/// Calibrated v2 (ROADMAP §6) — see the two consts above; re-tune with
/// `cstat-ingest measure-blend-accuracy --years 2024,2025,2026`.
pub fn preseason_blend_weight(as_of: NaiveDate, season: i32) -> f32 {
    let Some(open) = NaiveDate::from_ymd_opt(season - 1, 11, 1) else {
        return 0.0;
    };
    let d = (as_of - open).num_days();
    if d <= 0 {
        return PRESEASON_PEAK_WEIGHT;
    }
    if d >= PRESEASON_DECAY_DAYS {
        return 0.0;
    }
    (PRESEASON_PEAK_WEIGHT * (1.0 - d as f32 / PRESEASON_DECAY_DAYS as f32))
        .clamp(0.0, PRESEASON_PEAK_WEIGHT)
}

/// LIVE-path blend weight: today's decay weight, but **zero before the
/// season's Nov 1 open**. The explicit-`as_of_date` path deliberately allows
/// pre-open probing; the live path must not — see [`apply_preseason_blend`].
pub fn live_blend_weight(today: NaiveDate, season: i32) -> f32 {
    let Some(open) = NaiveDate::from_ymd_opt(season - 1, 11, 1) else {
        return 0.0;
    };
    if today < open {
        return 0.0;
    }
    preseason_blend_weight(today, season)
}

/// Outcome of an engaged preseason blend: the mixed margin, the win
/// probability derived from it, and the preseason weight (for basis labels).
#[derive(Clone, Copy)]
pub struct BlendedPrediction {
    pub margin: f32,
    pub win_prob: f64,
    pub weight: f32,
}

/// The early-season preseason × pit blend, shared by the `/api/predict`
/// handler, `predict_projection`, and the nightly `game_projections` writer so
/// every surface (Predict page, TeamDetail Projected column, ScoreTicker)
/// mixes identically — this block previously lived as two hand-synced copies
/// and each fix had to be applied twice.
///
/// Semantics:
/// - **`BlendClock::AsOf`** — the weight comes from that date. Pre-open
///   dates get the 0.70 peak (deliberate preseason probing, floor-guarded to
///   Sep 1 by the handler's validation).
/// - **`BlendClock::Live`** — the weight comes from *today*, but ONLY inside
///   the in-season window (Nov 1 open onward): opening-week live predictions
///   anchor on the preseason projection instead of a 1–2 game sample, while a
///   pre-open live request (e.g. browsing next season's matchups in October)
///   stays un-blended — its non-preseason leg would be the degenerate
///   empty-season model output, which would dilute the preseason forecast
///   rather than sharpen it. Past the 42-day decay the weight is 0 either way,
///   so off-season behavior is untouched.
/// - Returns `None` when the blend is inactive (weight 0, or either team has
///   no `team_preseason_projection` row) — callers fall back to the pure
///   model prediction.
/// - The win probability converts the blended margin with the σ of the
///   **model bundle that produced the margin leg**: pit σ on the `AsOf` path
///   (the leg is the honest pit model), prod σ on the `Live` path (the leg is
///   the prod/leaky model). This keeps each path self-consistent with its own
///   bundle's calibration, and keeps the live win% continuous across the
///   Dec-13 decay boundary, where the blend turns off and the response reverts
///   to the prod bundle.
pub async fn apply_preseason_blend(
    pool: &PgPool,
    season: i32,
    home_id: Uuid,
    away_id: Uuid,
    venue: Venue,
    clock: BlendClock,
    pit_margin: f32,
) -> Option<BlendedPrediction> {
    let weight = clock.blend_weight(season);
    if weight <= 0.0 {
        return None;
    }
    let pre_margin = fetch_preseason_margin(pool, season, home_id, away_id, venue).await?;
    blend_margins(weight, pre_margin, pit_margin, clock.is_pit())
}

/// The scalar mix itself, split out so a batch caller that already holds both
/// teams' preseason AdjEM (fetched once per season, not once per game) does
/// not re-query `team_preseason_projection` 6,000 times.
pub fn blend_margins(
    weight: f32,
    pre_margin: f32,
    pit_margin: f32,
    is_pit: bool,
) -> Option<BlendedPrediction> {
    if weight <= 0.0 {
        return None;
    }
    let margin = weight * pre_margin + (1.0 - weight) * pit_margin;
    Some(BlendedPrediction {
        margin,
        win_prob: margin_to_win_prob(margin, is_pit),
        weight,
    })
}

/// Venue adjustment applied to a preseason AdjEM difference, in points.
pub fn preseason_venue_hca(venue: Venue) -> f32 {
    match venue {
        Venue::Home => PRESEASON_HOME_COURT_ADVANTAGE,
        Venue::Away => -PRESEASON_HOME_COURT_ADVANTAGE,
        Venue::Neutral => 0.0,
    }
}

/// Preseason game margin (home-team perspective) from the two teams'
/// persisted projected AdjEM plus venue HCA. `None` when either team has no
/// projection row (too-thin roster, or a season `compute-projections` hasn't
/// run for) — the caller then falls back to pit-only.
async fn fetch_preseason_margin(
    pool: &PgPool,
    season: i32,
    home_id: Uuid,
    away_id: Uuid,
    venue: Venue,
) -> Option<f32> {
    async fn adjem(pool: &PgPool, season: i32, id: Uuid) -> Option<f32> {
        sqlx::query_scalar::<_, f32>(
            "SELECT projected_adj_em FROM team_preseason_projection \
             WHERE season = $1 AND team_id = $2",
        )
        .bind(season)
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    }
    let (home_adjem, away_adjem) =
        tokio::join!(adjem(pool, season, home_id), adjem(pool, season, away_id));
    Some(home_adjem? - away_adjem? + preseason_venue_hca(venue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_data_is_tagged_for_404_other_errors_are_not() {
        // A RowNotFound (team has no stats for the season) is tagged so the
        // route returns 404 instead of 500 (and so it doesn't page #errors-api).
        let missing = classify_feature_error(sqlx::Error::RowNotFound, 2021, "feature extraction");
        assert!(
            missing.starts_with(NO_PREDICTION_DATA_PREFIX),
            "RowNotFound must be tagged as missing-data, got: {missing}"
        );
        // A genuine failure keeps the plain message → stays a 500.
        let real = classify_feature_error(sqlx::Error::PoolTimedOut, 2021, "feature extraction");
        assert!(!real.starts_with(NO_PREDICTION_DATA_PREFIX));
        assert!(real.contains("feature extraction failed"));
    }

    #[test]
    fn margin_to_win_prob_is_well_calibrated() {
        for is_pit in [false, true] {
            // 0 margin → exact 50/50 regardless of bundle.
            assert!((margin_to_win_prob(0.0, is_pit) - 0.5).abs() < 1e-9);

            // Antisymmetric around 0: p(m) + p(-m) = 1. Guarantees
            // `predicted_winner` derived from win prob always agrees with
            // the sign of the margin.
            for m in [1.0, 5.0, 11.0, 25.0, -3.0, -17.5_f32] {
                let p = margin_to_win_prob(m, is_pit);
                let p_neg = margin_to_win_prob(-m, is_pit);
                assert!(
                    (p + p_neg - 1.0).abs() < 1e-9,
                    "is_pit={is_pit} p({m}) + p({}) = {p} + {p_neg} ≠ 1.0",
                    -m,
                );
            }

            // Monotonic in margin.
            for (lo, hi) in [(0.0_f32, 1.0_f32), (1.0, 5.0), (5.0, 15.0), (-2.0, 2.0)] {
                assert!(
                    margin_to_win_prob(lo, is_pit) < margin_to_win_prob(hi, is_pit),
                    "is_pit={is_pit} p({lo}) ≥ p({hi}) — monotonicity broken",
                );
            }

            // Sanity: margin-sign and (prob > 0.5) agree.
            for m in [-10.0, -1.0, -0.1, 0.1, 1.0, 10.0_f32] {
                let p = margin_to_win_prob(m, is_pit);
                assert_eq!(
                    m > 0.0,
                    p > 0.5,
                    "is_pit={is_pit} sign disagreement at margin={m}: prob={p}",
                );
            }
        }

        // Cross-bundle: at any margin > 0, the pit bundle (larger σ) is
        // less confident than the prod bundle. This is the load-bearing
        // calibration property that motivated the fix.
        for m in [1.0_f32, 5.0, 10.0, 20.0] {
            let p_prod = margin_to_win_prob(m, false);
            let p_pit = margin_to_win_prob(m, true);
            assert!(
                p_pit < p_prod,
                "pit bundle should be less confident than prod at margin={m}: pit={p_pit} prod={p_prod}",
            );
        }
    }

    #[test]
    fn neutral_symmetry_combination_is_exact() {
        // Sanity-check the math: the symmetric averaging must guarantee
        // margin(A,B) + margin(B,A) == 0, p(A,B) + p(B,A) == 1.0, and
        // total(A,B) == total(B,A) for any pair of forward/reverse
        // Prediction values. Margin/win-prob average antisymmetrically;
        // totals average additively (the same game's combined points
        // shouldn't change based on which side we labelled "home").
        let fwd = Prediction {
            predicted_margin: 7.3,
            home_win_probability: 0.78,
            predicted_total: 148.4,
        };
        let rev = Prediction {
            predicted_margin: -7.1, // not perfectly antisymmetric (the bug we're fixing)
            home_win_probability: 0.21,
            predicted_total: 148.6, // not perfectly symmetric either
        };

        let m_ab = 0.5 * (fwd.predicted_margin - rev.predicted_margin);
        let p_ab = 0.5 * (fwd.home_win_probability + (1.0 - rev.home_win_probability));
        let t_ab = 0.5 * (fwd.predicted_total + rev.predicted_total);

        // Now reversed call: forward becomes the original reverse, and vice versa.
        let m_ba = 0.5 * (rev.predicted_margin - fwd.predicted_margin);
        let p_ba = 0.5 * (rev.home_win_probability + (1.0 - fwd.home_win_probability));
        let t_ba = 0.5 * (rev.predicted_total + fwd.predicted_total);

        assert!((m_ab + m_ba).abs() < 1e-9, "margins should sum to 0");
        assert!(
            (p_ab + p_ba - 1.0).abs() < 1e-9,
            "win probs should sum to 1"
        );
        assert!(
            (t_ab - t_ba).abs() < 1e-9,
            "totals should be equal under team swap"
        );
    }

    #[test]
    fn preseason_blend_weight_schedule() {
        // Calibrated v2: peak 0.70 at the Nov 1 open, linear decay to 0 over 42
        // days (≈ Dec 13). cstat-season 2026 opens 2025-11-01.
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

        // Before / at the Nov 1 open → peak preseason weight (0.70).
        assert_eq!(
            preseason_blend_weight(d(2025, 9, 15), 2026),
            PRESEASON_PEAK_WEIGHT
        );
        assert_eq!(
            preseason_blend_weight(d(2025, 11, 1), 2026),
            PRESEASON_PEAK_WEIGHT
        );

        // At / after open + 42 days (2025-12-13) → pure pit.
        assert_eq!(preseason_blend_weight(d(2025, 12, 13), 2026), 0.0);
        assert_eq!(preseason_blend_weight(d(2026, 1, 15), 2026), 0.0);
        assert_eq!(preseason_blend_weight(d(2026, 4, 1), 2026), 0.0);

        // Monotonically decreasing strictly inside the window, bounded by peak.
        let early_nov = preseason_blend_weight(d(2025, 11, 8), 2026);
        let mid_nov = preseason_blend_weight(d(2025, 11, 20), 2026);
        let early_dec = preseason_blend_weight(d(2025, 12, 5), 2026);
        assert!(early_nov > mid_nov && mid_nov > early_dec);
        assert!((0.0..=PRESEASON_PEAK_WEIGHT).contains(&mid_nov));

        // Halfway through the 42-day decay (day 21 ≈ Nov 22) → peak/2 = 0.35.
        let halfway = preseason_blend_weight(d(2025, 11, 22), 2026);
        assert!(
            (halfway - PRESEASON_PEAK_WEIGHT / 2.0).abs() < 0.03,
            "midpoint weight {halfway} should be ≈{}",
            PRESEASON_PEAK_WEIGHT / 2.0,
        );

        // Season-relative: the same calendar offset in 2025's season
        // (opens 2024-11-01) decays identically.
        assert_eq!(
            preseason_blend_weight(d(2024, 11, 1), 2025),
            PRESEASON_PEAK_WEIGHT
        );
        assert_eq!(preseason_blend_weight(d(2024, 12, 13), 2025), 0.0);
    }

    #[test]
    fn live_blend_weight_gates_pre_open_dates() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

        // Pre-open live requests must NOT blend: preseason_blend_weight
        // returns the 0.70 peak for any date at-or-before the open, which is
        // fine for deliberate as_of_date probing but would blend a September
        // request for next season against a degenerate empty-season margin.
        assert_eq!(live_blend_weight(d(2026, 9, 15), 2027), 0.0);
        assert_eq!(live_blend_weight(d(2026, 10, 31), 2027), 0.0);

        // From the open onward, live matches the explicit-date schedule.
        assert_eq!(
            live_blend_weight(d(2026, 11, 1), 2027),
            PRESEASON_PEAK_WEIGHT
        );
        assert_eq!(
            live_blend_weight(d(2026, 11, 8), 2027),
            preseason_blend_weight(d(2026, 11, 8), 2027)
        );

        // Off-season / mid-season: no-op, matching the pre-live-blend world.
        assert_eq!(live_blend_weight(d(2026, 7, 14), 2026), 0.0);
        assert_eq!(live_blend_weight(d(2026, 2, 1), 2026), 0.0);
    }

    #[test]
    fn blend_clock_pit_predicate_matches_as_of() {
        // The pit predicate and the feature-path cutoff are the SAME bit: an
        // explicit as-of cutoff means pit features went in, so the pit sigma
        // must come out. Splitting them is the calibration-side train/serve
        // skew the two sigmas exist to prevent.
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(BlendClock::AsOf(d).as_of_date(), Some(d));
        assert!(BlendClock::AsOf(d).is_pit());
        assert_eq!(BlendClock::Live(d).as_of_date(), None);
        assert!(!BlendClock::Live(d).is_pit());
    }

    #[test]
    fn blend_margins_is_a_scalar_mix_and_off_at_zero_weight() {
        // The batch writer reaches the mix through blend_margins directly
        // (it holds both AdjEMs already); it must agree with the fetching
        // wrapper on the arithmetic and on the disengaged case.
        assert!(blend_margins(0.0, 5.0, -1.0, true).is_none());
        let b = blend_margins(0.25, 8.0, 0.0, true).expect("engaged");
        assert!((b.margin - 2.0).abs() < 1e-6);
        assert_eq!(b.weight, 0.25);
        assert!((b.win_prob - margin_to_win_prob(b.margin, true)).abs() < 1e-12);
    }

    #[test]
    fn preseason_venue_hca_is_antisymmetric() {
        assert_eq!(preseason_venue_hca(Venue::Neutral), 0.0);
        assert_eq!(
            preseason_venue_hca(Venue::Home),
            -preseason_venue_hca(Venue::Away)
        );
    }
}
