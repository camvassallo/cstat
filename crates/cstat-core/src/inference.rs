use crate::treeshap::{LgbModel, tree_shap};
use ort::session::Session;
use std::path::Path;
use std::sync::Mutex;

/// Number of input features expected by the margin/win ONNX models.
///
/// The totals model takes `TOTAL_NUM_FEATURES` (49 diffs + 9 level-sensitive
/// sums) — diff-only features can't predict totals because they throw away
/// absolute level (`diff_tempo=0` is ambiguous between two slow teams and
/// two fast teams). See ROADMAP "Predict follow-up — totals / tempo model"
/// and `training/features.py` for methodology.
pub const NUM_FEATURES: usize = 49;

/// Number of input features expected by the totals ONNX model.
///
/// Layout: indices 0..NUM_FEATURES are the same diff features the
/// margin/win models consume (so the same fetched team data feeds both
/// paths); indices NUM_FEATURES..TOTAL_NUM_FEATURES are 9 sum_* features
/// computed as `home + away` on the unflipped raw columns.
pub const TOTAL_NUM_FEATURES: usize = NUM_FEATURES + 9;

/// Per-feature display label and group for the explainability UI. Stored
/// in the same index order as `FEATURE_NAMES` so a contribution at
/// `contributions[i]` directly addresses `FEATURE_META[i]`. Tweak labels
/// here when adjusting how the contribution panel reads — never reorder
/// (the indices are wire-locked to the trained ONNX).
pub struct FeatureMeta {
    pub label: &'static str,
    pub group: &'static str,
}

pub const FEATURE_META: [FeatureMeta; NUM_FEATURES] = [
    FeatureMeta {
        label: "Home court",
        group: "Context",
    },
    FeatureMeta {
        label: "Conference game",
        group: "Context",
    },
    FeatureMeta {
        label: "Win pct",
        group: "Context",
    },
    FeatureMeta {
        label: "Adj offense",
        group: "Adjusted efficiency",
    },
    FeatureMeta {
        label: "Adj defense",
        group: "Adjusted efficiency",
    },
    FeatureMeta {
        label: "Adj efficiency margin",
        group: "Adjusted efficiency",
    },
    FeatureMeta {
        label: "eFG%",
        group: "Four factors (offense)",
    },
    FeatureMeta {
        label: "Turnover%",
        group: "Four factors (offense)",
    },
    FeatureMeta {
        label: "Off rebound%",
        group: "Four factors (offense)",
    },
    FeatureMeta {
        label: "FT rate",
        group: "Four factors (offense)",
    },
    FeatureMeta {
        label: "Opp eFG%",
        group: "Four factors (defense)",
    },
    FeatureMeta {
        label: "Opp turnover%",
        group: "Four factors (defense)",
    },
    FeatureMeta {
        label: "Def rebound%",
        group: "Four factors (defense)",
    },
    FeatureMeta {
        label: "Opp FT rate",
        group: "Four factors (defense)",
    },
    FeatureMeta {
        label: "Tempo",
        group: "Pace",
    },
    FeatureMeta {
        label: "SOS",
        group: "Strength of schedule",
    },
    FeatureMeta {
        label: "ELO",
        group: "Power ratings",
    },
    FeatureMeta {
        label: "Point diff",
        group: "Power ratings",
    },
    FeatureMeta {
        label: "Pythag win%",
        group: "Power ratings",
    },
    FeatureMeta {
        label: "Road win%",
        group: "Power ratings",
    },
    FeatureMeta {
        label: "Roster size",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster PPG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster RPG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster APG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster SPG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster BPG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster TOPG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster TS%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster eFG%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster usage",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Player SOS",
        group: "Strength of schedule",
    },
    FeatureMeta {
        label: "Roster ORTG",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster AST%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster TOV%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster STL%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster BLK%",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Roster GBPM",
        group: "Roster impact",
    },
    FeatureMeta {
        label: "Roster OGBPM",
        group: "Roster impact",
    },
    FeatureMeta {
        label: "Roster DGBPM",
        group: "Roster impact",
    },
    FeatureMeta {
        label: "Star PPG",
        group: "Star player",
    },
    FeatureMeta {
        label: "Star GBPM",
        group: "Star player",
    },
    FeatureMeta {
        label: "Star OGBPM",
        group: "Star player",
    },
    FeatureMeta {
        label: "Star DGBPM",
        group: "Star player",
    },
    FeatureMeta {
        label: "Star ORTG",
        group: "Star player",
    },
    FeatureMeta {
        label: "Minutes spread",
        group: "Roster aggregate",
    },
    FeatureMeta {
        label: "Recent game score",
        group: "Recent form",
    },
    FeatureMeta {
        label: "Recent TS%",
        group: "Recent form",
    },
    FeatureMeta {
        label: "PPG trend",
        group: "Recent form",
    },
    FeatureMeta {
        label: "GS trend",
        group: "Recent form",
    },
];

