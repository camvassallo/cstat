//! Phase 5c growth model: per-player feature builder + boot-time validator.
//!
//! Mirrors `training/train_trajectory_model.py` for the production model
//! (mean + q=0.1 + q=0.9). The Python script and this module share one
//! contract via `training/models/trajectory_model_meta.json` — the loader
//! in `inference.rs` hard-fails if `player_filter`, feature order, or
//! quantile alphas drift.
//!
//! Inference is per-player, not per-roster: given a `(player_id, season_N)`,
//! we project that player's `cam_gbpm_v3_psos` for `season_N + 1`. The
//! projection is destination-agnostic — for transferring players in v1 we
//! don't pass a destination-team feature, so the model reads them the same
//! way it reads same-team returners.
//!
//! Feature shape (37 cols, order is wire-locked):
//!   - 5 volume/context (mpg, gp, total_min, height_in, class_year_code)
//!   - 6 box-score per-game (ppg, rpg, apg, spg, bpg, topg)
//!   - 10 rate stats (ts, efg, usg, ast%, tov%, orb%, drb%, stl%, blk%, ft_rate)
//!   - 4 impact metrics (ogbpm, dgbpm, gbpm, campom)
//!   - 12 archetype mixture (primary 1.0× / secondary 0.5×)

use sqlx::PgPool;
use uuid::Uuid;

use crate::roster_features::ARCHETYPES;

/// Number of input features each of the three trajectory ONNX models expects.
/// Wire-locked to `trajectory_model_meta.json::features` order.
pub const TRAJECTORY_NUM_FEATURES: usize = 37;

/// Feature names in the exact order the three ONNX models consume. Boot-time
/// validator (see `inference.rs::validate_trajectory_meta`) hard-fails if
/// these drift from the meta JSON.
pub const TRAJECTORY_FEATURE_NAMES: [&str; TRAJECTORY_NUM_FEATURES] = [
    // Volume / context (5)
    "prior_mpg",
    "prior_gp",
    "prior_total_min",
    "prior_height_in",
    "prior_class_year_code",
    // Box score per-game (6)
    "prior_ppg",
    "prior_rpg",
    "prior_apg",
    "prior_spg",
    "prior_bpg",
    "prior_topg",
    // Rate stats (10)
    "prior_ts",
    "prior_efg",
    "prior_usg",
    "prior_ast_pct",
    "prior_tov_pct",
    "prior_orb_pct",
    "prior_drb_pct",
    "prior_stl_pct",
    "prior_blk_pct",
    "prior_ft_rate",
    // Impact (4)
    "prior_ogbpm",
    "prior_dgbpm",
    "prior_gbpm",
    "prior_campom",
    // Archetype mixture (12)
    "arch_wizard",
    "arch_sorcerer",
    "arch_warlock",
    "arch_bard",
    "arch_ranger",
    "arch_barbarian",
    "arch_paladin",
    "arch_monk",
    "arch_cleric",
    "arch_druid",
    "arch_rogue",
    "arch_fighter",
];

/// One player's prior-season row, joined across `player_season_stats`,
/// `torvik_player_stats`, `players`, and `player_archetypes`. Pulled by
/// `fetch_player_trajectory_row` for a single (player_id, season) lookup;
/// converted to the 37-element feature vector by
/// `build_trajectory_features`.
///
/// Class year and archetype assignments may be NULL even for qualified
/// players (Torvik bio is 96% coverage on `torvik_pid`; archetype assignment
/// covers the full qualified cohort). NULLs become sentinel values in the
/// feature vector (class_year → -1, archetype shares → 0.0) so the model
/// sees a consistent signal across rows.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrajectoryPlayerRow {
    // Volume / context
    pub minutes_per_game: Option<f64>,
    pub games_played: Option<i32>,
    pub height_inches: Option<i32>,
    pub class_year: Option<String>,
    // Box score per-game
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    // Rate stats
    pub true_shooting_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub usage_rate: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    // Impact metrics
    pub ogbpm: Option<f64>,
    pub dgbpm: Option<f64>,
    pub gbpm: Option<f64>,
    pub campom: Option<f64>,
    // Archetype mixture
    pub primary_class: Option<String>,
    pub secondary_class: Option<String>,
}

/// `class_year` text → integer code. Mirrors the Python `CLASS_YEAR_CODES`
/// map. NULL/unknown → -1 (separate bucket; LightGBM splits can isolate
/// it). Spelling permissive because NatStat and Torvik backfill use
/// different conventions ("Freshman"/"Fr", "Senior"/"Sr").
fn encode_class_year(s: Option<&str>) -> i32 {
    match s.map(|x| x.trim()) {
        Some("Fr") | Some("Freshman") => 0,
        Some("So") | Some("Sophomore") => 1,
        Some("Jr") | Some("Junior") => 2,
        Some("Sr") | Some("Senior") => 3,
        Some("Gr") | Some("Graduate") | Some("Grad") => 4,
        _ => -1,
    }
}

