//! Shared recruit-feature block consumed by the Phase 5c trajectory model
//! and (next) the freshman-impact prior model. Mirrors
//! `training/recruit_features.py` — feature order, sentinel conventions,
//! and the position-code taxonomy must match exactly. The
//! `trajectory_model_meta.json` validator at boot enforces the contract.
//!
//! Coverage caveat: class-of-2022 through 2026 are ingested today, but
//! the majority of trajectory-model rows are upperclassmen from earlier
//! classes that have no recruit row and fall into the
//! `recruit_is_ranked=0` bucket via sentinel encoding. LightGBM fits a
//! separate branch on the unranked-majority cohort. Historical backfill
//! (class-of-2021 and earlier) is a follow-up PR.
//!
//! Feature shape (11 cols, locked order — must equal Python
//! `RECRUIT_FEATURE_NAMES` in `training/recruit_features.py`):
//!   - `recruit_is_ranked` (0/1 — existence flag, drives a model branch)
//!   - `recruit_composite_rank` (i32; -1 sentinel)
//!   - `recruit_composite_rating` (f32; 0.0 missing)
//!   - `recruit_star_rating` (i32; 0 missing)
//!   - `recruit_position_rank` (i32; -1 sentinel)
//!   - `recruit_rank_movement` (i32; previous − current, 0 if either missing)
//!   - `recruit_height_in` (i32; parsed from "feet-inches", 0 if missing)
//!   - `recruit_weight_lb` (i32; 0 if missing)
//!   - `recruit_bmi_proxy` (f32; standard imperial BMI, 0.0 if either missing)
//!   - `recruit_position_code` (i32; PG=0..C=4, CG=5, other=-1)
//!   - `years_since_recruit` (i32; prior_season − recruit.year, -1 if no row)

/// Number of features the recruit block contributes. Wire-locked to
/// `RECRUIT_FEATURE_NAMES` and the Python-side
/// `training/recruit_features.py::N_RECRUIT_FEATURES`.
pub const RECRUIT_NUM_FEATURES: usize = 11;