/// Feature names in the exact order expected by the ONNX models.
pub const FEATURE_NAMES: [&str; NUM_FEATURES] = [
    "venue",
    "is_conference_game",
    "diff_win_pct",
    "diff_adj_offense",
    "diff_adj_defense",
    "diff_adj_efficiency_margin",
    "diff_effective_fg_pct",
    "diff_turnover_pct",
    "diff_off_rebound_pct",
    "diff_ft_rate",
    "diff_opp_effective_fg_pct",
    "diff_opp_turnover_pct",
    "diff_def_rebound_pct",
    "diff_opp_ft_rate",
    "diff_adj_tempo",
    "diff_sos",
    "diff_elo",
    "diff_point_diff",
    "diff_pythag_win_pct",
    "diff_road_win_pct",
    "diff_roster_size",
    "diff_w_ppg",
    "diff_w_rpg",
    "diff_w_apg",
    "diff_w_spg",
    "diff_w_bpg",
    "diff_w_topg",
    "diff_w_ts_pct",
    "diff_w_efg_pct",
    "diff_w_usage",
    "diff_w_player_sos",
    "diff_w_ortg",
    "diff_w_ast_pct",
    "diff_w_tov_pct",
    "diff_w_stl_pct",
    "diff_w_blk_pct",
    "diff_w_gbpm",
    "diff_w_ogbpm",
    "diff_w_dgbpm",
    "diff_star_ppg",
    "diff_star_gbpm",
    "diff_star_ogbpm",
    "diff_star_dgbpm",
    "diff_star_ortg",
    "diff_minutes_stddev",
    "diff_w_rolling_gs",
    "diff_w_rolling_ts",
    "diff_w_ppg_trend",
    "diff_w_gs_trend",
];

/// Feature names in the exact order expected by the totals ONNX model.
/// Indices 0..NUM_FEATURES match `FEATURE_NAMES` byte-for-byte; indices
/// NUM_FEATURES.. carry the 9 sum_* level-sensitive companions. Order
/// here is wire-locked to `model_meta.json::total_features` — never
/// reorder without retraining.
pub const TOTAL_FEATURE_NAMES: [&str; TOTAL_NUM_FEATURES] = [
    "venue",
    "is_conference_game",
    "diff_win_pct",
    "diff_adj_offense",
    "diff_adj_defense",
    "diff_adj_efficiency_margin",
    "diff_effective_fg_pct",
    "diff_turnover_pct",
    "diff_off_rebound_pct",
    "diff_ft_rate",
    "diff_opp_effective_fg_pct",
    "diff_opp_turnover_pct",
    "diff_def_rebound_pct",
    "diff_opp_ft_rate",
    "diff_adj_tempo",
    "diff_sos",
    "diff_elo",
    "diff_point_diff",
    "diff_pythag_win_pct",
    "diff_road_win_pct",
    "diff_roster_size",
    "diff_w_ppg",
    "diff_w_rpg",
    "diff_w_apg",
    "diff_w_spg",
    "diff_w_bpg",
    "diff_w_topg",
    "diff_w_ts_pct",
    "diff_w_efg_pct",
    "diff_w_usage",
    "diff_w_player_sos",
    "diff_w_ortg",
    "diff_w_ast_pct",
    "diff_w_tov_pct",
    "diff_w_stl_pct",
    "diff_w_blk_pct",
    "diff_w_gbpm",
    "diff_w_ogbpm",
    "diff_w_dgbpm",
    "diff_star_ppg",
    "diff_star_gbpm",
    "diff_star_ogbpm",
    "diff_star_dgbpm",
    "diff_star_ortg",
    "diff_minutes_stddev",
    "diff_w_rolling_gs",
    "diff_w_rolling_ts",
    "diff_w_ppg_trend",
    "diff_w_gs_trend",
    "sum_adj_tempo",
    "sum_adj_offense",
    "sum_adj_defense",
    "sum_effective_fg_pct",
    "sum_opp_effective_fg_pct",
    "sum_w_ppg",
    "sum_w_ortg",
    "sum_off_rebound_pct",
    "sum_def_rebound_pct",
];

