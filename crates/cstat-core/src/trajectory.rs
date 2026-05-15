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
//! Feature shape (48 cols, order is wire-locked):
//!   - 5 volume/context (mpg, gp, total_min, height_in, class_year_code)
//!   - 6 box-score per-game (ppg, rpg, apg, spg, bpg, topg)
//!   - 10 rate stats (ts, efg, usg, ast%, tov%, orb%, drb%, stl%, blk%, ft_rate)
//!   - 4 impact metrics (ogbpm, dgbpm, gbpm, campom)
//!   - 12 archetype mixture (primary 1.0× / secondary 0.5×)
//!   - 11 recruit block (see `recruit_features.rs::RECRUIT_FEATURE_NAMES`)
//!
//! Recruit block is sourced via LEFT JOIN against `recruits`; majority of
//! returners pre-2024 have no row and fall into the `recruit_is_ranked=0`
//! sentinel branch. The freshman-impact prior model (next PR) will reuse
//! the same `recruit_features` module so this contract is shared.

use sqlx::PgPool;
use uuid::Uuid;

use crate::recruit_features::{
    RECRUIT_NUM_FEATURES, RecruitFeatureRow, build_recruit_feature_block,
};
use crate::roster_features::ARCHETYPES;

/// Size of the trajectory-specific feature head (volume/context + box +
/// rate + impact + archetype). The recruit block is appended after, and
/// `TRAJECTORY_NUM_FEATURES = TRAJECTORY_HEAD_FEATURES + RECRUIT_NUM_FEATURES`.
const TRAJECTORY_HEAD_FEATURES: usize = 37;

/// Number of input features each of the three trajectory ONNX models expects.
/// Wire-locked to `trajectory_model_meta.json::features` order.
pub const TRAJECTORY_NUM_FEATURES: usize = TRAJECTORY_HEAD_FEATURES + RECRUIT_NUM_FEATURES;

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
    // Recruit block (11) — must equal `RECRUIT_FEATURE_NAMES` in lockstep.
    // The `recruit_names_match_subarray` test cross-validates the literal
    // against the const so accidental edits to one diverge loudly.
    "recruit_is_ranked",
    "recruit_composite_rank",
    "recruit_composite_rating",
    "recruit_star_rating",
    "recruit_position_rank",
    "recruit_rank_movement",
    "recruit_height_in",
    "recruit_weight_lb",
    "recruit_bmi_proxy",
    "recruit_position_code",
    "years_since_recruit",
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
    // Recruit block — LEFT JOIN on `recruits.cstat_player_id`. When the
    // join misses (most pre-2024 returners) every field is None and the
    // recruit feature block falls into its sentinel branch. Types mirror
    // `migrations/020_recruits.sql`: composite_rank / position_rank /
    // previous_rank / weight / year are INTEGER (i32); composite_rating
    // is REAL (f32); star_rating is SMALLINT (i16); height / position
    // are TEXT.
    pub recruit_composite_rank: Option<i32>,
    pub recruit_composite_rating: Option<f32>,
    pub recruit_star_rating: Option<i16>,
    pub recruit_position_rank: Option<i32>,
    pub recruit_previous_rank: Option<i32>,
    pub recruit_height: Option<String>,
    pub recruit_weight: Option<i32>,
    pub recruit_position: Option<String>,
    pub recruit_year: Option<i32>,
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
            pa.secondary_class,
            rec.composite_rank   AS recruit_composite_rank,
            rec.composite_rating AS recruit_composite_rating,
            rec.star_rating      AS recruit_star_rating,
            rec.position_rank    AS recruit_position_rank,
            rec.previous_rank    AS recruit_previous_rank,
            rec.height           AS recruit_height,
            rec.weight           AS recruit_weight,
            rec.position         AS recruit_position,
            rec.year             AS recruit_year
        FROM player_season_stats pss
        JOIN players ply ON ply.id = pss.player_id
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        LEFT JOIN recruits rec
            ON rec.cstat_player_id = pss.player_id
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