/// Fetch one prior-season row for trajectory inference. Returns `None` when
/// the player doesn't pass the qualification gate (≥5 GP, ≥5 MPG) for the
/// requested season — caller renders "no projection" rather than getting a
/// noisy prediction off a 3-game sample.
///
/// Joins:
///   - `player_season_stats` (PSS) for box / rate stats + volume gates
///   - `players` for `height_inches` + `class_year`
///   - `torvik_player_stats` (TPS) for GBPM components + CamPom
///   - `player_archetypes` for primary/secondary class
pub async fn fetch_player_trajectory_row(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<TrajectoryPlayerRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, TrajectoryPlayerRow>(
        r#"
        SELECT
            pss.minutes_per_game,
            pss.games_played,
            ply.height_inches,
            ply.class_year,
            pss.ppg,
            pss.rpg,
            pss.apg,
            pss.spg,
            pss.bpg,
            pss.topg,
            pss.true_shooting_pct,
            pss.effective_fg_pct,
            pss.usage_rate,
            pss.ast_pct,
            pss.tov_pct,
            pss.orb_pct,
            pss.drb_pct,
            pss.stl_pct,
            pss.blk_pct,
            pss.ft_rate,
            tps.ogbpm,
            tps.dgbpm,
            tps.gbpm,
            tps.cam_gbpm_v3_psos AS campom,
            pa.primary_class,
            pa.secondary_class
        FROM player_season_stats pss
        JOIN players ply ON ply.id = pss.player_id
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        WHERE pss.player_id = $1
          AND pss.season = $2
          AND pss.games_played >= 5
          AND pss.minutes_per_game >= 5
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Convert one DB row into the 37-element feature vector the ONNX models
/// expect. Feature order locked to `TRAJECTORY_FEATURE_NAMES`. Missing
/// rate stats are filled with `0.0` (matches the roster_features.rs
/// convention; gp/mpg gate keeps box stats populated). Missing CamPom or
/// GBPM components fall back to `0.0`, but the training pipeline drops
/// rows with these missing — at inference time, callers should check
/// `row.campom.is_some()` before passing to the model to avoid serving a
/// projection built from a sentinel zero.
pub fn build_trajectory_features(row: &TrajectoryPlayerRow) -> [f32; TRAJECTORY_NUM_FEATURES] {
    let total_min = match (row.minutes_per_game, row.games_played) {
        (Some(m), Some(g)) => Some(m * g as f64),
        _ => None,
    };
    let class_year_code = encode_class_year(row.class_year.as_deref()) as f64;

    // Archetype mixture: primary 1.0× / secondary 0.5×. Same weighting as
    // the team Identity/Gaps index in §5a.
    let primary = row.primary_class.as_deref();
    let secondary = row.secondary_class.as_deref();
    let mut arch = [0.0_f64; 12];
    for (idx, name) in ARCHETYPES.iter().enumerate() {
        if primary == Some(*name) {
            arch[idx] += 1.0;
        }
        if secondary == Some(*name) {
            arch[idx] += 0.5;
        }
    }

    // Build the value list in feature-name order. NULL → 0.0 (rate stats,
    // box stats) or -1 (class_year_code, set above as a real f64).
    // Order MUST match TRAJECTORY_FEATURE_NAMES.
    let values: [f64; TRAJECTORY_NUM_FEATURES] = [
        // Volume / context (5)
        row.minutes_per_game.unwrap_or(0.0),
        row.games_played.map(|x| x as f64).unwrap_or(0.0),
        total_min.unwrap_or(0.0),
        row.height_inches.map(|x| x as f64).unwrap_or(0.0),
        class_year_code,
        // Box score per-game (6)
        row.ppg.unwrap_or(0.0),
        row.rpg.unwrap_or(0.0),
        row.apg.unwrap_or(0.0),
        row.spg.unwrap_or(0.0),
        row.bpg.unwrap_or(0.0),
        row.topg.unwrap_or(0.0),
        // Rate stats (10)
        row.true_shooting_pct.unwrap_or(0.0),
        row.effective_fg_pct.unwrap_or(0.0),
        row.usage_rate.unwrap_or(0.0),
        row.ast_pct.unwrap_or(0.0),
        row.tov_pct.unwrap_or(0.0),
        row.orb_pct.unwrap_or(0.0),
        row.drb_pct.unwrap_or(0.0),
        row.stl_pct.unwrap_or(0.0),
        row.blk_pct.unwrap_or(0.0),
        row.ft_rate.unwrap_or(0.0),
        // Impact (4)
        row.ogbpm.unwrap_or(0.0),
        row.dgbpm.unwrap_or(0.0),
        row.gbpm.unwrap_or(0.0),
        row.campom.unwrap_or(0.0),
        // Archetype mixture (12)
        arch[0],
        arch[1],
        arch[2],
        arch[3],
        arch[4],
        arch[5],
        arch[6],
        arch[7],
        arch[8],
        arch[9],
        arch[10],
        arch[11],
    ];

    let mut out = [0.0_f32; TRAJECTORY_NUM_FEATURES];
    for (i, v) in values.iter().enumerate() {
        out[i] = *v as f32;
    }
    out
}

/// Trajectory inference result. `mean` is the model's central projection;
/// `lower` / `upper` are the q=0.1 / q=0.9 quantile model outputs. The
/// band width is what the UI surfaces as the floor/ceiling — wider for
/// freshmen with thin samples, tighter for senior returners.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrajectoryPrediction {
    pub mean: f32,
    pub lower: f32,
    pub upper: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row() -> TrajectoryPlayerRow {
        TrajectoryPlayerRow {
            minutes_per_game: Some(28.4),
            games_played: Some(32),
            height_inches: Some(80),
            class_year: Some("Junior".into()),
            ppg: Some(16.2),
            rpg: Some(7.1),
            apg: Some(3.4),
            spg: Some(1.2),
            bpg: Some(0.5),
            topg: Some(2.1),
            true_shooting_pct: Some(0.58),
            effective_fg_pct: Some(0.54),
            usage_rate: Some(0.22),
            ast_pct: Some(0.18),
            tov_pct: Some(0.14),
            orb_pct: Some(0.05),
            drb_pct: Some(0.18),
            stl_pct: Some(0.024),
            blk_pct: Some(0.018),
            ft_rate: Some(0.35),
            ogbpm: Some(2.1),
            dgbpm: Some(1.2),
            gbpm: Some(3.3),
            campom: Some(4.5),
            primary_class: Some("Wizard".into()),
            secondary_class: Some("Bard".into()),
        }
    }

    #[test]
    fn feature_vector_layout() {
        let row = make_row();
        let v = build_trajectory_features(&row);
        // Spot-checks against the locked feature order.
        assert_eq!(v[0], 28.4); // prior_mpg
        assert_eq!(v[1], 32.0); // prior_gp
        assert!((v[2] - 28.4 * 32.0).abs() < 1e-3); // prior_total_min
        assert_eq!(v[3], 80.0); // prior_height_in
        assert_eq!(v[4], 2.0); // prior_class_year_code (Junior = 2)
        assert!((v[5] - 16.2).abs() < 1e-3); // prior_ppg
        assert!((v[24] - 4.5).abs() < 1e-3); // prior_campom (index 24)
        // Wizard slot fires at 1.0; Bard at 0.5; rest 0.
        let wiz_idx = TRAJECTORY_FEATURE_NAMES
            .iter()
            .position(|&n| n == "arch_wizard")
            .unwrap();
        let bard_idx = TRAJECTORY_FEATURE_NAMES
            .iter()
            .position(|&n| n == "arch_bard")
            .unwrap();
        assert!((v[wiz_idx] - 1.0).abs() < 1e-6);
        assert!((v[bard_idx] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn class_year_encoding() {
        assert_eq!(encode_class_year(Some("Fr")), 0);
        assert_eq!(encode_class_year(Some("Freshman")), 0);
        assert_eq!(encode_class_year(Some("Sr")), 3);
        assert_eq!(encode_class_year(Some("Senior")), 3);
        assert_eq!(encode_class_year(Some("Grad")), 4);
        assert_eq!(encode_class_year(Some("Graduate")), 4);
        assert_eq!(encode_class_year(Some("  Junior  ")), 2);
        assert_eq!(encode_class_year(None), -1);
        assert_eq!(encode_class_year(Some("Weird value")), -1);
    }

    #[test]
    fn missing_values_become_sentinels() {
        let row = TrajectoryPlayerRow {
            minutes_per_game: None,
            games_played: None,
            height_inches: None,
            class_year: None,
            ppg: None,
            rpg: None,
            apg: None,
            spg: None,
            bpg: None,
            topg: None,
            true_shooting_pct: None,
            effective_fg_pct: None,
            usage_rate: None,
            ast_pct: None,
            tov_pct: None,
            orb_pct: None,
            drb_pct: None,
            stl_pct: None,
            blk_pct: None,
            ft_rate: None,
            ogbpm: None,
            dgbpm: None,
            gbpm: None,
            campom: None,
            primary_class: None,
            secondary_class: None,
        };
        let v = build_trajectory_features(&row);
        // class_year_code slot becomes -1 sentinel; everything else 0.
        assert_eq!(v[4], -1.0);
        for (i, &x) in v.iter().enumerate() {
            if i == 4 {
                continue;
            }
            assert_eq!(
                x, 0.0,
                "feature {} expected 0.0 got {}",
                TRAJECTORY_FEATURE_NAMES[i], x
            );
        }
    }
}