/// Prediction output from the ONNX models.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Prediction {
    /// Predicted point margin (positive = home team favored).
    pub predicted_margin: f32,
    /// Probability that the home team wins (0.0–1.0).
    pub home_win_probability: f64,
    /// Predicted total points (home + away). The API derives the two
    /// team scores from `(total ± margin) / 2`.
    pub predicted_total: f32,
}

/// Prediction plus per-feature TreeSHAP contributions.
///
/// `contributions[i]` is the SHAP value for feature `i`: how much that
/// feature pushed the prediction relative to the model's expected output
/// (the cover-weighted mean leaf value across the ensemble). Positive =
/// pushed toward home, negative = toward away. SHAP values are additive:
/// `base_value + Σ contributions = predicted_margin` to floating-point
/// precision. Note that SHAP signs are the *model's* answer about
/// direction — they can legitimately disagree with the data on
/// non-monotonic features (the model has interactions). The frontend
/// keys panel uses `|contribution|` for importance and a separate
/// data-direction lookup (`homeAdvantageSign`) for the leader name to
/// keep the user-facing narrative stats-faithful.
#[derive(Debug, Clone)]
pub struct PredictionWithContributions {
    pub predicted_margin: f32,
    pub contributions: [f32; NUM_FEATURES],
}

/// Failure mode loading the predictor (ONNX session error or LightGBM
/// `.lgb` parse error). Wraps the underlying error so callers can still
/// `Display` it through `anyhow!` / `?`.
#[derive(Debug)]
pub enum LoadError {
    Ort(ort::Error),
    Lgb(crate::treeshap::LgbParseError),
    /// `.lgb` file's feature count disagrees with the compiled `NUM_FEATURES`.
    FeatureCountMismatch {
        expected: usize,
        actual: usize,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Ort(e) => write!(f, "ONNX load: {e}"),
            LoadError::Lgb(e) => write!(f, "LightGBM .lgb parse: {e}"),
            LoadError::FeatureCountMismatch { expected, actual } => write!(
                f,
                "margin .lgb feature count {actual} ≠ compiled NUM_FEATURES {expected}",
            ),
        }
    }
}

impl std::error::Error for LoadError {}

// Generic over `ort::Error<R>`'s phantom marker so any builder-stage
// failure (`Error<SessionBuilder>`, `Error<()>`, etc.) propagates through
// `?` without per-stage `.map_err`. ort's own `From<Error<X>> for Error<()>`
// chains the conversion.
impl<R> From<ort::Error<R>> for LoadError
where
    ort::Error<()>: From<ort::Error<R>>,
{
    fn from(e: ort::Error<R>) -> Self {
        LoadError::Ort(e.into())
    }
}

impl From<crate::treeshap::LgbParseError> for LoadError {
    fn from(e: crate::treeshap::LgbParseError) -> Self {
        LoadError::Lgb(e)
    }
}