/// Feature names in the exact order producers/consumers must use.
pub const RECRUIT_FEATURE_NAMES: [&str; RECRUIT_NUM_FEATURES] = [
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

/// Raw recruit row, captured from a LEFT JOIN against `recruits`. All fields
/// are `Option<…>` because the join is LEFT (most pre-2024 returners have
/// no recruit row); when `year` is `None` the entire block is treated as
/// "no recruit data" (`recruit_is_ranked=0` and sentinel values throughout).
#[derive(Debug, Clone, Default)]
pub struct RecruitFeatureRow {
    pub composite_rank: Option<i32>,
    pub composite_rating: Option<f64>,
    pub star_rating: Option<i32>,
    pub position_rank: Option<i32>,
    pub previous_rank: Option<i32>,
    pub height: Option<String>,
    pub weight: Option<i32>,
    pub position: Option<String>,
    /// Recruiting class year. `Some(y)` is the existence signal — when this
    /// is `None`, we treat all other fields as missing too (the LEFT JOIN
    /// returned no row).
    pub year: Option<i32>,
}

/// Parse 247's `feet-inches` height string into total inches. Returns
/// `None` on unparseable / out-of-range input. Empirically all ingested
/// recruits (n=1,147 with a cstat join) parse cleanly in range `5-6`
/// through `7-3`.
pub fn parse_height_inches(s: Option<&str>) -> Option<i32> {
    let s = s?;
    let (feet_str, inches_str) = s.split_once('-')?;
    let feet: i32 = feet_str.trim().parse().ok()?;
    let inches: i32 = inches_str.trim().parse().ok()?;
    if !(4..=8).contains(&feet) || !(0..=11).contains(&inches) {
        return None;
    }
    Some(feet * 12 + inches)
}

/// Map a 247 position string to its integer code. `None` / unknown → -1.
/// MUST match the Python `POSITION_CODES` dict in `recruit_features.py`.
pub fn position_code(p: Option<&str>) -> i32 {
    match p.map(|x| x.trim().to_ascii_uppercase()) {
        Some(ref s) if s == "PG" => 0,
        Some(ref s) if s == "SG" => 1,
        Some(ref s) if s == "SF" => 2,
        Some(ref s) if s == "PF" => 3,
        Some(ref s) if s == "C" => 4,
        Some(ref s) if s == "CG" => 5,
        _ => -1,
    }
}

/// Build the 11-element recruit feature block. `prior_season` is the
/// trajectory model's `s_n` (i.e., the season we're predicting *from*) —
/// `years_since_recruit = prior_season - recruit.year`. When the recruit
/// row is absent (`row.year.is_none()`), every slot uses its sentinel.
pub fn build_recruit_feature_block(
    row: &RecruitFeatureRow,
    prior_season: i32,
) -> [f32; RECRUIT_NUM_FEATURES] {
    let is_ranked = row.year.is_some();

    let composite_rank = row.composite_rank.unwrap_or(-1);
    let composite_rating = row.composite_rating.unwrap_or(0.0);
    let star_rating = row.star_rating.unwrap_or(0);
    let position_rank = row.position_rank.unwrap_or(-1);

    // Rank movement: positive = climbed (previous rank was a higher number),
    // negative = fell. Both ranks must be present; 0 is the neutral sentinel.
    let rank_movement = match (row.previous_rank, row.composite_rank) {
        (Some(prev), Some(curr)) => prev - curr,
        _ => 0,
    };

    let height_in = parse_height_inches(row.height.as_deref()).unwrap_or(0);
    let weight_lb = row.weight.unwrap_or(0);
    let bmi_proxy = if height_in > 0 && weight_lb > 0 {
        703.0 * (weight_lb as f64) / ((height_in as f64) * (height_in as f64))
    } else {
        0.0
    };

    let pos_code = position_code(row.position.as_deref());
    let years_since = match row.year {
        Some(y) => prior_season - y,
        None => -1,
    };

    // Order MUST match RECRUIT_FEATURE_NAMES.
    [
        if is_ranked { 1.0 } else { 0.0 },
        composite_rank as f32,
        composite_rating as f32,
        star_rating as f32,
        position_rank as f32,
        rank_movement as f32,
        height_in as f32,
        weight_lb as f32,
        bmi_proxy as f32,
        pos_code as f32,
        years_since as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_parser() {
        assert_eq!(parse_height_inches(Some("6-7")), Some(79));
        assert_eq!(parse_height_inches(Some("5-10")), Some(70));
        assert_eq!(parse_height_inches(Some("7-3")), Some(87));
        assert_eq!(parse_height_inches(Some("6-12")), None); // inches out of range
        assert_eq!(parse_height_inches(Some("9-0")), None); // feet out of range
        assert_eq!(parse_height_inches(Some("garbage")), None);
        assert_eq!(parse_height_inches(None), None);
    }

    #[test]
    fn position_codes_match_python() {
        // These integers are the contract with `training/recruit_features.py`.
        assert_eq!(position_code(Some("PG")), 0);
        assert_eq!(position_code(Some("SG")), 1);
        assert_eq!(position_code(Some("SF")), 2);
        assert_eq!(position_code(Some("PF")), 3);
        assert_eq!(position_code(Some("C")), 4);
        assert_eq!(position_code(Some("CG")), 5);
        assert_eq!(position_code(Some("pg")), 0); // case-insensitive
        assert_eq!(position_code(Some("  SF  ")), 2); // trims whitespace
        assert_eq!(position_code(Some("G")), -1); // unknown
        assert_eq!(position_code(None), -1);
    }

    #[test]
    fn block_layout_full_row() {
        // 247-style recruit: ranked #5, 6'7", 215 lb, SF, class-of-2025,
        // moved up 3 spots from previous list (prev rank 8). Projecting
        // from cstat-season 2026 = 1 year past freshman.
        let row = RecruitFeatureRow {
            composite_rank: Some(5),
            composite_rating: Some(0.9852),
            star_rating: Some(5),
            position_rank: Some(2),
            previous_rank: Some(8),
            height: Some("6-7".into()),
            weight: Some(215),
            position: Some("SF".into()),
            year: Some(2025),
        };
        let v = build_recruit_feature_block(&row, 2026);

        let idx = |name: &str| {
            RECRUIT_FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap()
        };
        assert_eq!(v[idx("recruit_is_ranked")], 1.0);
        assert_eq!(v[idx("recruit_composite_rank")], 5.0);
        assert!((v[idx("recruit_composite_rating")] - 0.9852).abs() < 1e-4);
        assert_eq!(v[idx("recruit_star_rating")], 5.0);
        assert_eq!(v[idx("recruit_position_rank")], 2.0);
        assert_eq!(v[idx("recruit_rank_movement")], 3.0); // 8 - 5
        assert_eq!(v[idx("recruit_height_in")], 79.0); // 6'7"
        assert_eq!(v[idx("recruit_weight_lb")], 215.0);
        // BMI = 703 * 215 / 79^2 ≈ 24.21
        assert!((v[idx("recruit_bmi_proxy")] - 24.21).abs() < 0.05);
        assert_eq!(v[idx("recruit_position_code")], 2.0); // SF
        assert_eq!(v[idx("years_since_recruit")], 1.0); // 2026 - 2025
    }

    #[test]
    fn missing_row_becomes_sentinels() {
        let row = RecruitFeatureRow::default(); // year is None → no recruit
        let v = build_recruit_feature_block(&row, 2026);

        let idx = |name: &str| {
            RECRUIT_FEATURE_NAMES
                .iter()
                .position(|n| *n == name)
                .unwrap()
        };
        assert_eq!(v[idx("recruit_is_ranked")], 0.0);
        assert_eq!(v[idx("recruit_composite_rank")], -1.0);
        assert_eq!(v[idx("recruit_composite_rating")], 0.0);
        assert_eq!(v[idx("recruit_star_rating")], 0.0);
        assert_eq!(v[idx("recruit_position_rank")], -1.0);
        assert_eq!(v[idx("recruit_rank_movement")], 0.0);
        assert_eq!(v[idx("recruit_height_in")], 0.0);
        assert_eq!(v[idx("recruit_weight_lb")], 0.0);
        assert_eq!(v[idx("recruit_bmi_proxy")], 0.0);
        assert_eq!(v[idx("recruit_position_code")], -1.0);
        assert_eq!(v[idx("years_since_recruit")], -1.0);
    }

    #[test]
    fn previous_rank_missing_movement_zero() {
        // Ranked freshman without previous-rank history (44pct of ranked
        // recruits in cstat data, per recruits-table coverage check).
        let row = RecruitFeatureRow {
            composite_rank: Some(50),
            composite_rating: Some(0.9),
            star_rating: Some(4),
            position_rank: Some(8),
            previous_rank: None,
            height: Some("6-3".into()),
            weight: Some(195),
            position: Some("PG".into()),
            year: Some(2024),
        };
        let v = build_recruit_feature_block(&row, 2025);
        let movement_idx = RECRUIT_FEATURE_NAMES
            .iter()
            .position(|n| *n == "recruit_rank_movement")
            .unwrap();
        assert_eq!(v[movement_idx], 0.0);
    }
}
