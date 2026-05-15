//! Phase 6 freshman-impact prior model: per-recruit projection of a
//! freshman's first-college-season CamPom v3.
//!
//! Mirrors `training/train_freshman_model.py` for the production model
//! (mean + q=0.1 + q=0.9). The Python script and this module share one
//! contract via `training/models/freshman_model_meta.json`; the loader in
//! `inference.rs` hard-fails if `player_filter`, feature order, or
//! quantile alphas drift.
//!
//! Inference is per-recruit: given a `recruit_id`, project the recruit's
//! upcoming freshman-season `cam_gbpm_v3_psos`. Target season is
//! implicitly `recruit.year + 1` (the season the recruit first plays).
//!
//! Feature shape (13 cols, order is wire-locked):
//!   - 11 from the shared recruit block (see `recruit_features.rs`)
//!   - 2 freshman-specific (school-context):
//!     * `committed_team_prior_adjem` — committed team's AdjEM the
//!       season BEFORE the recruit arrived. Captures program quality at
//!       signing time. Sentinel 0.0 if missing (team without a
//!       `team_season_stats` row for `recruit.year`).
//!     * `peer_class_strength` — mean composite_rating across the
//!       committed team's full class for that year, including the focal
//!       recruit. Sentinel 0.0 if the team has no rated recruits in the
//!       class.
//!
//! These two features rank #1 and #2 by importance in the trained model;
//! they're the half of the lift that recruit-direct features alone can't
//! supply. They also intentionally avoid the dog-fooding trap of reading
//! the recruit's actual freshman-season team AdjEM (which would be partly
//! determined by the very recruit we're projecting).

use sqlx::PgPool;
use uuid::Uuid;

use crate::recruit_features::{
    RECRUIT_NUM_FEATURES, RecruitFeatureRow, build_recruit_feature_block,
};

/// Size of the freshman-specific feature tail (school-context).
const FRESHMAN_EXTRA_FEATURES: usize = 2;

/// Number of input features each of the three freshman ONNX models expects.
/// Wire-locked to `freshman_model_meta.json::features` order.
pub const FRESHMAN_NUM_FEATURES: usize = RECRUIT_NUM_FEATURES + FRESHMAN_EXTRA_FEATURES;

/// Feature names in the exact order the three ONNX models consume. The
/// recruit block is the leading slice; school-context features follow.
/// Boot-time validator (`inference.rs::validate_freshman_meta`) hard-fails
/// if these drift from the meta JSON. The `freshman_names_match_recruit_subarray`
/// test cross-validates the literal against `RECRUIT_FEATURE_NAMES` so
/// accidental edits diverge loudly.
pub const FRESHMAN_FEATURE_NAMES: [&str; FRESHMAN_NUM_FEATURES] = [
    // Recruit block (11) — must equal RECRUIT_FEATURE_NAMES.
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
    // School-context (2).
    "committed_team_prior_adjem",
    "peer_class_strength",
];

/// One recruit's pre-college feature row. Combines raw recruit fields
/// (consumed by the shared `RecruitFeatureRow` builder) with the two
/// school-context features computed at fetch time. All optional because
/// joins may miss (defunct programs, solo signings, etc.); missing
/// values use the sentinel encoding documented at the column level.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FreshmanFeatureRow {
    // Raw recruit fields — types mirror `migrations/020_recruits.sql`.
    pub composite_rank: Option<i32>,
    pub composite_rating: Option<f32>,
    pub star_rating: Option<i16>,
    pub position_rank: Option<i32>,
    pub previous_rank: Option<i32>,
    pub height: Option<String>,
    pub weight: Option<i32>,
    pub position: Option<String>,
    /// Recruiting class year (calendar year of HS graduation). The
    /// existence signal — `Some(y)` means we have a recruit row.
    pub year: Option<i32>,
    /// Committed team's AdjEM in `recruit.year` (the season BEFORE the
    /// recruit first plays). Reads from `team_season_stats`. NULL when
    /// the team has no prior-season row (defunct, conference-realignment
    /// edge case, etc.).
    pub committed_team_prior_adjem: Option<f64>,
    /// Mean 247 composite_rating across the committed team's full class
    /// for `recruit.year`, including the focal recruit. NULL when no
    /// other recruit at the same team in that class has a rating.
    pub peer_class_strength: Option<f64>,
}

