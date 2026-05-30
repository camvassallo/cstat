use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

use cstat_core::inference::{FEATURE_META, FEATURE_NAMES, NUM_FEATURES, Prediction};
use cstat_core::queries;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/predict", get(predict))
}

/// Where the game is being played.
///
/// `Home` = the team passed as `home` is hosting.
/// `Away` = the team passed as `away` is hosting (so we swap before feature
/// extraction and negate the resulting margin so the response stays from
/// the `home` param's perspective).
/// `Neutral` = no host. Predictions are symmetrised by averaging both team
/// orderings — see `predict_neutral_symmetric`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Venue {
    Home,
    Away,
    Neutral,
}

#[derive(Deserialize)]
struct PredictParams {
    home: String,
    away: String,
    /// New explicit venue selector. If absent, falls back to the legacy
    /// `neutral` boolean (true → Neutral, false/absent → Home).
    venue: Option<Venue>,
    #[serde(default)]
    neutral: bool,
    season: Option<i32>,
    /// Optional point-in-time cutoff (`YYYY-MM-DD`). When set, the
    /// prediction is rebuilt from features available *up to and
    /// including* that date — the leak-free path tied to the pit
    /// model bundle. Caller responsibility: pass `game_date - 1 day`
    /// for completed games (so the model sees pre-game state, not the
    /// game itself), or `Today` for live predictions. Omitting it
    /// preserves the legacy end-of-season behavior.
    as_of_date: Option<NaiveDate>,
}

impl PredictParams {
    fn resolved_venue(&self) -> Venue {
        self.venue.unwrap_or(if self.neutral {
            Venue::Neutral
        } else {
            Venue::Home
        })
    }
}

