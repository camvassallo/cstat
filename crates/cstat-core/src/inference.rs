use ort::session::Session;
use std::path::Path;
use std::sync::Mutex;

/// Number of input features expected by the ONNX models.
pub const NUM_FEATURES: usize = 47;

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
    "diff_star_ppg",
    "diff_star_bpm",
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

        // Win model: outputs [label, probabilities]
        let win_input = TensorRef::from_array_view((shape, features.as_slice()))?;
        let mut win_session = self.win_session.lock().unwrap();
        let win_outputs = win_session.run(ort::inputs![win_input])?;
        let (_, probs) = win_outputs[1].try_extract_tensor::<f64>()?;
        // Index 1 = probability of class 1 (home win)
        let home_win_probability = if probs.len() >= 2 { probs[1] } else { probs[0] };

        Ok(Prediction {
            predicted_margin,
            home_win_probability,
        })
    }
}