/// Convert one DB row into the feature vector the ONNX models expect.
/// Feature order locked to `TRAJECTORY_FEATURE_NAMES`. Missing rate stats
/// are filled with `0.0` (matches the roster_features.rs convention; gp/mpg
/// gate keeps box stats populated). Missing CamPom or GBPM components fall
/// back to `0.0`, but the training pipeline drops rows with these missing —
/// at inference time, callers should check `row.campom.is_some()` before
/// passing to the model to avoid serving a projection built from a sentinel
/// zero.
///
/// `prior_season` is the season the row represents (= `s_n` in the
/// trajectory pairing). It's used by the recruit block to compute
/// `years_since_recruit = prior_season - recruit.year`.
pub fn build_trajectory_features(
    row: &TrajectoryPlayerRow,
    prior_season: i32,
) -> [f32; TRAJECTORY_NUM_FEATURES] {
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

    // Recruit block — sentinel-encoded by `build_recruit_feature_block`
    // when the LEFT JOIN missed. Appended at the end of the vector;
    // RECRUIT_FEATURE_NAMES order is the trailing slice of
    // TRAJECTORY_FEATURE_NAMES.
    let recruit_row = RecruitFeatureRow {
        composite_rank: row.recruit_composite_rank,
        composite_rating: row.recruit_composite_rating.map(|x| x as f64),
        star_rating: row.recruit_star_rating.map(|x| x as i32),
        position_rank: row.recruit_position_rank,
        previous_rank: row.recruit_previous_rank,
        height: row.recruit_height.clone(),
        weight: row.recruit_weight,
        position: row.recruit_position.clone(),
        year: row.recruit_year,
    };
    let recruit_block = build_recruit_feature_block(&recruit_row, prior_season);

    // Build the value list in feature-name order. NULL → 0.0 (rate stats,
    // box stats) or -1 (class_year_code, set above as a real f64).
    // Order MUST match TRAJECTORY_FEATURE_NAMES; the recruit block is
    // appended after the archetype mixture.
    let values_head: [f64; TRAJECTORY_HEAD_FEATURES] = [
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
    for (i, v) in values_head.iter().enumerate() {
        out[i] = *v as f32;
    }
    for (i, v) in recruit_block.iter().enumerate() {
        out[TRAJECTORY_HEAD_FEATURES + i] = *v;
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
    use crate::recruit_features::RECRUIT_FEATURE_NAMES;

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
            recruit_composite_rank: None,
            recruit_composite_rating: None,
            recruit_star_rating: None,
            recruit_position_rank: None,
            recruit_previous_rank: None,
            recruit_height: None,
            recruit_weight: None,
            recruit_position: None,
            recruit_year: None,
        }
    }

    #[test]
    fn feature_vector_layout() {
        let row = make_row();
        let v = build_trajectory_features(&row, 2026);
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
        // No recruit row in make_row() — every recruit slot is its sentinel.
        let is_ranked_idx = TRAJECTORY_FEATURE_NAMES
            .iter()
            .position(|&n| n == "recruit_is_ranked")
            .unwrap();
        let years_since_idx = TRAJECTORY_FEATURE_NAMES
            .iter()
            .position(|&n| n == "years_since_recruit")
            .unwrap();
        assert_eq!(v[is_ranked_idx], 0.0);
        assert_eq!(v[years_since_idx], -1.0);
    }

    #[test]
    fn recruit_block_appended_when_present() {
        let mut row = make_row();
        row.recruit_composite_rank = Some(5);
        row.recruit_composite_rating = Some(0.9852);
        row.recruit_star_rating = Some(5);
        row.recruit_position_rank = Some(2);
        row.recruit_previous_rank = Some(8);
        row.recruit_height = Some("6-8".into());
        row.recruit_weight = Some(220);
        row.recruit_position = Some("SF".into());
        row.recruit_year = Some(2025);

        let v = build_trajectory_features(&row, 2026);

        let idx = |name: &str| {
            TRAJECTORY_FEATURE_NAMES
                .iter()
                .position(|&n| n == name)
                .unwrap()
        };
        assert_eq!(v[idx("recruit_is_ranked")], 1.0);
        assert_eq!(v[idx("recruit_composite_rank")], 5.0);
        assert!((v[idx("recruit_composite_rating")] - 0.9852).abs() < 1e-4);
        assert_eq!(v[idx("recruit_star_rating")], 5.0);
        assert_eq!(v[idx("recruit_rank_movement")], 3.0); // 8 - 5
        assert_eq!(v[idx("recruit_height_in")], 80.0); // 6'8" = 80
        assert_eq!(v[idx("recruit_weight_lb")], 220.0);
        assert_eq!(v[idx("recruit_position_code")], 2.0); // SF
        assert_eq!(v[idx("years_since_recruit")], 1.0); // 2026 - 2025
    }

    #[test]
    fn recruit_names_match_subarray() {
        // Hard-check the contract: the trailing slice of
        // TRAJECTORY_FEATURE_NAMES must equal RECRUIT_FEATURE_NAMES in
        // order. If someone reorders one without the other, this test
        // (and the boot validator) catch it before a stale model serves
        // garbage predictions.
        let trailing = &TRAJECTORY_FEATURE_NAMES[TRAJECTORY_HEAD_FEATURES..];
        assert_eq!(trailing.len(), RECRUIT_FEATURE_NAMES.len());
        for (got, expected) in trailing.iter().zip(RECRUIT_FEATURE_NAMES.iter()) {
            assert_eq!(got, expected);
        }
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
            recruit_composite_rank: None,
            recruit_composite_rating: None,
            recruit_star_rating: None,
            recruit_position_rank: None,
            recruit_previous_rank: None,
            recruit_height: None,
            recruit_weight: None,
            recruit_position: None,
            recruit_year: None,
        };
        let v = build_trajectory_features(&row, 2026);
        // Sentinel slots: class_year_code (-1), recruit_composite_rank (-1),
        // recruit_position_rank (-1), recruit_position_code (-1),
        // years_since_recruit (-1). Everything else 0.0.
        let class_year_idx = TRAJECTORY_FEATURE_NAMES
            .iter()
            .position(|&n| n == "prior_class_year_code")
            .unwrap();
        let neg_one_features: &[&str] = &[
            "prior_class_year_code",
            "recruit_composite_rank",
            "recruit_position_rank",
            "recruit_position_code",
            "years_since_recruit",
        ];
        let neg_one_indices: Vec<usize> = neg_one_features
            .iter()
            .map(|name| {
                TRAJECTORY_FEATURE_NAMES
                    .iter()
                    .position(|n| n == name)
                    .unwrap()
            })
            .collect();
        assert_eq!(v[class_year_idx], -1.0);
        for (i, &x) in v.iter().enumerate() {
            if neg_one_indices.contains(&i) {
                assert_eq!(
                    x, -1.0,
                    "feature {} expected -1.0 got {}",
                    TRAJECTORY_FEATURE_NAMES[i], x
                );
            } else {
                assert_eq!(
                    x, 0.0,
                    "feature {} expected 0.0 got {}",
                    TRAJECTORY_FEATURE_NAMES[i], x
                );
            }
        }
    }
}