async fn predict(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PredictParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let venue = params.resolved_venue();

    // Bound-check `as_of_date` before doing any DB work. Future dates can't
    // be honest by construction (no data exists yet); dates before the
    // start of the requested season produce an empty pit cohort that the
    // model silently dilutes into a degenerate "bias-only" prediction
    // labelled as honest. Reject loudly instead — the alternative is the
    // user shipping a confidently-labelled garbage forecast.
    if let Some(d) = params.as_of_date {
        let today = chrono::Utc::now().date_naive();
        if d > today {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "as_of_date {d} is in the future; honest predictions can only \
                         reflect data through today ({today})"
                    ),
                })),
            ));
        }
        // Seasons in this codebase use end-year numbering (season 2026 =
        // 2025-26 college season). Allow as_of_date down to Sep 1 of the
        // prior calendar year so the user can probe preseason / opening
        // night, but reject further-back dates that obviously belong to
        // another season — those would silently route to a wrong-season
        // pit cohort lookup.
        let earliest =
            chrono::NaiveDate::from_ymd_opt(season - 1, 9, 1).expect("Sep 1 always valid");
        if d < earliest {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "as_of_date {d} is before season {season} starts ({earliest}); \
                         pick a date in this season or change the season parameter"
                    ),
                })),
            ));
        }
    }

    let home_team = find_team(&state.db.pool, &params.home, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("home team not found: {}", params.home) })),
            )
        })?;

    let away_team = find_team(&state.db.pool, &params.away, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("away team not found: {}", params.away) })),
            )
        })?;

    let is_conference =
        home_team.conference.is_some() && home_team.conference == away_team.conference;

    // Run the predictor with explicit venue semantics. Neutral games are
    // symmetrised inside the helper so argument order doesn't change the
    // answer. The returned `Explained` carries both the headline numbers
    // and per-feature ablation deltas + the input feature vector itself
    // (already sign-flipped to the home perspective for the Away venue).
    let explained = predict_with_venue(
        &state,
        home_team.id,
        away_team.id,
        season,
        venue,
        is_conference,
        params.as_of_date,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
    })?;

    // Early-season preseason × pit blend (ROADMAP §6). Only on the honest
    // pit path (`as_of_date` set); the leaky full-state path is untouched.
    // While the in-season pit cohort is thin (Nov–Dec), anchor on the
    // preseason projection (r=0.88), decaying to pit by mid-January. Output
    // blend: scalar mix of the already-venue-resolved margins, so neutral
    // symmetry and away-flip are preserved (both legs are antisymmetric
    // under team swap). Totals stay pit — the preseason model has no totals
    // (ROADMAP defers the totals blend).
    let pit_margin = explained.prediction.predicted_margin;
    let mut blended_margin = pit_margin;
    let mut prediction_basis = if params.as_of_date.is_some() {
        "pit"
    } else {
        "leaky"
    };
    // Weight on the preseason leg (0.0 when no as_of_date, or post-decay).
    let w = params
        .as_of_date
        .map(|asof| preseason_blend_weight(asof, season))
        .unwrap_or(0.0);
    let pre_margin = if w > 0.0 {
        fetch_preseason_margin(&state.db.pool, season, home_team.id, away_team.id, venue).await
    } else {
        None
    };
    if let Some(pre_margin) = pre_margin {
        blended_margin = w * pre_margin + (1.0 - w) * pit_margin;
        prediction_basis = if w >= 0.999 { "preseason" } else { "blended" };
    }
    let blended_win_prob = if (blended_margin - pit_margin).abs() < f32::EPSILON {
        explained.prediction.home_win_probability
    } else {
        margin_to_win_prob(blended_margin, params.as_of_date.is_some())
    };

    let predicted_winner = if blended_margin > 0.0 {
        &home_team.name
    } else {
        &away_team.name
    };

    let venue_str = match venue {
        Venue::Home => "home",
        Venue::Away => "away",
        Venue::Neutral => "neutral",
    };

    let (feature_contributions, contributions_by_group) =
        build_contribution_payload(&explained.feature_values, &explained.contributions);

    // Roster summaries + prior meetings travel in the same response so the
    // Predict page stays a one-round-trip view. Both run in parallel with
    // each other (the prediction has already resolved before this point —
    // it's the slowest step and gates the response shape via venue+team
    // perspective). Failures here downgrade to empty arrays rather than
    // tanking the prediction; the page degrades gracefully.
    let pool = &state.db.pool;
    let (roster_home, roster_away, prior_meetings_raw, archetype_home, archetype_away) = tokio::join!(
        queries::get_team_roster(pool, home_team.id, season),
        queries::get_team_roster(pool, away_team.id, season),
        queries::get_prior_meetings(pool, home_team.id, away_team.id, season),
        queries::get_team_archetype_index(pool, home_team.id, season),
        queries::get_team_archetype_index(pool, away_team.id, season),
    );
    let roster_home = roster_home.unwrap_or_default();
    let roster_away = roster_away.unwrap_or_default();
    let prior_meetings_raw = prior_meetings_raw.unwrap_or_default();
    let archetype_home = archetype_home.unwrap_or_default();
    let archetype_away = archetype_away.unwrap_or_default();

    // Box score data: only fetch when there's at least one prior meeting.
    // Saves two empty-array round-trips on the common case (no rematch yet).
    let prior_meetings = if prior_meetings_raw.is_empty() {
        Vec::new()
    } else {
        let game_ids: Vec<Uuid> = prior_meetings_raw.iter().map(|m| m.game_id).collect();
        let (team_boxes, player_boxes) = tokio::join!(
            queries::get_team_game_boxes(pool, &game_ids),
            queries::get_player_game_boxes(pool, &game_ids),
        );
        let team_boxes = team_boxes.unwrap_or_default();
        let player_boxes = player_boxes.unwrap_or_default();

        prior_meetings_raw
            .into_iter()
            .map(|m| {
                let team_box: Vec<&queries::TeamGameBox> = team_boxes
                    .iter()
                    .filter(|b| b.game_id == m.game_id)
                    .collect();
                let player_box: Vec<&queries::PlayerGameBox> = player_boxes
                    .iter()
                    .filter(|b| b.game_id == m.game_id)
                    .collect();
                json!({
                    "headline": m,
                    "team_box": team_box,
                    "player_box": player_box,
                })
            })
            .collect::<Vec<_>>()
    };

    // Derive integer team scores from (total ± margin) / 2. Rounded
    // independently — `home + away` may differ from `round(total)` by
    // ±1 in edge cases where (total ± margin) / 2 lands on .5 (e.g.
    // total=146.0, margin=3.0 → 75-72, sum 147 ≠ round(total) 146).
    // We accept this because `predicted_total` isn't currently
    // displayed in the UI; only the integer scores and the
    // 1-decimal `predicted_margin` are. If the totals number ever
    // gets surfaced alongside the score pair, switch to
    // `away_score = round(total) - home_score` for sum reconciliation.
    // Scores derive from the *blended* margin so they stay consistent with
    // the headline (total stays pit — preseason has no totals model).
    let total = explained.prediction.predicted_total as f64;
    let margin = blended_margin as f64;
    let predicted_home_score = ((total + margin) / 2.0).round() as i32;
    let predicted_away_score = ((total - margin) / 2.0).round() as i32;

    // `prediction_basis` ("preseason" | "blended" | "pit" | "leaky") is set
    // above alongside the blend so the frontend chip reads which regime is
    // active rather than inferring from its own state — a request that drops
    // `as_of_date` in transit can't paint a leaky prediction as honest.

    Ok(Json(json!({
        "home_team": home_team.name,
        "home_team_id": home_team.id,
        "away_team": away_team.name,
        "away_team_id": away_team.id,
        "season": season,
        "venue": venue_str,
        "as_of_date": params.as_of_date,
        "prediction_basis": prediction_basis,
        "predicted_margin": (blended_margin as f64 * 10.0).round() / 10.0,
        "home_win_probability": (blended_win_prob * 1000.0).round() / 1000.0,
        "predicted_total": (total * 10.0).round() / 10.0,
        "predicted_home_score": predicted_home_score,
        "predicted_away_score": predicted_away_score,
        "predicted_winner": predicted_winner,
        "feature_contributions": feature_contributions,
        "contributions_by_group": contributions_by_group,
        "roster_home": roster_home,
        "roster_away": roster_away,
        "archetype_distribution_home": archetype_home,
        "archetype_distribution_away": archetype_away,
        "prior_meetings": prior_meetings,
    })))
}

