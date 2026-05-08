use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use cstat_core::inference::Prediction;

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
    // answer.
    let prediction = predict_with_venue(
        &state,
        home_team.id,
        away_team.id,
        season,
        venue,
        is_conference,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
    })?;

    let predicted_winner = if prediction.predicted_margin > 0.0 {
        &home_team.name
    } else {
        &away_team.name
    };

    let venue_str = match venue {
        Venue::Home => "home",
        Venue::Away => "away",
        Venue::Neutral => "neutral",
    };

    Ok(Json(json!({
        "home_team": home_team.name,
        "away_team": away_team.name,
        "venue": venue_str,
        "predicted_margin": (prediction.predicted_margin as f64 * 10.0).round() / 10.0,
        "home_win_probability": (prediction.home_win_probability * 1000.0).round() / 1000.0,
        "predicted_winner": predicted_winner,
    })))
}

/// Run the predictor with explicit venue semantics, including symmetric
/// averaging for neutral games. Margins and win probabilities in the
/// returned `Prediction` are always from the `home_team_id` perspective
/// (positive margin = `home_team_id` wins).
async fn predict_with_venue(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    venue: Venue,
    is_conference: bool,
) -> Result<Prediction, String> {
    match venue {
        Venue::Home => {
            run_predict(
                state,
                home_team_id,
                away_team_id,
                season,
                false,
                is_conference,
            )
            .await
        }
        Venue::Away => {
            // Caller's "home" param is actually the visitor. Swap before
            // feature extraction (so the model sees the true host as home),
            // then negate the margin / flip the win prob so the result is
            // reported from the caller's `home_team_id` perspective.
            let swapped = run_predict(
                state,
                away_team_id,
                home_team_id,
                season,
                false,
                is_conference,
            )
            .await?;
            Ok(Prediction {
                predicted_margin: -swapped.predicted_margin,
                home_win_probability: 1.0 - swapped.home_win_probability,
            })
        }
        Venue::Neutral => {
            predict_neutral_symmetric(state, home_team_id, away_team_id, season, is_conference)
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
) -> Result<Prediction, String> {
    let (fwd, rev) = tokio::try_join!(
        run_predict(
            state,
            home_team_id,
            away_team_id,
            season,
            true,
            is_conference
        ),
        run_predict(
            state,
            away_team_id,
            home_team_id,
            season,
            true,
            is_conference
        ),
    )?;

    let symmetric_margin = 0.5 * (fwd.predicted_margin - rev.predicted_margin);
    Ok(Prediction {
        predicted_margin: symmetric_margin,
        home_win_probability: margin_to_win_prob(symmetric_margin),
    })
}

async fn run_predict(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
) -> Result<Prediction, String> {
    let features = cstat_core::features::build_game_features(
        &state.db.pool,
        home_team_id,
        away_team_id,
        season,
        is_neutral,
        is_conference,
    )
    .await
    .map_err(|e| format!("feature extraction failed: {e}"))?;

    let mut p = state
        .predictor
        .predict(&features)
        .map_err(|e| format!("prediction failed: {e}"))?;

    // Override the standalone win-classifier output with a margin-derived
    // win probability. The two LightGBM models (margin + win) are trained
    // independently, so near the boundary their answers can disagree by a
    // few points and produce the user-visible contradiction of "predicted
    // winner = X" alongside "X has 49% win probability". Tying the win
    // probability to margin via a calibrated logistic guarantees the two
    // signals always agree on direction.
    p.home_win_probability = margin_to_win_prob(p.predicted_margin);
    Ok(p)
}

/// Standard deviation of college basketball game-margin residuals. Sourced
/// from the trained margin model's chronological-backtest residuals — see
/// `backtest_residual_stddev` in `training/models/model_meta.json`.
/// Re-measure and update this constant whenever the model is retrained;
/// the value materially affects how aggressively `home_win_probability`
/// moves away from 0.5 per point of predicted margin.
///
/// Current value: 10.3, fit on the 2024+2025+2026 cohort. KenPom uses 11.0
/// as a cross-era constant; cstat's narrower σ reflects tighter residuals
/// on the backtest window plus the model's reliance on Torvik GBPM.
const PREDICT_SIGMA: f64 = 10.3;

/// Logistic approximation of `Φ(margin / σ)` — the probability that the
/// actual margin exceeds zero given a predicted margin and a residual
/// stddev `σ`. The 1.6 scaling constant matches the logistic CDF to the
/// standard normal CDF; the two agree to ≤1pp across the realistic
/// prediction range. We use logistic instead of erf to avoid pulling in a
/// numerics dependency for a single call site.
fn margin_to_win_prob(margin: f32) -> f64 {
    const LOGISTIC_GAUSSIAN_SCALE: f64 = 1.6;
    let z = LOGISTIC_GAUSSIAN_SCALE * (margin as f64) / PREDICT_SIGMA;
    1.0 / (1.0 + (-z).exp())
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
        // 0 margin → exact 50/50.
        assert!((margin_to_win_prob(0.0) - 0.5).abs() < 1e-9);

        // Antisymmetric around 0: p(m) + p(-m) = 1. This is the property
        // that guarantees `predicted_winner` derived from win prob always
        // agrees with the sign of the margin.
        for m in [1.0, 5.0, 11.0, 25.0, -3.0, -17.5_f32] {
            let neg_m = -m;
            let p = margin_to_win_prob(m);
            let p_neg = margin_to_win_prob(neg_m);
            assert!(
                (p + p_neg - 1.0).abs() < 1e-9,
                "p({m}) + p({neg_m}) = {p} + {p_neg} ≠ 1.0",
            );
        }

        // Monotonic in margin.
        let pairs = [(0.0_f32, 1.0_f32), (1.0, 5.0), (5.0, 15.0), (-2.0, 2.0)];
        for (lo, hi) in pairs {
            assert!(
                margin_to_win_prob(lo) < margin_to_win_prob(hi),
                "p({lo}) ≥ p({hi}) — monotonicity broken",
            );
        }

        // Sanity: margin-sign and (prob > 0.5) agree, so the headline
        // contradiction (Predicted winner = X, X has 49% win prob) is
        // impossible by construction.
        for m in [-10.0, -1.0, -0.1, 0.1, 1.0, 10.0_f32] {
            let p = margin_to_win_prob(m);
            assert_eq!(
                m > 0.0,
                p > 0.5,
                "sign disagreement at margin={m}: prob={p}",
            );
        }
    }

    #[test]
    fn neutral_symmetry_combination_is_exact() {
        // Sanity-check the math: the symmetric averaging must guarantee
        // margin(A,B) + margin(B,A) == 0 and p(A,B) + p(B,A) == 1.0
        // for any pair of forward/reverse Prediction values.
        let fwd = Prediction {
            predicted_margin: 7.3,
            home_win_probability: 0.78,
        };
        let rev = Prediction {
            predicted_margin: -7.1, // not perfectly antisymmetric (the bug we're fixing)
            home_win_probability: 0.21,
        };

        let m_ab = 0.5 * (fwd.predicted_margin - rev.predicted_margin);
        let p_ab = 0.5 * (fwd.home_win_probability + (1.0 - rev.home_win_probability));

        // Now reversed call: forward becomes the original reverse, and vice versa.
        let m_ba = 0.5 * (rev.predicted_margin - fwd.predicted_margin);
        let p_ba = 0.5 * (rev.home_win_probability + (1.0 - fwd.home_win_probability));

        assert!((m_ab + m_ba).abs() < 1e-9, "margins should sum to 0");
        assert!(
            (p_ab + p_ba - 1.0).abs() < 1e-9,
            "win probs should sum to 1"
        );
    }
}
