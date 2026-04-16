use ort::session::Session;
use std::path::Path;
use std::sync::Mutex;

/// Number of input features expected by the ONNX models.
pub const NUM_FEATURES: usize = 49;

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
    "diff_w_bpm",
    "diff_w_player_sos",
    "diff_w_obpm",
    "diff_w_dbpm",
    "diff_w_ortg",
    "diff_w_ast_pct",
    "diff_w_tov_pct",
    "diff_w_stl_pct",
    "diff_w_blk_pct",
    "diff_w_gbpm",
    "diff_star_ppg",
    "diff_star_bpm",
    "diff_star_gbpm",
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

        // Strong home team: positive efficiency margin, high ELO diff
        let mut home_favored = [0.0_f32; NUM_FEATURES];
        home_favored[0] = 1.0; // venue = home
        home_favored[5] = 15.0; // diff_adj_efficiency_margin
        home_favored[16] = 100.0; // diff_elo

        // Strong away team: flip the signs
        let mut away_favored = [0.0_f32; NUM_FEATURES];
        away_favored[0] = 1.0;
        away_favored[5] = -15.0;
        away_favored[16] = -100.0;

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
}