/// Holds loaded models for margin, win, and total prediction. ONNX
/// powers the fast prediction path; the parsed LightGBM `.lgb` mirror of
/// the margin model powers TreeSHAP attribution for the Predict page's
/// keys panel. Totals don't get attribution — the keys panel narrative
/// is margin-only.
pub struct Predictor {
    margin_session: Mutex<Session>,
    win_session: Mutex<Session>,
    total_session: Mutex<Session>,
    margin_lgb: LgbModel,
}

impl Predictor {
    /// Load models from the given directory.
    ///
    /// Expects `margin_model.onnx`, `win_model.onnx`,
    /// `total_model.onnx`, and `margin_model.lgb` in `model_dir`. The
    /// `.lgb` is the LightGBM text dump that backs TreeSHAP attribution.
    pub fn load(model_dir: &Path) -> Result<Self, LoadError> {
        let margin_session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_dir.join("margin_model.onnx"))?;

        let win_session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_dir.join("win_model.onnx"))?;

        let total_session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_dir.join("total_model.onnx"))?;

        let margin_lgb = LgbModel::load(&model_dir.join("margin_model.lgb"))?;
        if margin_lgb.num_features != NUM_FEATURES {
            return Err(LoadError::FeatureCountMismatch {
                expected: NUM_FEATURES,
                actual: margin_lgb.num_features,
            });
        }

        Ok(Self {
            margin_session: Mutex::new(margin_session),
            win_session: Mutex::new(win_session),
            total_session: Mutex::new(total_session),
            margin_lgb,
        })
    }

    /// Run all three models and return predictions.
    ///
    /// Margin and win consume the 49-element diff feature vector; total
    /// consumes the 58-element diff+sum vector. The two are produced
    /// together by `features::build_all_features` so the API issues one
    /// DB-fetch pass per matchup.
    pub fn predict(
        &self,
        diff: &[f32; NUM_FEATURES],
        diff_and_sum: &[f32; TOTAL_NUM_FEATURES],
    ) -> Result<Prediction, ort::Error> {
        use ort::value::TensorRef;

        let diff_shape = [1_usize, NUM_FEATURES];
        let total_shape = [1_usize, TOTAL_NUM_FEATURES];

        // Margin model: single float output
        let margin_input = TensorRef::from_array_view((diff_shape, diff.as_slice()))?;
        let mut margin_session = self.margin_session.lock().unwrap();
        let margin_outputs = margin_session.run(ort::inputs![margin_input])?;
        let (_, margin_data) = margin_outputs[0].try_extract_tensor::<f32>()?;
        let predicted_margin = margin_data[0];
        drop(margin_outputs);
        drop(margin_session);

        // Win model: outputs [label (int64), probabilities (float32, shape [1, 2])]
        let win_input = TensorRef::from_array_view((diff_shape, diff.as_slice()))?;
        let mut win_session = self.win_session.lock().unwrap();
        let win_outputs = win_session.run(ort::inputs![win_input])?;
        let (_, probs) = win_outputs[1].try_extract_tensor::<f32>()?;
        // Index 1 = probability of class 1 (home win)
        let home_win_probability = if probs.len() >= 2 {
            probs[1] as f64
        } else {
            probs[0] as f64
        };
        drop(win_outputs);
        drop(win_session);

        // Total model: single float output, 58-feature input
        let total_input = TensorRef::from_array_view((total_shape, diff_and_sum.as_slice()))?;
        let mut total_session = self.total_session.lock().unwrap();
        let total_outputs = total_session.run(ort::inputs![total_input])?;
        let (_, total_data) = total_outputs[0].try_extract_tensor::<f32>()?;
        let predicted_total = total_data[0];

        Ok(Prediction {
            predicted_margin,
            home_win_probability,
            predicted_total,
        })
    }

    /// Run only the totals model. Used by the API alongside
    /// `predict_with_contributions` (margin path) so the two model
    /// outputs ride the same DB-fetch round-trip without paying for
    /// margin twice.
    pub fn predict_total(
        &self,
        diff_and_sum: &[f32; TOTAL_NUM_FEATURES],
    ) -> Result<f32, ort::Error> {
        use ort::value::TensorRef;
        let shape = [1_usize, TOTAL_NUM_FEATURES];
        let input = TensorRef::from_array_view((shape, diff_and_sum.as_slice()))?;
        let mut session = self.total_session.lock().unwrap();
        let outputs = session.run(ort::inputs![input])?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(data[0])
    }

    /// Run the margin model and return TreeSHAP feature attributions.
    ///
    /// Margin comes from the ONNX session (fast, well-tested path); SHAP
    /// values come from the parsed `.lgb` mirror via the canonical
    /// Lundberg/Erion/Lee algorithm. Both are reading the same trained
    /// LightGBM model — they agree to floating-point precision.
    ///
    /// Win probability is intentionally omitted — derive it from the
    /// returned margin via the calibrated logistic in the API layer
    /// (`PREDICT_SIGMA`) so the headline numbers stay self-consistent.
    pub fn predict_with_contributions(
        &self,
        features: &[f32; NUM_FEATURES],
    ) -> Result<PredictionWithContributions, ort::Error> {
        use ort::value::TensorRef;

        // Margin via ONNX (single-row prediction).
        let shape = [1_usize, NUM_FEATURES];
        let input = TensorRef::from_array_view((shape, features.as_slice()))?;
        let mut session = self.margin_session.lock().unwrap();
        let outputs = session.run(ort::inputs![input])?;
        let (_, preds) = outputs[0].try_extract_tensor::<f32>()?;
        let predicted_margin = preds[0];
        drop(outputs);
        drop(session);

        // SHAP attributions via TreeSHAP on the parsed .lgb. Promote to
        // f64 for the recursion (LightGBM stores thresholds in f64);
        // demote results back to f32 for the return type. Stack-allocate
        // the f64 view so this stays alloc-free on the hot path.
        let mut features_f64 = [0.0_f64; NUM_FEATURES];
        for (i, &v) in features.iter().enumerate() {
            features_f64[i] = v as f64;
        }
        let shap = tree_shap(&self.margin_lgb, &features_f64);

        let mut contributions = [0.0_f32; NUM_FEATURES];
        for (i, v) in shap.iter().enumerate() {
            contributions[i] = *v as f32;
        }

        Ok(PredictionWithContributions {
            predicted_margin,
            contributions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../training/models")
    }

    #[test]
    fn feature_names_match_model_meta() {
        let meta_path = model_dir().join("model_meta.json");
        let content = match std::fs::read_to_string(&meta_path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!(
                    "skipping: model_meta.json not found at {}",
                    meta_path.display()
                );
                return;
            }
        };
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();

        let meta_features: Vec<String> = meta["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert_eq!(meta_features.len(), NUM_FEATURES);
        for (i, (expected, actual)) in meta_features.iter().zip(FEATURE_NAMES.iter()).enumerate() {
            assert_eq!(expected, actual, "feature mismatch at index {i}");
        }
    }

    #[test]
    fn total_feature_names_match_model_meta() {
        let meta_path = model_dir().join("model_meta.json");
        let content = match std::fs::read_to_string(&meta_path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!(
                    "skipping: model_meta.json not found at {}",
                    meta_path.display()
                );
                return;
            }
        };
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();

        let meta_features: Vec<String> = meta["total_features"]
            .as_array()
            .expect("model_meta.json missing total_features — retrain with totals model")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert_eq!(meta_features.len(), TOTAL_NUM_FEATURES);
        for (i, (expected, actual)) in meta_features
            .iter()
            .zip(TOTAL_FEATURE_NAMES.iter())
            .enumerate()
        {
            assert_eq!(expected, actual, "total feature mismatch at index {i}");
        }
    }

    #[test]
    fn load_margin_model_and_predict_zeros() {
        let dir = model_dir();
        let path = dir.join("margin_model.onnx");
        if !path.exists() {
            eprintln!("skipping: ONNX model not found at {}", path.display());
            return;
        }

        let mut session = Session::builder()
            .unwrap()
            .with_intra_threads(1)
            .unwrap()
            .commit_from_file(&path)
            .unwrap();

        let features = [0.0_f32; NUM_FEATURES];
        let shape = [1_usize, NUM_FEATURES];
        let input = ort::value::TensorRef::from_array_view((shape, features.as_slice())).unwrap();
        let outputs = session.run(ort::inputs![input]).unwrap();
        let (_, data) = outputs[0].try_extract_tensor::<f32>().unwrap();
        let margin = data[0];

        eprintln!("margin model zero-feature prediction: {margin}");
        assert!(
            margin.abs() < 20.0,
            "margin {margin} unreasonably large for zero features"
        );
    }

    #[test]
    fn load_models_and_predict_zeros() {
        let dir = model_dir();
        if !dir.join("margin_model.onnx").exists() || !dir.join("margin_model.lgb").exists() || !dir.join("total_model.onnx").exists() {
            eprintln!("skipping: model files not found at {}", dir.display());
            return;
        }

        let predictor = Predictor::load(&dir).expect("failed to load models");
        let features = [0.0_f32; NUM_FEATURES];
        let total_features = [0.0_f32; TOTAL_NUM_FEATURES];
        let pred = predictor
            .predict(&features, &total_features)
            .expect("prediction failed");

        // With all-zero features (neutral matchup), margin should be near zero
        // and win probability near 0.5
        assert!(
            pred.predicted_margin.abs() < 20.0,
            "margin {} is unreasonably large for zero features",
            pred.predicted_margin
        );
        assert!(
            (0.0..=1.0).contains(&pred.home_win_probability),
            "win probability {} out of range",
            pred.home_win_probability
        );
    }

    #[test]
    fn predict_responds_to_feature_direction() {
        let dir = model_dir();
        if !dir.join("margin_model.onnx").exists() || !dir.join("margin_model.lgb").exists() || !dir.join("total_model.onnx").exists() {
            return;
        }

        let predictor = Predictor::load(&dir).unwrap();

        // Look up indices by name so the test stays correct when feature
        // ordering changes.
        let idx = |name: &str| {
            FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap_or_else(|| panic!("feature {name} missing from FEATURE_NAMES"))
        };
        let i_venue = idx("venue");
        let i_eff = idx("diff_adj_efficiency_margin");
        let i_elo = idx("diff_elo");
        let i_gbpm = idx("diff_w_gbpm");

        // Strong home team: positive efficiency margin, high ELO diff,
        // plus a positive Torvik impact diff (now the dominant ML signal).
        let mut home_favored = [0.0_f32; NUM_FEATURES];
        home_favored[i_venue] = 1.0;
        home_favored[i_eff] = 25.0;
        home_favored[i_elo] = 200.0;
        home_favored[i_gbpm] = 5.0;

        // Strong away team: flip the signs.
        let mut away_favored = [0.0_f32; NUM_FEATURES];
        away_favored[i_venue] = 1.0;
        away_favored[i_eff] = -25.0;
        away_favored[i_elo] = -200.0;
        away_favored[i_gbpm] = -5.0;

        // Pad to the totals model's feature count — sums stay 0 since
        // this test only asserts on margin/win behavior, not totals.
        let mut home_favored_total = [0.0_f32; TOTAL_NUM_FEATURES];
        home_favored_total[..NUM_FEATURES].copy_from_slice(&home_favored);
        let mut away_favored_total = [0.0_f32; TOTAL_NUM_FEATURES];
        away_favored_total[..NUM_FEATURES].copy_from_slice(&away_favored);

        let pred_home = predictor
            .predict(&home_favored, &home_favored_total)
            .unwrap();
        let pred_away = predictor
            .predict(&away_favored, &away_favored_total)
            .unwrap();

        assert!(
            pred_home.predicted_margin > pred_away.predicted_margin,
            "home-favored margin ({}) should exceed away-favored ({})",
            pred_home.predicted_margin,
            pred_away.predicted_margin
        );
        assert!(
            pred_home.home_win_probability > pred_away.home_win_probability,
            "home-favored win prob ({}) should exceed away-favored ({})",
            pred_home.home_win_probability,
            pred_away.home_win_probability
        );
    }

    #[test]
    fn feature_meta_aligned_with_feature_names() {
        // Smoke check the parallel arrays. FEATURE_META is indexed by the
        // same offset as FEATURE_NAMES; if anyone reorders FEATURE_NAMES
        // without touching FEATURE_META the explainability UI silently
        // mislabels every contribution. Spot-check the few cases where
        // mislabeling would be most visible (venue, GBPM cluster, star).
        let by_name = |name: &str| -> &FeatureMeta {
            let i = FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
            &FEATURE_META[i]
        };
        assert_eq!(by_name("venue").group, "Context");
        assert_eq!(by_name("diff_w_gbpm").group, "Roster impact");
        assert_eq!(by_name("diff_star_gbpm").group, "Star player");
        assert_eq!(
            by_name("diff_adj_efficiency_margin").group,
            "Adjusted efficiency"
        );
        assert_eq!(by_name("diff_w_rolling_gs").group, "Recent form");
    }

    #[test]
    fn predict_with_contributions_matches_full_predict() {
        let dir = model_dir();
        if !dir.join("margin_model.onnx").exists() || !dir.join("margin_model.lgb").exists() || !dir.join("total_model.onnx").exists() {
            return;
        }

        let predictor = Predictor::load(&dir).unwrap();

        let idx = |name: &str| {
            FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap_or_else(|| panic!("feature {name} missing from FEATURE_NAMES"))
        };

        // Build a non-trivial feature vector so several features contribute.
        let mut features = [0.0_f32; NUM_FEATURES];
        features[idx("venue")] = 1.0;
        features[idx("diff_adj_efficiency_margin")] = 12.0;
        features[idx("diff_elo")] = 80.0;
        features[idx("diff_w_gbpm")] = 3.0;
        features[idx("diff_w_ogbpm")] = 1.5;
        features[idx("diff_w_dgbpm")] = 1.5;

        // Build the totals input by appending zero sums — totals model
        // isn't checked in this test, only margin attribution.
        let mut total_features = [0.0_f32; TOTAL_NUM_FEATURES];
        total_features[..NUM_FEATURES].copy_from_slice(&features);

        let baseline = predictor.predict(&features, &total_features).unwrap();
        let attributed = predictor.predict_with_contributions(&features).unwrap();

        // The TreeSHAP-attributed margin must match the standalone ONNX
        // prediction — both are reading the same trained model.
        assert!(
            (attributed.predicted_margin - baseline.predicted_margin).abs() < 1e-3,
            "attributed margin {} ≠ single-row margin {}",
            attributed.predicted_margin,
            baseline.predicted_margin,
        );

        // SHAP additivity invariant: base + Σ contributions = predicted
        // margin to floating-point precision. (Unlike ablation, SHAP
        // values reconstruct the prediction exactly.)
        let base = predictor.margin_lgb.base_value() as f32;
        let sum_contrib: f32 = attributed.contributions.iter().sum();
        let reconstructed = base + sum_contrib;
        assert!(
            (reconstructed - baseline.predicted_margin).abs() < 1e-2,
            "base ({base}) + Σ shap ({sum_contrib}) = {reconstructed} ≠ predicted {} (Δ {})",
            baseline.predicted_margin,
            reconstructed - baseline.predicted_margin,
        );

        // At least one feature should have a non-trivial SHAP value given
        // the strong inputs (high ELO diff, big efficiency margin, etc).
        let max_abs = attributed
            .contributions
            .iter()
            .map(|c| c.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_abs > 0.1,
            "no feature contributed > 0.1 points; TreeSHAP likely broken (max |c| = {max_abs})",
        );
    }
}