/// Fetch one recruit's feature row for freshman-impact inference.
/// Returns `None` if the recruit_id doesn't exist. A recruit row whose
/// `committed_team_id` doesn't resolve to a `team_season_stats` row
/// still returns `Some(...)` — the school-context features fall back to
/// their sentinels and the model handles it.
///
/// Join chain:
///   - `recruits` keyed on `id = $1`
///   - committed team's UUID → natstat_id traversal to find the team's
///     instance in the season before the recruit arrived (UUIDs are
///     season-scoped; natstat_id is the stable cross-season identifier)
///   - `team_season_stats` for that prior season's AdjEM
///   - a per-(year, committed_team) AVG(composite_rating) subquery for
///     peer-class strength
pub async fn fetch_freshman_features(
    pool: &PgPool,
    recruit_id: Uuid,
) -> Result<Option<FreshmanFeatureRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, FreshmanFeatureRow>(
        r#"
        SELECT
            r.composite_rank,
            r.composite_rating,
            r.star_rating,
            r.position_rank,
            r.previous_rank,
            r.height,
            r.weight,
            r.position,
            r.year,
            adjem.adj_efficiency_margin AS committed_team_prior_adjem,
            peer.mean_rating AS peer_class_strength
        FROM recruits r
        LEFT JOIN teams tm_signing
            ON tm_signing.id = r.committed_team_id
        LEFT JOIN teams tm_prior
            ON tm_prior.natstat_id = tm_signing.natstat_id
            AND tm_prior.season = r.year
        LEFT JOIN team_season_stats adjem
            ON adjem.team_id = tm_prior.id AND adjem.season = r.year
        LEFT JOIN (
            SELECT year, committed_team_id, AVG(composite_rating) AS mean_rating
            FROM recruits
            WHERE composite_rating IS NOT NULL AND committed_team_id IS NOT NULL
            GROUP BY year, committed_team_id
        ) peer
            ON peer.year = r.year AND peer.committed_team_id = r.committed_team_id
        WHERE r.id = $1
        "#,
    )
    .bind(recruit_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Convert one DB row into the 13-element feature vector the ONNX models
/// expect. Feature order locked to `FRESHMAN_FEATURE_NAMES`. The first 11
/// slots delegate to `build_recruit_feature_block`; the last 2 are the
/// school-context features with sentinel 0.0 for NULLs.
///
/// `prior_season` for the recruit block is `recruit.year` so that
/// `years_since_recruit = recruit.year - recruit.year = 0` for every
/// freshman (degenerate-but-consistent, mirrors the Python training).
/// When the recruit row is missing entirely (`year.is_none()`), the
/// extractor's existing -1 sentinel for `years_since_recruit` fires
/// instead — the same way the trajectory model treats no-recruit rows.
pub fn build_freshman_features(row: &FreshmanFeatureRow) -> [f32; FRESHMAN_NUM_FEATURES] {
    let recruit_row = RecruitFeatureRow {
        composite_rank: row.composite_rank,
        composite_rating: row.composite_rating.map(|x| x as f64),
        star_rating: row.star_rating.map(|x| x as i32),
        position_rank: row.position_rank,
        previous_rank: row.previous_rank,
        height: row.height.clone(),
        weight: row.weight,
        position: row.position.clone(),
        year: row.year,
    };
    // Training passes `recruit.year` as prior_season so years_since=0;
    // sentinel branch fires when year is None (no recruit row).
    let prior_season = row.year.unwrap_or(0);
    let recruit_block = build_recruit_feature_block(&recruit_row, prior_season);

    let mut out = [0.0_f32; FRESHMAN_NUM_FEATURES];
    for (i, v) in recruit_block.iter().enumerate() {
        out[i] = *v;
    }
    out[RECRUIT_NUM_FEATURES] = row.committed_team_prior_adjem.unwrap_or(0.0) as f32;
    out[RECRUIT_NUM_FEATURES + 1] = row.peer_class_strength.unwrap_or(0.0) as f32;
    out
}

/// Freshman inference result. `mean` is the model's central projection;
/// `lower` / `upper` are the q=0.1 / q=0.9 quantile model outputs. Band
/// width is what the UI surfaces — wide for thin-sample tiers (T1 elite
/// where draft-bound prospects skew the corpus), tighter for T3/T4 where
/// the training set is dense.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FreshmanPrediction {
    pub mean: f32,
    pub lower: f32,
    pub upper: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recruit_features::RECRUIT_FEATURE_NAMES;

    fn make_row() -> FreshmanFeatureRow {
        // Class-of-2025 SF, 6'7", 215 lb, ranked #5, committed to a
        // top-tier program. Mirrors the recruit_features.rs test case
        // so failures point at the same input shape.
        FreshmanFeatureRow {
            composite_rank: Some(5),
            composite_rating: Some(0.9852),
            star_rating: Some(5),
            position_rank: Some(2),
            previous_rank: Some(8),
            height: Some("6-7".into()),
            weight: Some(215),
            position: Some("SF".into()),
            year: Some(2025),
            committed_team_prior_adjem: Some(28.5),
            peer_class_strength: Some(0.9651),
        }
    }

    #[test]
    fn feature_vector_layout() {
        let row = make_row();
        let v = build_freshman_features(&row);

        let idx = |name: &str| {
            FRESHMAN_FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap()
        };
        assert_eq!(v[idx("recruit_is_ranked")], 1.0);
        assert_eq!(v[idx("recruit_composite_rank")], 5.0);
        assert!((v[idx("recruit_composite_rating")] - 0.9852).abs() < 1e-4);
        assert_eq!(v[idx("recruit_height_in")], 79.0);
        assert_eq!(v[idx("recruit_position_code")], 2.0); // SF
        // years_since_recruit = recruit.year - recruit.year = 0 for all
        // freshmen by construction.
        assert_eq!(v[idx("years_since_recruit")], 0.0);
        // School-context tail.
        assert!((v[idx("committed_team_prior_adjem")] - 28.5).abs() < 1e-4);
        assert!((v[idx("peer_class_strength")] - 0.9651).abs() < 1e-4);
    }

    #[test]
    fn missing_school_context_uses_sentinels() {
        let mut row = make_row();
        row.committed_team_prior_adjem = None;
        row.peer_class_strength = None;
        let v = build_freshman_features(&row);
        let idx = |name: &str| {
            FRESHMAN_FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap()
        };
        assert_eq!(v[idx("committed_team_prior_adjem")], 0.0);
        assert_eq!(v[idx("peer_class_strength")], 0.0);
    }

    #[test]
    fn missing_recruit_row_falls_back_to_recruit_sentinels() {
        // If somehow `year.is_none()`, the recruit block is fully
        // sentinel — including `years_since_recruit = -1` instead of 0.
        // Practically the SQL won't return a row with year=NULL because
        // `recruits.year` is NOT NULL; this test guards against future
        // join-shape changes that could surface that case.
        let row = FreshmanFeatureRow {
            composite_rank: None,
            composite_rating: None,
            star_rating: None,
            position_rank: None,
            previous_rank: None,
            height: None,
            weight: None,
            position: None,
            year: None,
            committed_team_prior_adjem: None,
            peer_class_strength: None,
        };
        let v = build_freshman_features(&row);
        let idx = |name: &str| {
            FRESHMAN_FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap()
        };
        assert_eq!(v[idx("recruit_is_ranked")], 0.0);
        assert_eq!(v[idx("years_since_recruit")], -1.0);
    }

    #[test]
    fn freshman_names_match_recruit_subarray() {
        // Hard-check the recruit contract: the leading slice of
        // FRESHMAN_FEATURE_NAMES must equal RECRUIT_FEATURE_NAMES in
        // order. Mirrors the trajectory model's similar test — if the
        // shared block reorders, this test (and the boot validator)
        // catch the drift loudly.
        let head = &FRESHMAN_FEATURE_NAMES[..RECRUIT_NUM_FEATURES];
        assert_eq!(head.len(), RECRUIT_FEATURE_NAMES.len());
        for (got, expected) in head.iter().zip(RECRUIT_FEATURE_NAMES.iter()) {
            assert_eq!(got, expected);
        }
    }
}
