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
use std::collections::HashMap;
use uuid::Uuid;

use crate::features::{self, GameFeatures, TeamSeason};
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

/// Prefix marking a matchup the engine refuses to answer *as asked* — the
/// request combined options that have no coherent joint meaning, rather than
/// naming data we happen not to hold. Today that is exactly one case:
/// point-in-time plus two different seasons (see [`predict_matchup`]).
///
/// It needs its own prefix because the route's two existing outcomes are both
/// wrong for it. [`NO_PREDICTION_DATA_PREFIX`] would claim we looked and found
/// nothing, when in fact the question was malformed; and the untagged fallback
/// is a 500, which the `guards.rs` 5xx tap posts to `#errors-api` — paging a
/// human for a bad query string is the precise false-fire the route's error
/// classifier was written to avoid. The route maps this to **400**.
pub const INVALID_MATCHUP_PREFIX: &str = "invalid matchup";

/// Turn a feature-extraction sqlx error into the route-facing message, tagging
/// the missing-data case with [`NO_PREDICTION_DATA_PREFIX`] (see there).
fn classify_feature_error(
    e: sqlx::Error,
    home: TeamSeason,
    away: TeamSeason,
    what: &str,
) -> String {
    match e {
        // The same-season wording is preserved verbatim: the predict route
        // matches on NO_PREDICTION_DATA_PREFIX to turn this into a 404, and
        // this is the message users see for the overwhelmingly common case.
        // Cross-era gets its own phrasing because "season {season}" has no
        // single answer, and "not Division I in 2015" is the likeliest cause.
        sqlx::Error::RowNotFound if home.season == away.season => format!(
            "{NO_PREDICTION_DATA_PREFIX}: one or both teams have no data for season {}",
            home.season
        ),
        sqlx::Error::RowNotFound => format!(
            "{NO_PREDICTION_DATA_PREFIX}: one or both teams have no data (home season {}, away season {})",
            home.season, away.season
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
    home: TeamSeason,
    away: TeamSeason,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
    attribution: Attribution,
) -> Result<Explained, String> {
    // Single DB-fetch pass produces both the 49-element diff vector
    // (margin/win input) and the 58-element diff+sum vector (totals
    // input). The feature extraction is the expensive step.
    let f = match as_of_date {
        Some(d) => {
            // `build_all_features_pit` is single-season by signature (its
            // cohort map spans one season's players), so a cross-era request
            // cannot reach it. Callers that can produce two seasons — today
            // only the predict route, via `home_season` / `away_season` —
            // reject the combination at the edge, where they can say which
            // query param to drop; this is the backstop for anything that
            // does not. Tagged
            // INVALID_MATCHUP_PREFIX so that backstop answers 400 rather than
            // a 500 that pages #errors-api over a malformed request.
            if home.season != away.season {
                return Err(format!(
                    "{INVALID_MATCHUP_PREFIX}: point-in-time predictions are single-season; \
                     got home {} vs away {}",
                    home.season, away.season
                ));
            }
            features::build_all_features_pit(
                pool,
                home.id,
                away.id,
                home.season,
                is_neutral,
                is_conference,
                d,
            )
            .await
            .map_err(|e| classify_feature_error(e, home, away, "pit feature extraction"))?
        }
        None => features::build_all_features(pool, home, away, is_neutral, is_conference)
            .await
            .map_err(|e| classify_feature_error(e, home, away, "feature extraction"))?,
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
    home: TeamSeason,
    away: TeamSeason,
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
                home,
                away,
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
            //
            // Each side's season travels with its team through the swap —
            // that's what `TeamSeason` is for. Swapping ids while leaving two
            // loose season scalars in place would look up each team's row in
            // the other team's year, which for a cross-era matchup is a wrong
            // answer that still returns 200.
            let swapped = predict_matchup(
                pool,
                predictor,
                away,
                home,
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
            // Same swap discipline as the Away arm: `(away, home)` carries
            // both seasons along with both ids.
            let (fwd, rev) = tokio::try_join!(
                predict_matchup(
                    pool,
                    predictor,
                    home,
                    away,
                    true,
                    is_conference,
                    as_of_date,
                    attribution,
                ),
                predict_matchup(
                    pool,
                    predictor,
                    away,
                    home,
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
    // Deliberately still a single `season`: this serves REAL scheduled games
    // (the Projected column, the ScoreTicker, the nightly `game_projections`
    // writer), and both sides of a real game are always the same year. Cross-era
    // matchups enter through `predict_with_venue` directly.
    let (home, away) = TeamSeason::same_season(home_team_id, away_team_id, season);
    let explained = predict_with_venue(
        pool,
        predictor,
        home,
        away,
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
    win_prob_at_sigma(
        margin,
        if is_pit {
            PREDICT_SIGMA_PIT
        } else {
            PREDICT_SIGMA_PROD
        },
    )
}

/// [`margin_to_win_prob`] with the residual stddev supplied directly, for the
/// one regime whose margin comes from no model bundle at all — the
/// preseason-only path, whose σ is measured against actual game margins
/// rather than read from a model meta (see [`PRESEASON_ONLY_SIGMA`]).
///
/// Everything else should go through `margin_to_win_prob`: picking a σ that
/// does not belong to the thing that produced the margin is the calibration
/// half of the train/serve skew the feature audit caught.
fn win_prob_at_sigma(margin: f32, sigma: f64) -> f64 {
    const LOGISTIC_GAUSSIAN_SCALE: f64 = 1.6;
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
    let diff = fetch_preseason_adj_em_diff(pool, season, home_id, away_id).await?;
    Some(diff + preseason_venue_hca(venue))
}

/// `projected_adj_em(home) − projected_adj_em(away)`, before any venue or
/// scale adjustment. `None` when either team has no row.
///
/// Split out from [`fetch_preseason_margin`] because the two regimes that read
/// it disagree about what to do with it, on purpose: the blend leg adds a bare
/// HCA and leaves the scale alone (the shipped, calibrated-as-a-pair form),
/// while [`preseason_only_prediction`] applies its own slope. Sharing the
/// *query* and not the arithmetic is what keeps a change to one from silently
/// moving the other.
async fn fetch_preseason_adj_em_diff(
    pool: &PgPool,
    season: i32,
    home_id: Uuid,
    away_id: Uuid,
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
    Some(home_adjem? - away_adjem?)
}

// ---------------------------------------------------------------------------
// The preseason-only regime (#387)
// ---------------------------------------------------------------------------

/// Scale factor on the preseason AdjEM difference when it is the WHOLE
/// prediction, converting an efficiency edge per 100 possessions into points
/// on a scoreboard.
///
/// Two effects push the same way, and neither is in the blend leg's form:
///
///   * **possessions.** AdjEM is per 100; a game is ~68, so a 10-point
///     efficiency edge is worth ~6.8 points of margin;
///   * **attenuation.** The projection is a forecast with error, and the
///     minimum-MSE linear map from a noisy predictor onto the truth shrinks it
///     by `var(signal) / var(predictor)`.
///
/// Measured walk-forward (fit on seasons < S, score S, S = 2021..2026) over
/// 25,501 completed games by
/// `training/experiments/experiment_preseason_margin_calibration.py`. Serving
/// the difference at unit scale — which is what
/// [`fetch_preseason_margin`] does — over-predicts the home side by **+1.73
/// points on average and +6.56 on games with a 15-point projected gap**, which
/// is most of a projected non-conference schedule. Calibrated: pooled MAE
/// 10.43 → 9.61, bias +1.73 → −0.03, 6/6 seasons, paired z −27.3.
///
/// The per-fold fit lands 0.574 → 0.595 with no trend worth chasing; 0.59 is
/// the rounded value, and scoring the rounded pair as its own candidate costs
/// nothing (MAE 9.610 vs 9.613 for the per-fold refit).
///
/// **Deliberately not applied to the blend leg.** The blend's 0.70 peak weight
/// and 42-day decay were calibrated *with* the unit-scale leg in place
/// (`measure-blend-accuracy` grid-searches `hca × w_max × end_day`, never a
/// slope), so the two constants absorbed part of this error. Fixing the leg
/// means re-deriving the schedule against a changed leg and moving every
/// served early-season prediction and every `game_projections` row — a
/// separate change, tracked in #390.
const PRESEASON_ONLY_SLOPE: f32 = 0.59;

/// Home-court advantage for the preseason-only regime, in points.
///
/// Fit alongside the slope on the same folds (3.14 → 3.36 per fold, 3.2
/// rounded). Notably close to the blend leg's
/// [`PRESEASON_HOME_COURT_ADVANTAGE`] of 3.5 — which is the useful part of the
/// result: refitting the intercept *alone*, with the scale left at 1.0, drags
/// it down to 1.67 and still loses (MAE 10.33, z −9.4). The shipped HCA was
/// never the problem; it was absorbing the scale error.
///
/// **A neutral floor is worth zero here, and that is a measurement rather
/// than an assumption.** Fit pooled, neutral games want a residual home
/// advantage of +0.6 to +0.9 — stable across all six folds, so not noise.
/// Split by date it resolves: early-season multi-team tournaments, the
/// genuinely neutral ones, sit at **+0.25**, while mid-season "neutral" games
/// sit at **+2.42**. The latter are conference games moved to a city arena
/// and in-state rivalries at a shared venue — a home game for one side that
/// `games.is_neutral_site` has labelled neutral. So the pooled figure
/// measures label noise in a population this regime never serves: a
/// pre-tipoff forecast is asked about November tournaments, not February.
/// Scored on early-season neutral games alone, serving 0 beats the per-venue
/// fit on both counts (MAE 9.564 vs 9.609, bias −0.26 vs +0.68); the
/// per-venue variant's pooled edge comes entirely from mid-season games, by
/// fitting the artifact. `neutral_label_diagnostic` in the experiment
/// re-derives this.
const PRESEASON_ONLY_HCA: f32 = 3.2;

/// Residual stddev for the preseason-only margin, feeding
/// [`win_prob_at_sigma`].
///
/// Neither model bundle's σ applies: this margin comes from no bundle. Fit on
/// the same walk-forward folds by minimising log loss under the served
/// logistic — not set to the residual RMSE (12.18), because
/// `margin_to_win_prob`'s 1.6 gaussian-matching constant makes the
/// best-calibrating scale a different quantity. Per-fold 11.05 → 11.20, so
/// 11.1; at the shipped slope and HCA that gives an expected calibration error
/// of 0.0097 across ten deciles, against 0.0241 for the unit-scale form. Its closeness to
/// [`PREDICT_SIGMA_PIT`] (11.03) is a coincidence of two unrelated fits and
/// not a reason to share a constant.
const PRESEASON_ONLY_SIGMA: f64 = 11.1;

// The three properties the measurement established, independent of the exact
// fitted values — a const block rather than a test, so an edit that breaks one
// fails to compile instead of failing a suite someone can skip.
const _: () = {
    // The slope is a SHRINK. A value at or above 1.0 would mean the AdjEM
    // difference under-states the game margin, which is the opposite of both
    // effects it corrects for.
    assert!(PRESEASON_ONLY_SLOPE > 0.0 && PRESEASON_ONLY_SLOPE < 1.0);
    // A forecast made before a ball is tipped must be less certain than one
    // from a model that has watched games, so its σ has to be wider than
    // either bundle's. Sharing a bundle's σ here — the tempting shortcut — is
    // exactly what this forbids.
    assert!(PRESEASON_ONLY_SIGMA > PREDICT_SIGMA_PROD);
    assert!(PRESEASON_ONLY_SIGMA > PREDICT_SIGMA_PIT);
};

/// A margin and win probability derived entirely from the two teams'
/// `team_preseason_projection` rows — no ONNX inference, no season stats, no
/// Torvik.
#[derive(Clone, Copy, Debug)]
pub struct PreseasonOnlyPrediction {
    /// Projected home margin, calibrated. Home perspective.
    pub margin: f32,
    pub home_win_prob: f64,
    /// The raw `projected_adj_em` difference behind it, before scale and
    /// venue. Carried so a caller can show the strength gap it came from
    /// rather than re-querying.
    pub adj_em_diff: f32,
}

/// Whether the preseason-only regime applies to this matchup, and if so what
/// it says.
///
/// Three outcomes rather than an `Option`, because "the season has started, so
/// run the model" and "the season has not started and we hold nothing for this
/// team" are different answers that need different HTTP statuses, and
/// collapsing them is how a too-thin team ends up served a 500 or, worse, a
/// model prediction built from an empty cohort.
#[derive(Clone, Copy, Debug)]
pub enum PreseasonOnlyRegime {
    /// At least one of the two teams has played. The ordinary feature/model
    /// path applies and this regime must not engage — a played team's stats
    /// are the better evidence, and the blend already handles the weeks after
    /// that, weighting the same projection at 0.70 and decaying it out.
    NotApplicable,
    /// Neither team has played, and both carry a projection row.
    Serve(PreseasonOnlyPrediction),
    /// Neither team has played, and at least one side has no projection row
    /// (a too-thin roster, or a season `compute-projections` has not run
    /// for). 74 of 364 teams on the 2027 board are in this state.
    NoProjection,
}

/// Calibrated preseason margin from an AdjEM difference and a venue.
///
/// Public and pure so the identity `margin(diff, Home) − margin(diff, Away) ==
/// 2 × HCA` and the scale itself are testable without a database, and so the
/// per-game writer #388 will need can reuse the exact arithmetic instead of
/// re-deriving it.
pub fn preseason_only_margin(adj_em_diff: f32, venue: Venue) -> f32 {
    let hca = match venue {
        Venue::Home => PRESEASON_ONLY_HCA,
        Venue::Away => -PRESEASON_ONLY_HCA,
        Venue::Neutral => 0.0,
    };
    PRESEASON_ONLY_SLOPE * adj_em_diff + hca
}

/// Win probability for a preseason-only margin, at that regime's own σ.
pub fn preseason_only_win_prob(margin: f32) -> f64 {
    win_prob_at_sigma(margin, PRESEASON_ONLY_SIGMA)
}

/// Has this team played a game to a final score in this season?
///
/// The precondition for the preseason-only regime, asked **per team rather
/// than per season**, and that distinction is the whole behaviour of the
/// regime through opening week. A season-level "has anything been played"
/// test collapses the moment the first game ends: on the night of the opener
/// one result would send the other ~360 teams' matchups back to a path that
/// has nothing to build a feature vector from, for another week. Asked per
/// team, each side leaves the regime when it has actually played, which is
/// when there is finally something better to answer with.
///
/// Deliberately a positive fact about the team rather than a fallback on
/// feature extraction failing. Those are not the same set: a team that played
/// and then lost its `team_season_stats` row is a data gap the pipeline needs
/// to surface, and — worse — a team that has a *blank* row (the `/teams`
/// ingest step writes one before any box score exists) does not fail
/// extraction at all. It yields an all-default feature vector and a confident
/// garbage margin. Keying on games played catches that case; keying on the
/// error could not see it.
///
/// Errs toward `true` on a query failure: that routes to the ordinary path,
/// which reports its own error honestly, rather than answering a live team's
/// matchup from a preseason prior.
///
/// Two `EXISTS` probes against `idx_games_home_team` / `idx_games_away_team`,
/// short-circuiting on the first match, at 0.035–0.037 ms whichever way the
/// answer goes; the two teams' probes run concurrently. They sit on the hot
/// path for every same-season `/api/predict` call, ahead of
/// 49-feature extraction and three ONNX sessions, so the round-trip is worth
/// deciding the regime before doing any of that work.
pub async fn team_has_played(pool: &PgPool, season: i32, team_id: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM games \
           WHERE season = $1 AND home_team_id = $2 \
             AND home_score IS NOT NULL AND away_score IS NOT NULL) \
             OR EXISTS (SELECT 1 FROM games \
           WHERE season = $1 AND away_team_id = $2 \
             AND home_score IS NOT NULL AND away_score IS NOT NULL)",
    )
    .bind(season)
    .bind(team_id)
    .fetch_one(pool)
    .await
    .unwrap_or(true)
}

/// Every team's projected AdjEM for a season, in one query.
///
/// The batch counterpart to [`preseason_only_regime`], for a caller
/// projecting a whole schedule (#388). Resolving the regime per game would
/// cost four round-trips a game — two "has this team played" probes and two
/// anchor lookups — so a 31-game schedule would run 124 queries to compute 31
/// subtractions. This is the same split `blend_margins` exists for on the
/// blend side: share the query, not the arithmetic.
///
/// A team missing from the map has no projection (a roster too thin to
/// project — 74 of 364 on the 2027 board), and a caller must render that as
/// unknown rather than substituting a number.
pub async fn fetch_preseason_adj_em_map(
    pool: &PgPool,
    season: i32,
) -> Result<HashMap<Uuid, f32>, sqlx::Error> {
    let rows: Vec<(Uuid, f32)> = sqlx::query_as(
        "SELECT team_id, projected_adj_em FROM team_preseason_projection WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Resolve [`PreseasonOnlyRegime`] for one matchup.
///
/// Single-season by construction: a cross-era what-if has two seasons, so the
/// two sides' "has it played" answers are not about the same calendar and
/// callers must not reach this with one.
pub async fn preseason_only_regime(
    pool: &PgPool,
    season: i32,
    home_id: Uuid,
    away_id: Uuid,
    venue: Venue,
) -> PreseasonOnlyRegime {
    let (home_played, away_played) = tokio::join!(
        team_has_played(pool, season, home_id),
        team_has_played(pool, season, away_id)
    );
    if home_played || away_played {
        return PreseasonOnlyRegime::NotApplicable;
    }
    match fetch_preseason_adj_em_diff(pool, season, home_id, away_id).await {
        Some(adj_em_diff) => {
            let margin = preseason_only_margin(adj_em_diff, venue);
            PreseasonOnlyRegime::Serve(PreseasonOnlyPrediction {
                margin,
                home_win_prob: preseason_only_win_prob(margin),
                adj_em_diff,
            })
        }
        None => PreseasonOnlyRegime::NoProjection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_data_is_tagged_for_404_other_errors_are_not() {
        let (home, away) = TeamSeason::same_season(Uuid::nil(), Uuid::nil(), 2021);
        // A RowNotFound (team has no stats for the season) is tagged so the
        // route returns 404 instead of 500 (and so it doesn't page #errors-api).
        let missing =
            classify_feature_error(sqlx::Error::RowNotFound, home, away, "feature extraction");
        assert!(
            missing.starts_with(NO_PREDICTION_DATA_PREFIX),
            "RowNotFound must be tagged as missing-data, got: {missing}"
        );
        // Same-season wording is load-bearing for what users read on the
        // common 404; assert it verbatim so the cross-era arm can't capture it.
        assert_eq!(
            missing,
            format!("{NO_PREDICTION_DATA_PREFIX}: one or both teams have no data for season 2021")
        );
        // A genuine failure keeps the plain message → stays a 500.
        let real =
            classify_feature_error(sqlx::Error::PoolTimedOut, home, away, "feature extraction");
        assert!(!real.starts_with(NO_PREDICTION_DATA_PREFIX));
        assert!(real.contains("feature extraction failed"));
    }

    #[test]
    fn cross_era_missing_data_names_both_seasons_and_stays_404_tagged() {
        let missing = classify_feature_error(
            sqlx::Error::RowNotFound,
            TeamSeason::new(Uuid::nil(), 2015),
            TeamSeason::new(Uuid::nil(), 2026),
            "feature extraction",
        );
        // Still a 404, not a 500 — "that program wasn't Division I in 2015" is
        // a client error exactly as the same-season case is.
        assert!(missing.starts_with(NO_PREDICTION_DATA_PREFIX));
        // "season {season}" has no single answer here, so both are named.
        assert!(missing.contains("home season 2015"), "got: {missing}");
        assert!(missing.contains("away season 2026"), "got: {missing}");
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

    // ------------------------------------------------- preseason-only (#387)

    #[test]
    fn preseason_only_margin_applies_the_slope_and_an_antisymmetric_hca() {
        // The scale is the whole point of the regime, so pin it numerically
        // rather than only asserting it is "less than 1": a 10-point
        // efficiency edge is 5.9 points on a neutral floor.
        assert!((preseason_only_margin(10.0, Venue::Neutral) - 5.9).abs() < 1e-5);
        assert_eq!(preseason_only_margin(0.0, Venue::Neutral), 0.0);

        // Venue is additive and antisymmetric, so swapping the two teams and
        // the venue together returns the same game from the other side.
        for d in [-24.0_f32, -7.5, 0.0, 3.0, 31.0] {
            let home = preseason_only_margin(d, Venue::Home);
            let away = preseason_only_margin(d, Venue::Away);
            assert!(
                (home - away - 2.0 * PRESEASON_ONLY_HCA).abs() < 1e-4,
                "diff {d}: home {home} away {away}"
            );
            assert!((preseason_only_margin(-d, Venue::Away) + home).abs() < 1e-4);
        }
    }

    #[test]
    fn preseason_only_shrinks_the_adj_em_diff_that_the_blend_leg_serves_whole() {
        // The bug this regime exists not to have: `fetch_preseason_margin`
        // serves the AdjEM difference at unit scale, which over-predicts by
        // +6.6 points on a 15-point projected gap. Calibrated, the strength
        // term must be strictly smaller in magnitude at every gap — and the
        // wider the gap, the bigger the correction, which is why the error the
        // measurement found is concentrated in blowouts.
        let mut last = 0.0_f32;
        for d in [1.0_f32, 5.0, 10.0, 20.0, 40.0] {
            let calibrated = preseason_only_margin(d, Venue::Neutral);
            let blend_leg = d + preseason_venue_hca(Venue::Neutral);
            assert!(
                calibrated.abs() < blend_leg.abs(),
                "gap {d}: calibrated {calibrated} must shrink the {blend_leg} the blend leg serves"
            );
            let correction = blend_leg - calibrated;
            assert!(
                correction > last,
                "gap {d}: correction {correction} should grow with the gap (was {last})"
            );
            last = correction;
        }
    }

    #[test]
    fn preseason_only_win_prob_is_calibrated_and_less_confident_than_a_model_bundle() {
        assert!((preseason_only_win_prob(0.0) - 0.5).abs() < 1e-12);
        for m in [0.5_f32, 4.0, 12.0, 30.0] {
            let p = preseason_only_win_prob(m);
            // Symmetric about a pick'em, and strictly monotone.
            assert!((p + preseason_only_win_prob(-m) - 1.0).abs() < 1e-12);
            assert!(p > preseason_only_win_prob(m - 0.5), "not monotone at {m}");
            assert!((0.5..1.0).contains(&p), "{m} → {p}");
            // A forecast made before a ball is tipped must be less confident
            // than the same margin from a model that has watched games. This
            // is what a shared σ would have quietly given up: σ 11.1 against
            // the prod bundle's 10.46.
            assert!(
                p < margin_to_win_prob(m, false),
                "{m}: preseason-only {p} must not out-confidence the prod bundle"
            );
        }
    }

    #[test]
    fn a_preseason_only_prediction_is_self_consistent() {
        // The struct a caller reads must agree with the two public helpers —
        // #388 will build a whole schedule from `preseason_only_margin`
        // directly, and a page that disagrees with `/api/predict` on the same
        // game is the failure this pins down.
        for (d, venue) in [(9.0_f32, Venue::Home), (-3.0, Venue::Neutral)] {
            let margin = preseason_only_margin(d, venue);
            let p = PreseasonOnlyPrediction {
                margin,
                home_win_prob: preseason_only_win_prob(margin),
                adj_em_diff: d,
            };
            assert!((p.margin - preseason_only_margin(p.adj_em_diff, venue)).abs() < 1e-6);
            assert!((p.home_win_prob - preseason_only_win_prob(p.margin)).abs() < 1e-12);
        }
    }

    #[test]
    fn the_calibration_constants_are_the_measured_ones() {
        // Guards the three constants against a drive-by edit: each is a
        // walk-forward fit recorded in
        // `eval_history/preseason_margin_calibration_20260927_summary.json`,
        // and changing one without re-running
        // `experiments/experiment_preseason_margin_calibration.py` is the
        // Layer-4 drift `docs/model_dependency_graph.md` §3 warns about.
        // The structural invariants (slope is a shrink, σ is wider than either
        // bundle's) are asserted at compile time beside the constants; these
        // are the exact fitted values, which only a re-run can change.
        assert_eq!(PRESEASON_ONLY_SLOPE, 0.59);
        assert_eq!(PRESEASON_ONLY_HCA, 3.2);
        assert_eq!(PRESEASON_ONLY_SIGMA, 11.1);
    }
}
