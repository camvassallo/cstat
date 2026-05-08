use ort::session::Session;
use std::path::Path;
use std::sync::Mutex;

/// Number of input features expected by the ONNX models.
pub const NUM_FEATURES: usize = 49;

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

/// Prediction output from the ONNX models.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Prediction {
    /// Predicted point margin (positive = home team favored).
    pub predicted_margin: f32,
    /// Probability that the home team wins (0.0–1.0).
    pub home_win_probability: f64,
}

/// Prediction plus per-feature ablation contributions.
///
/// `contributions[i]` is `full_pred − pred_with_features[i]_zeroed`. Since
/// every feature in `FEATURE_NAMES` is a diff (home − away) or a
/// 0/1 indicator, "feature zeroed" reads as "teams equal on this
/// dimension", and the contribution reads as "how much did this dimension
/// push the margin off zero". Positive = pushed toward home, negative =
/// toward away. Contributions don't necessarily sum to `predicted_margin`
/// (tree models have feature interactions); they rank-order which inputs
/// drove the prediction.
#[derive(Debug, Clone)]
pub struct PredictionWithContributions {
    pub predicted_margin: f32,
    pub contributions: [f32; NUM_FEATURES],
}

/// Holds loaded ONNX model sessions for margin and win prediction.
pub struct Predictor {
    margin_session: Mutex<Session>,
    win_session: Mutex<Session>,
}

impl Predictor {
    /// Load ONNX models from the given directory.
    ///
    /// Expects `margin_model.onnx` and `win_model.onnx` in `model_dir`.
    pub fn load(model_dir: &Path) -> Result<Self, ort::Error> {
        let margin_session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_dir.join("margin_model.onnx"))?;

        let win_session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_dir.join("win_model.onnx"))?;

        Ok(Self {
            margin_session: Mutex::new(margin_session),
            win_session: Mutex::new(win_session),
        })
    }

    /// Run both models on a feature vector and return predictions.
    pub fn predict(&self, features: &[f32; NUM_FEATURES]) -> Result<Prediction, ort::Error> {
        use ort::value::TensorRef;

        let shape = [1_usize, NUM_FEATURES];

        // Margin model: single float output
        let margin_input = TensorRef::from_array_view((shape, features.as_slice()))?;
        let mut margin_session = self.margin_session.lock().unwrap();
        let margin_outputs = margin_session.run(ort::inputs![margin_input])?;
        let (_, margin_data) = margin_outputs[0].try_extract_tensor::<f32>()?;
        let predicted_margin = margin_data[0];
        drop(margin_outputs);
        drop(margin_session);

        // Win model: outputs [label (int64), probabilities (float32, shape [1, 2])]
        let win_input = TensorRef::from_array_view((shape, features.as_slice()))?;
        let mut win_session = self.win_session.lock().unwrap();
        let win_outputs = win_session.run(ort::inputs![win_input])?;
        let (_, probs) = win_outputs[1].try_extract_tensor::<f32>()?;
        // Index 1 = probability of class 1 (home win)
        let home_win_probability = if probs.len() >= 2 {
            probs[1] as f64
        } else {
            probs[0] as f64
        };

        Ok(Prediction {
            predicted_margin,
            home_win_probability,
        })
    }

    /// Run the margin model with ablation-based feature attribution.
    ///
    /// Builds a single batched `(NUM_FEATURES + 1, NUM_FEATURES)` input
    /// where row 0 is the full feature vector and row `i+1` has feature
    /// `i` set to 0. One ONNX call returns all `NUM_FEATURES + 1`
    /// predictions; the contribution of feature `i` is then
    /// `output[0] − output[i+1]`.
    ///
    /// Win probability is intentionally omitted — derive it from the
    /// returned margin via the calibrated logistic in the API layer
    /// (`PREDICT_SIGMA`) so the headline numbers stay self-consistent.
    pub fn predict_with_contributions(
        &self,
        features: &[f32; NUM_FEATURES],
    ) -> Result<PredictionWithContributions, ort::Error> {
        use ort::value::TensorRef;

        const ROWS: usize = NUM_FEATURES + 1;

        // Build the batched input. Row 0 is the full vector; row i+1 has
        // feature i zeroed. Layout is row-major (axum/ort default).
        let mut batch = vec![0.0_f32; ROWS * NUM_FEATURES];
        for (i, val) in features.iter().enumerate() {
            batch[i] = *val; // row 0
        }
        for i in 0..NUM_FEATURES {
            let row_offset = (i + 1) * NUM_FEATURES;
            for (j, val) in features.iter().enumerate() {
                batch[row_offset + j] = *val;
            }
            batch[row_offset + i] = 0.0; // ablate feature i
        }

        let shape = [ROWS, NUM_FEATURES];
        let input = TensorRef::from_array_view((shape, batch.as_slice()))?;
        let mut session = self.margin_session.lock().unwrap();
        let outputs = session.run(ort::inputs![input])?;
        let (_, preds) = outputs[0].try_extract_tensor::<f32>()?;

        // First output = full prediction, rest = ablated.
        let full = preds[0];
        let mut contributions = [0.0_f32; NUM_FEATURES];
        for i in 0..NUM_FEATURES {
            contributions[i] = full - preds[i + 1];
        }

        Ok(PredictionWithContributions {
            predicted_margin: full,
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
        if !dir.join("margin_model.onnx").exists() {
            eprintln!("skipping: ONNX models not found at {}", dir.display());
            return;
        }

        let predictor = Predictor::load(&dir).expect("failed to load models");
        let features = [0.0_f32; NUM_FEATURES];
        let pred = predictor.predict(&features).expect("prediction failed");

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
        if !dir.join("margin_model.onnx").exists() {
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

        let pred_home = predictor.predict(&home_favored).unwrap();
        let pred_away = predictor.predict(&away_favored).unwrap();

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
        if !dir.join("margin_model.onnx").exists() {
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

        let baseline = predictor.predict(&features).unwrap();
        let attributed = predictor.predict_with_contributions(&features).unwrap();

        // The batched margin must match the single-row margin to floating
        // tolerance — same model, same inputs, same answer.
        assert!(
            (attributed.predicted_margin - baseline.predicted_margin).abs() < 1e-3,
            "batched margin {} ≠ single-row margin {}",
            attributed.predicted_margin,
            baseline.predicted_margin,
        );

        // Features that are zero in the input must have zero contribution
        // (ablating zero is a no-op).
        for (i, name) in FEATURE_NAMES.iter().enumerate() {
            if features[i] == 0.0 {
                assert!(
                    attributed.contributions[i].abs() < 1e-4,
                    "feature {name} is 0 in input but contribution is {}",
                    attributed.contributions[i],
                );
            }
        }

        // The biggest non-zero feature (`diff_elo` at 80, vs the trained
        // model's heavy reliance on the GBPM cluster) should produce a
        // non-trivial contribution. Don't assert the exact ranking — that
        // depends on the trained model — just that *some* set feature has
        // a meaningfully non-zero attribution, so we know ablation is
        // wired up.
        let max_abs = attributed
            .contributions
            .iter()
            .map(|c| c.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_abs > 0.1,
            "no feature contributed > 0.1 points; ablation likely broken (max |c| = {max_abs})",
        );
    }
}