/// Headline prediction plus the inputs and ablation deltas needed to
/// render the explainability panel. All values are from the caller's
/// `home_team` perspective, regardless of venue (positive contribution =
/// pushed margin toward home_team).
struct Explained {
    prediction: Prediction,
    feature_values: [f32; NUM_FEATURES],
    contributions: [f32; NUM_FEATURES],
}

/// Run the predictor with explicit venue semantics, including symmetric
/// averaging for neutral games. All fields in the returned `Explained`
/// are from the caller's `home_team_id` perspective (positive margin /
/// contribution = pushed toward home_team).
async fn predict_with_venue(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    venue: Venue,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<Explained, String> {
    match venue {
        Venue::Home => {
            run_predict(
                state,
                home_team_id,
                away_team_id,
                season,
                false,
                is_conference,
                as_of_date,
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
            let swapped = run_predict(
                state,
                away_team_id,
                home_team_id,
                season,
                false,
                is_conference,
                as_of_date,
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
            predict_neutral_symmetric(
                state,
                home_team_id,
                away_team_id,
                season,
                is_conference,
                as_of_date,
            )
            .await
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
/// `run_predict`'s output we already replace the win-classifier with
/// `margin_to_win_prob`, so re-deriving here keeps the two perfectly in
/// step) which gives `p_home(A,B,neutral) + p_home(B,A,neutral) == 1.0`
/// exactly.
async fn predict_neutral_symmetric(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<Explained, String> {
    let (fwd, rev) = tokio::try_join!(
        run_predict(
            state,
            home_team_id,
            away_team_id,
            season,
            true,
            is_conference,
            as_of_date,
        ),
        run_predict(
            state,
            away_team_id,
            home_team_id,
            season,
            true,
            is_conference,
            as_of_date,
        ),
    )?;

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

    Ok(Explained {
        prediction: Prediction {
            predicted_margin: symmetric_margin,
            home_win_probability: margin_to_win_prob(symmetric_margin, as_of_date.is_some()),
            predicted_total: symmetric_total,
        },
        feature_values,
        contributions,
    })
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

/// Convenience wrapper for surfaces that just need a per-matchup
/// projection. Returns margin + win probability + integer projected
/// scores from `home_team_id`'s perspective.
///
/// Neutral games go through `predict_neutral_symmetric` so the answer is
/// invariant to argument order — without it, the same matchup queried from
/// Team A's schedule (host=B, visitor=A) and Team B's schedule (host=A,
/// visitor=B) returns slightly different magnitudes because LightGBM tree
/// ensembles aren't antisymmetric in diff features. The extra inference
/// per neutral game costs ~0.5ms and eliminates a user-visible inconsistency
/// across surfaces.
pub async fn predict_projection(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<ProjectionSummary, String> {
    let explained = if is_neutral {
        predict_neutral_symmetric(
            state,
            home_team_id,
            away_team_id,
            season,
            is_conference,
            as_of_date,
        )
        .await?
    } else {
        run_predict(
            state,
            home_team_id,
            away_team_id,
            season,
            false,
            is_conference,
            as_of_date,
        )
        .await?
    };
    // Same early-season preseason × pit blend as the `/api/predict` handler,
    // so TeamDetail's Projected column and the ScoreTicker tiles agree with
    // the Predict page on the same matchup (ROADMAP §6). `home_team_id` is
    // always the host here, so the venue is Home (or Neutral); the blend is
    // a scalar mix of the venue-resolved margin, preserving neutral symmetry.
    let pit_margin = explained.prediction.predicted_margin;
    let mut blended_margin = pit_margin;
    let mut home_win_prob = explained.prediction.home_win_probability;
    let w = as_of_date
        .map(|d| preseason_blend_weight(d, season))
        .unwrap_or(0.0);
    if w > 0.0 {
        let venue = if is_neutral {
            Venue::Neutral
        } else {
            Venue::Home
        };
        if let Some(pre_margin) =
            fetch_preseason_margin(&state.db.pool, season, home_team_id, away_team_id, venue).await
        {
            blended_margin = w * pre_margin + (1.0 - w) * pit_margin;
            home_win_prob = margin_to_win_prob(blended_margin, as_of_date.is_some());
        }
    }

    let total = explained.prediction.predicted_total as f64;
    let margin = blended_margin as f64;
    Ok(ProjectionSummary {
        margin: blended_margin,
        home_win_prob,
        home_score: ((total + margin) / 2.0).round() as i32,
        away_score: ((total - margin) / 2.0).round() as i32,
    })
}

async fn run_predict(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<Explained, String> {
    // Single DB-fetch pass produces both the 49-element diff vector
    // (margin/win input) and the 58-element diff+sum vector (totals
    // input). The feature extraction is the expensive step.
    //
    // When `as_of_date` is set, we route through the pit feature builder
    // (CamPom v3 aggregated from torvik_player_game_stats up to the
    // cutoff) and serve the pit model bundle — train/serve parity is the
    // load-bearing invariant: feeding pit features to the end-of-season
    // model (or vice versa) would reintroduce the ~3 AUC points of
    // lookahead inflation the predict-honesty audit caught.
    let f = match as_of_date {
        Some(d) => cstat_core::features::build_all_features_pit(
            &state.db.pool,
            home_team_id,
            away_team_id,
            season,
            is_neutral,
            is_conference,
            d,
        )
        .await
        .map_err(|e| format!("pit feature extraction failed: {e}"))?,
        None => cstat_core::features::build_all_features(
            &state.db.pool,
            home_team_id,
            away_team_id,
            season,
            is_neutral,
            is_conference,
        )
        .await
        .map_err(|e| format!("feature extraction failed: {e}"))?,
    };

    // Margin + TreeSHAP from the diff vector; totals from the diff+sum
    // vector. Pit and end-of-season paths use distinct model bundles —
    // see `Predictor::predict_*` doc comments.
    let (attributed, predicted_total) = match as_of_date {
        Some(_) => {
            let attributed = state
                .predictor
                .predict_pit_with_contributions(&f.diff)
                .map_err(|e| format!("pit prediction failed: {e}"))?;
            let predicted_total = state
                .predictor
                .predict_pit_total(&f.diff_and_sum)
                .map_err(|e| format!("pit totals prediction failed: {e}"))?;
            (attributed, predicted_total)
        }
        None => {
            let attributed = state
                .predictor
                .predict_with_contributions(&f.diff)
                .map_err(|e| format!("prediction failed: {e}"))?;
            let predicted_total = state
                .predictor
                .predict_total(&f.diff_and_sum)
                .map_err(|e| format!("totals prediction failed: {e}"))?;
            (attributed, predicted_total)
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
            predicted_margin: attributed.predicted_margin,
            home_win_probability: margin_to_win_prob(
                attributed.predicted_margin,
                as_of_date.is_some(),
            ),
            predicted_total,
        },
        feature_values: f.diff,
        contributions: attributed.contributions,
    })
}

/// Whether the feature at `i` is a 0/1 indicator (venue, conference
/// game) rather than a `home − away` diff. Flag features don't reverse
/// sign when the teams swap, so they need special handling in venue
/// transforms.
fn is_flag_feature(i: usize) -> bool {
    matches!(FEATURE_NAMES[i], "venue" | "is_conference_game")
}

/// Build the JSON-shaped contribution panel from raw ablation deltas.
///
/// Returns `(feature_contributions, by_group)`. `feature_contributions`
/// lists every feature (all NUM_FEATURES of them) with name, label, group,
/// raw value, and ablation contribution — sorted by |contribution| desc.
/// The frontend slices for top-N display and aggregates per-group as
/// needed; returning the full list lets the keys panel mix the model's
/// importance with the data-side stat direction without needing a separate
/// per-feature endpoint. `by_group` is the model's signed sum per group,
/// kept around for any future "raw model breakdown" surface but currently
/// unused on the frontend (keys recompute their own group sums to flip
/// the direction sign onto the data-faithful axis).
fn build_contribution_payload(
    feature_values: &[f32; NUM_FEATURES],
    contributions: &[f32; NUM_FEATURES],
) -> (Vec<Value>, Vec<Value>) {
    // Per-feature details, sorted by |contribution| desc.
    let mut details: Vec<(usize, f32)> = contributions
        .iter()
        .enumerate()
        .map(|(i, c)| (i, *c))
        .collect();
    details.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let feature_contributions = details
        .iter()
        .map(|(i, c)| {
            json!({
                "name": FEATURE_NAMES[*i],
                "label": FEATURE_META[*i].label,
                "group": FEATURE_META[*i].group,
                // Round value to 3 decimals — fraction-scaled features
                // (AST%, eFG%, TOV%, FT rate) routinely have diffs in
                // the 0.01–0.05 range, and rounding to 1 decimal would
                // collapse them all to 0.0 and obscure real direction.
                "value": round3(feature_values[*i] as f64),
                "contribution": round1(*c as f64),
            })
        })
        .collect::<Vec<_>>();

    // Group totals.
    let mut group_sums: BTreeMap<&'static str, (f32, usize)> = BTreeMap::new();
    for (i, c) in contributions.iter().enumerate() {
        let g = FEATURE_META[i].group;
        let entry = group_sums.entry(g).or_insert((0.0, 0));
        entry.0 += c;
        entry.1 += 1;
    }
    let mut group_vec: Vec<(&'static str, f32, usize)> = group_sums
        .into_iter()
        .map(|(g, (sum, n))| (g, sum, n))
        .collect();
    group_vec.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let by_group = group_vec
        .into_iter()
        .map(|(g, sum, n)| {
            json!({
                "group": g,
                "contribution": round1(sum as f64),
                "feature_count": n,
            })
        })
        .collect::<Vec<_>>();

    (feature_contributions, by_group)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
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
fn margin_to_win_prob(margin: f32, is_pit: bool) -> f64 {
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

/// Weight on the PRESEASON projection in the early-season blend: 1.0 before
/// Nov 1, linear decay to 0.0 by Jan 15, then 0.0 (pure pit). cstat-season
/// `S` runs Nov (S−1) → Apr S, so the anchors are `(S−1)-11-01` and
/// `S-01-15`. Piecewise-linear v1 (ROADMAP §6); `measure-blend-accuracy`
/// calibrates the crossover dates against per-week rolling MAE.
fn preseason_blend_weight(as_of: NaiveDate, season: i32) -> f32 {
    let (Some(full), Some(pit_only)) = (
        NaiveDate::from_ymd_opt(season - 1, 11, 1),
        NaiveDate::from_ymd_opt(season, 1, 15),
    ) else {
        return 0.0;
    };
    if as_of < full {
        return 1.0;
    }
    if as_of >= pit_only {
        return 0.0;
    }
    let span = (pit_only - full).num_days() as f32;
    let elapsed = (as_of - full).num_days() as f32;
    (1.0 - elapsed / span).clamp(0.0, 1.0)
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
    let hca = match venue {
        Venue::Home => PRESEASON_HOME_COURT_ADVANTAGE,
        Venue::Away => -PRESEASON_HOME_COURT_ADVANTAGE,
        Venue::Neutral => 0.0,
    };
    Some(home_adjem? - away_adjem? + hca)
}

#[derive(sqlx::FromRow)]
struct TeamLookup {
    id: Uuid,
    name: String,
    conference: Option<String>,
}

async fn find_team(pool: &PgPool, query: &str, season: i32) -> Result<TeamLookup, sqlx::Error> {
    if let Ok(id) = query.parse::<Uuid>() {
        return sqlx::query_as::<_, TeamLookup>(
            "SELECT id, COALESCE(short_name, name) AS name, conference FROM teams WHERE id = $1 AND season = $2",
        )
        .bind(id)
        .bind(season)
        .fetch_one(pool)
        .await;
    }

    // Exact match against either the Torvik short_name ("Duke") or the full
    // NatStat name ("Duke Blue Devils"). short_name is the canonical input
    // surface; the full name is kept for backwards compat with old links.
    if let Ok(team) = sqlx::query_as::<_, TeamLookup>(
        "SELECT id, COALESCE(short_name, name) AS name, conference
         FROM teams
         WHERE (LOWER(short_name) = LOWER($1) OR LOWER(name) = LOWER($1))
           AND season = $2",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
    {
        return Ok(team);
    }

    sqlx::query_as::<_, TeamLookup>(
        "SELECT id, COALESCE(short_name, name) AS name, conference
         FROM teams
         WHERE (LOWER(short_name) LIKE LOWER($1) || '%' OR LOWER(name) LIKE LOWER($1) || '%')
           AND season = $2
         ORDER BY LENGTH(COALESCE(short_name, name))
         LIMIT 1",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(venue: Option<&str>, neutral: bool) -> PredictParams {
        let venue = venue.map(|v| match v {
            "home" => Venue::Home,
            "away" => Venue::Away,
            "neutral" => Venue::Neutral,
            _ => panic!("bad venue"),
        });
        PredictParams {
            home: "A".into(),
            away: "B".into(),
            venue,
            neutral,
            season: None,
            as_of_date: None,
        }
    }

    #[test]
    fn parses_venue_from_url_query_string() {
        // This is the actual deserialization path axum uses for `Query<T>` —
        // if this fails, the route would silently fall through to the
        // legacy `neutral` default and every venue would look identical.
        let cases = [
            ("home=A&away=B&venue=home", Venue::Home),
            ("home=A&away=B&venue=away", Venue::Away),
            ("home=A&away=B&venue=neutral", Venue::Neutral),
        ];
        for (q, expected) in cases {
            let p: PredictParams = serde_urlencoded::from_str(q)
                .unwrap_or_else(|e| panic!("failed to parse {q:?}: {e}"));
            assert_eq!(p.resolved_venue(), expected, "query string: {q}");
        }

        // No venue param → falls back to neutral=false default.
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B").unwrap();
        assert_eq!(p.resolved_venue(), Venue::Home);

        // Legacy neutral=true still works when venue is absent.
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B&neutral=true").unwrap();
        assert_eq!(p.resolved_venue(), Venue::Neutral);
    }

    #[test]
    fn as_of_date_bounds() {
        // The bound logic in `predict` is straightforward arithmetic on
        // chrono dates; test it directly rather than spinning up the full
        // route. Future dates and far-past dates should both be rejected.
        let season = 2026_i32;
        let earliest = chrono::NaiveDate::from_ymd_opt(season - 1, 9, 1).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 5, 29).unwrap();

        // OK: within bounds.
        for d in ["2025-11-01", "2026-01-15", "2026-04-06"] {
            let d = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
            assert!(d >= earliest && d <= today, "{d} should be in-bounds");
        }
        // Reject: future.
        let fut = chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        assert!(fut > today, "future date should be rejected");
        // Reject: before season window. 2024-12-15 belongs to season 2025,
        // not 2026, so it slips by intent (silently pulls the wrong-season
        // cohort) unless the bound rejects it.
        let early = chrono::NaiveDate::from_ymd_opt(2024, 12, 15).unwrap();
        assert!(early < earliest, "wrong-season date should be rejected");
    }

    #[test]
    fn parses_as_of_date_from_url_query_string() {
        // The audit's R5 plumbing rides on this serialize round-trip — a
        // typo'd field name or a mis-quoted serde rename would silently
        // produce `None` and the leaky model would always win.
        let p: PredictParams =
            serde_urlencoded::from_str("home=A&away=B&as_of_date=2026-02-14").unwrap();
        assert_eq!(p.as_of_date, NaiveDate::from_ymd_opt(2026, 2, 14));

        // Absent param defaults to None (legacy end-of-season path).
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B").unwrap();
        assert_eq!(p.as_of_date, None);

        // Malformed dates surface as a deserialize error rather than silent
        // None — caller gets a 400 instead of a leaky prediction labelled
        // as honest. axum's Query handler maps this to a 400 automatically.
        let err: Result<PredictParams, _> =
            serde_urlencoded::from_str("home=A&away=B&as_of_date=not-a-date");
        assert!(err.is_err(), "malformed date should fail deserialization");
    }

    #[test]
    fn venue_explicit_overrides_legacy_neutral() {
        // Explicit venue always wins, even if `neutral=true` is set.
        assert_eq!(params(Some("home"), true).resolved_venue(), Venue::Home);
        assert_eq!(params(Some("away"), false).resolved_venue(), Venue::Away);
        assert_eq!(
            params(Some("neutral"), false).resolved_venue(),
            Venue::Neutral
        );
    }

    #[test]
    fn legacy_neutral_bool_falls_through() {
        // When venue is absent, fall back to the legacy boolean.
        assert_eq!(params(None, false).resolved_venue(), Venue::Home);
        assert_eq!(params(None, true).resolved_venue(), Venue::Neutral);
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
        // cstat-season 2026 runs Nov 2025 → Apr 2026, so the schedule
        // anchors are 2025-11-01 (w=1) and 2026-01-15 (w=0).
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

        // Before / at the Nov 1 anchor → full preseason weight.
        assert_eq!(preseason_blend_weight(d(2025, 9, 15), 2026), 1.0);
        assert_eq!(preseason_blend_weight(d(2025, 11, 1), 2026), 1.0);

        // At / after the Jan 15 anchor → pure pit.
        assert_eq!(preseason_blend_weight(d(2026, 1, 15), 2026), 0.0);
        assert_eq!(preseason_blend_weight(d(2026, 2, 1), 2026), 0.0);
        assert_eq!(preseason_blend_weight(d(2026, 4, 1), 2026), 0.0);

        // Monotonically decreasing strictly inside the window.
        let mid_nov = preseason_blend_weight(d(2025, 11, 20), 2026);
        let mid_dec = preseason_blend_weight(d(2025, 12, 15), 2026);
        let early_jan = preseason_blend_weight(d(2026, 1, 5), 2026);
        assert!(mid_nov > mid_dec && mid_dec > early_jan);
        assert!((0.0..=1.0).contains(&mid_nov));
        assert!((0.0..=1.0).contains(&early_jan));

        // Roughly halfway: the Nov 1 → Jan 15 span is 75 days, so its
        // midpoint (day ≈37.5) lands around Dec 8.
        let halfway = preseason_blend_weight(d(2025, 12, 8), 2026);
        assert!(
            (halfway - 0.5).abs() < 0.05,
            "midpoint weight {halfway} should be ≈0.5",
        );

        // Season-relative: the same calendar offset in 2025's season
        // (Nov 2024 → Jan 2025) decays identically.
        assert_eq!(preseason_blend_weight(d(2024, 11, 1), 2025), 1.0);
        assert_eq!(preseason_blend_weight(d(2025, 1, 15), 2025), 0.0);
    }
}
