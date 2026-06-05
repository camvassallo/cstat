//! Roster-impact projection features (the impact-aggregation pipeline).
//!
//! The box-score `roster_features::build_roster_features` deliberately
//! *excludes* cam_v3: for a roster-*swap* model, feeding it a player-impact
//! metric collapses it to the identity `Σ(cam_v3 × minute_share) ≈ AdjEM`
//! and kills the composition signal.
//!
//! The roster-impact projection is the opposite use case. A *roster projection*
//! is exactly that
//! identity — `AdjEM_projected = f(Σ projected cam_v3)`. This module builds
//! the feature vector for `roster_impact_model.onnx`, a clean calibrator
//! from a roster's projected-cam_v3 distribution (plus archetype and
//! experience structure) to team AdjEM. At serve time the projections
//! route supplies *projected* cam_v3 (trajectory model for returners /
//! arrivals, freshman model for recruits via `freshman_row`);
//! all projection error then lives in those upstream models — honest and
//! decomposable.
//!
//! Train/serve parity: this builder and `train_roster_impact_model.py`
//! apply the identical rotation normalization — rank by cam_v3, take the
//! top 13, weight every aggregate by `CANONICAL_ROTATION_MPG` by rank.
//! No out-of-distribution minutes (the box-score failure mode).

use crate::roster_features::{ARCHETYPES, CANONICAL_ROTATION_MPG, PlayerRow};
use std::collections::HashMap;
use uuid::Uuid;

/// Feature count for `roster_impact_model.onnx`. Layout:
/// `[roster_size, 8×cam_*, 4×exp_*_share, 12×arch_*, outbound_cam_v3_sum,
///   inbound_cam_v3_sum]`.
pub const ROSTER_IMPACT_NUM_FEATURES: usize = 27;

/// Feature names, wire-locked to the ONNX input column order. Must match
/// `roster_impact_model_meta.json::features` byte-for-byte — the boot
/// validator (`inference::validate_roster_impact_meta`) fails fast on drift.
pub const ROSTER_IMPACT_FEATURE_NAMES: [&str; ROSTER_IMPACT_NUM_FEATURES] = [
    "roster_size",
    "cam_wmean",
    "cam_sum",
    "cam_top1",
    "cam_top3_mean",
    "cam_top7_mean",
    "cam_count_gt5",
    "cam_count_gt10",
    "cam_count_gt15",
    "exp_fr_share",
    "exp_so_share",
    "exp_jr_share",
    "exp_sr_share",
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
    "outbound_cam_v3_sum",
    "inbound_cam_v3_sum",
];

/// Map an inconsistently-stored `class_year` to an experience bucket
/// (0 = Fr, 1 = So, 2 = Jr, 3 = Sr). Mirrors `normalize_class` in
/// `train_roster_impact_model.py` exactly — prefix-matched so `Senior` /
/// `Sr` / `SR` all fold to the same bucket. Unrecognized → `None`
/// (contributes to no experience share).
fn class_bucket(cy: Option<&str>) -> Option<usize> {
    let lc = cy?.trim().to_ascii_lowercase();
    if lc.starts_with("fr") {
        Some(0)
    } else if lc.starts_with("so") {
        Some(1)
    } else if lc.starts_with("jr") || lc.starts_with("ju") {
        Some(2)
    } else if lc.starts_with("sr") || lc.starts_with("se") {
        Some(3)
    } else {
        None
    }
}

/// Overwrite each roster row's `cam_v3` with a forward-looking projection.
///
/// `projected` maps `player_id → projected next-season cam_v3` (the
/// trajectory model's output for returners / arrivals). Rows absent from
/// the map keep their existing `cam_v3` — for synthesized recruits that's
/// the freshman model's prediction (set by `freshman_row`); for
/// a returner whose trajectory inference failed it's their current-season
/// cam_v3, a "no growth projected" fallback that's better than dropping
/// them out of the rotation.
pub fn apply_projected_cam_v3(roster: &mut [PlayerRow], projected: &HashMap<Uuid, f64>) {
    for p in roster.iter_mut() {
        if let Some(&v) = projected.get(&p.player_id) {
            p.cam_v3 = Some(v);
        }
    }
}

/// Build the 27-feature roster-impact vector for one projected roster.
///
/// Expects each `PlayerRow.cam_v3` to already carry the *projected*
/// next-season value (call `apply_projected_cam_v3` first). The roster is
/// ranked by cam_v3 descending (`None` sorts last), the top 13 form the
/// rotation, and `CANONICAL_ROTATION_MPG` supplies the per-rank minute
/// weight for every minutes-weighted aggregate — identical to the
/// training-side aggregation in `train_roster_impact_model.py`.
///
/// `outbound_cam_v3_sum` is the team's portal loss: the sum of base-season
/// cam_v3 across players who left this team via the spring portal cycle
/// moving them into the target season (positive = lost talent).
/// `inbound_cam_v3_sum` is the symmetric portal gain: sum of base-season
/// cam_v3 across players who arrived from other D-I teams.
///
/// Both sums use base-season cam_v3 (the player's level when last
/// observed). Missing torvik coverage on a portal player contributes 0
/// (`COALESCE` convention, matches the audit script and the SQL in
/// `train_roster_impact_model.py::{OUTBOUND_QUERY, INBOUND_QUERY}`).
/// 0.0 is the sentinel for pre-portal-era seasons or teams with no
/// movement; the tree-based model naturally splits on `> 0` vs `= 0`.
/// They live as separate features (rather than a single net) so the
/// trees can learn asymmetric effects — a team gaining and losing
/// equivalent CamPom isn't necessarily AdjEM-neutral (different roles,
/// system fit, etc.).
///
/// An empty roster yields an all-zero vector except for the portal
/// slots, which still record the team's portal movement — a team can
/// have 0 qualifying returners and still have lost / gained talent.
pub fn build_roster_impact_features(
    roster: &[PlayerRow],
    outbound_cam_v3_sum: f32,
    inbound_cam_v3_sum: f32,
) -> [f32; ROSTER_IMPACT_NUM_FEATURES] {
    let mut out = [0.0_f32; ROSTER_IMPACT_NUM_FEATURES];
    out[25] = outbound_cam_v3_sum;
    out[26] = inbound_cam_v3_sum;
    if roster.is_empty() {
        return out;
    }

    // Rank by cam_v3 desc; missing coverage sorts last (bench slots) —
    // same convention as `roster_features::project_rotation`.
    let mut by_rank: Vec<&PlayerRow> = roster.iter().collect();
    by_rank.sort_by(|a, b| {
        let aq = a.cam_v3.unwrap_or(f64::NEG_INFINITY);
        let bq = b.cam_v3.unwrap_or(f64::NEG_INFINITY);
        bq.partial_cmp(&aq).unwrap_or(std::cmp::Ordering::Equal)
    });
    let rotation_n = by_rank.len().min(CANONICAL_ROTATION_MPG.len());
    let rotation = &by_rank[..rotation_n];
    let total_w: f64 = CANONICAL_ROTATION_MPG[..rotation_n].iter().sum();

    // [0] roster_size — rotation depth (≤ 13).
    out[0] = rotation_n as f32;

    // [1..9] cam_v3 distribution — over rotation players with Torvik
    // coverage (`Some`). A player with `None` cam_v3 still holds a
    // rotation slot (counts toward roster_size / experience / archetype)
    // but is skipped here, exactly as the training aggregator drops NaN.
    let cam: Vec<(f64, f64)> = rotation
        .iter()
        .enumerate()
        .filter_map(|(rank, p)| p.cam_v3.map(|c| (c, CANONICAL_ROTATION_MPG[rank])))
        .collect();
    if !cam.is_empty() {
        let wsum: f64 = cam.iter().map(|(_, w)| w).sum();
        out[1] = if wsum > 0.0 {
            (cam.iter().map(|(c, w)| c * w).sum::<f64>() / wsum) as f32
        } else {
            0.0
        };
        out[2] = cam.iter().map(|(c, _)| c).sum::<f64>() as f32;

        let mut vals: Vec<f64> = cam.iter().map(|(c, _)| *c).collect();
        vals.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let mean_of_top = |k: usize| -> f32 {
            let n = k.min(vals.len());
            (vals[..n].iter().sum::<f64>() / n as f64) as f32
        };
        out[3] = vals[0] as f32;
        out[4] = mean_of_top(3);
        out[5] = mean_of_top(7);
        out[6] = cam.iter().filter(|(c, _)| *c > 5.0).count() as f32;
        out[7] = cam.iter().filter(|(c, _)| *c > 10.0).count() as f32;
        out[8] = cam.iter().filter(|(c, _)| *c > 15.0).count() as f32;
    }

    // [9..13] experience mix — canonical-MPG-weighted class shares.
    let mut exp_w = [0.0_f64; 4];
    for (rank, p) in rotation.iter().enumerate() {
        if let Some(idx) = class_bucket(p.class_year.as_deref()) {
            exp_w[idx] += CANONICAL_ROTATION_MPG[rank];
        }
    }
    if total_w > 0.0 {
        for (i, w) in exp_w.iter().enumerate() {
            out[9 + i] = (w / total_w) as f32;
        }
    }

    // [13..25] archetype balance — canonical-MPG-weighted primary-class
    // shares. Players without an archetype (synthesized recruits)
    // contribute to no bucket, so a recruit-heavy roster's shares sum to
    // < 1 — intentional dilution, matching the training aggregator.
    let mut arch_w = [0.0_f64; 12];
    for (rank, p) in rotation.iter().enumerate() {
        if let Some(cls) = p.primary_class.as_deref()
            && let Some(i) = ARCHETYPES.iter().position(|a| *a == cls)
        {
            arch_w[i] += CANONICAL_ROTATION_MPG[rank];
        }
    }
    if total_w > 0.0 {
        for (i, w) in arch_w.iter().enumerate() {
            out[13 + i] = (w / total_w) as f32;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meta_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../training/models/roster_impact_model_meta.json")
    }

    /// Build a PlayerRow with the fields the impact features read.
    fn row(cam_v3: Option<f64>, class: Option<&str>, arch: Option<&str>) -> PlayerRow {
        PlayerRow {
            player_id: Uuid::new_v4(),
            total_min: 600.0,
            mpg: 24.0,
            ppg: Some(10.0),
            rpg: Some(4.0),
            apg: Some(2.0),
            spg: Some(1.0),
            bpg: Some(0.5),
            topg: Some(1.5),
            ts: Some(0.55),
            efg: Some(0.52),
            usg: Some(20.0),
            ast_pct: Some(15.0),
            tov_pct: Some(15.0),
            orb_pct: Some(5.0),
            drb_pct: Some(15.0),
            stl_pct: Some(2.0),
            blk_pct: Some(1.5),
            ft_rate: Some(0.35),
            primary_class: arch.map(str::to_string),
            secondary_class: None,
            class_year: class.map(str::to_string),
            cam_v3,
        }
    }

    #[test]
    fn feature_count_constant_matches_names() {
        assert_eq!(
            ROSTER_IMPACT_FEATURE_NAMES.len(),
            ROSTER_IMPACT_NUM_FEATURES
        );
    }

    #[test]
    fn empty_roster_is_all_zero() {
        let out = build_roster_impact_features(&[], 0.0, 0.0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn roster_size_caps_at_rotation_depth() {
        // 15 players — only the top 13 by cam_v3 form the rotation.
        let roster: Vec<PlayerRow> = (0..15)
            .map(|i| row(Some(i as f64), Some("Fr"), Some("Wizard")))
            .collect();
        let out = build_roster_impact_features(&roster, 0.0, 0.0);
        assert_eq!(out[0], 13.0, "roster_size should cap at 13");
    }

    #[test]
    fn cam_aggregates_are_rank_ordered() {
        // cam_v3 values 10, 8, 6, 4, 2 — top1 = 10, sum = 30.
        let roster: Vec<PlayerRow> = [10.0, 8.0, 6.0, 4.0, 2.0]
            .iter()
            .map(|&c| row(Some(c), Some("So"), Some("Sorcerer")))
            .collect();
        let out = build_roster_impact_features(&roster, 0.0, 0.0);
        assert!((out[2] - 30.0).abs() < 1e-4, "cam_sum");
        assert!((out[3] - 10.0).abs() < 1e-4, "cam_top1");
        assert!((out[4] - 8.0).abs() < 1e-4, "cam_top3_mean = (10+8+6)/3");
        // count > 5 = {10, 8, 6} = 3; > 10 = 0.
        assert!((out[6] - 3.0).abs() < 1e-4, "cam_count_gt5");
        assert!((out[7] - 0.0).abs() < 1e-4, "cam_count_gt10");
        // cam_wmean weights by canonical MPG → top players dominate, so
        // the weighted mean sits above the simple mean (6.0).
        assert!(
            out[1] > 6.0,
            "cam_wmean front-loads the top of the rotation"
        );
    }

    #[test]
    fn missing_cam_v3_holds_a_slot_but_skips_cam_aggregates() {
        // 2 covered + 1 uncovered → roster_size 3, cam aggregates over 2.
        let roster = vec![
            row(Some(12.0), Some("Jr"), Some("Druid")),
            row(Some(8.0), Some("Jr"), Some("Druid")),
            row(None, Some("Jr"), Some("Druid")),
        ];
        let out = build_roster_impact_features(&roster, 0.0, 0.0);
        assert_eq!(out[0], 3.0, "uncovered player still holds a rotation slot");
        assert!((out[2] - 20.0).abs() < 1e-4, "cam_sum over the 2 covered");
        // All three are Jr → exp_jr_share == 1.0.
        assert!((out[11] - 1.0).abs() < 1e-4, "exp_jr_share");
    }

    #[test]
    fn experience_and_archetype_shares_are_minute_weighted() {
        // Rank 0 (32 MPG) = Sr/Wizard, rank 1 (29.8) = Fr/Bard.
        let roster = vec![
            row(Some(15.0), Some("Senior"), Some("Wizard")),
            row(Some(5.0), Some("Fr"), Some("Bard")),
        ];
        let out = build_roster_impact_features(&roster, 0.0, 0.0);
        let total = 32.0 + 29.8;
        // exp_sr_share = 32 / 61.8 ; exp_fr_share = 29.8 / 61.8.
        assert!((out[12] - (32.0 / total)).abs() < 1e-4, "exp_sr_share");
        assert!((out[9] - (29.8 / total)).abs() < 1e-4, "exp_fr_share");
        // arch_wizard at index 13, arch_bard at index 16.
        assert!((out[13] - (32.0 / total)).abs() < 1e-4, "arch_wizard share");
        assert!((out[16] - (29.8 / total)).abs() < 1e-4, "arch_bard share");
    }

    #[test]
    fn portal_sums_land_in_trailing_slots() {
        let roster = vec![row(Some(10.0), Some("Jr"), Some("Wizard"))];
        let out = build_roster_impact_features(&roster, 8.5, 3.25);
        assert!((out[25] - 8.5).abs() < 1e-4, "outbound_cam_v3_sum slot");
        assert!((out[26] - 3.25).abs() < 1e-4, "inbound_cam_v3_sum slot");
        // A team can be empty AND still have moved portal talent — verify
        // both portal slots are populated even with zero rotation.
        let empty = build_roster_impact_features(&[], 12.0, 7.0);
        assert!((empty[25] - 12.0).abs() < 1e-4, "outbound on empty roster");
        assert!((empty[26] - 7.0).abs() < 1e-4, "inbound on empty roster");
        for &v in &empty[..25] {
            assert_eq!(v, 0.0, "non-portal slots stay zero on empty roster");
        }
    }

    #[test]
    fn apply_projected_overwrites_only_mapped_rows() {
        let mut roster = vec![
            row(Some(5.0), Some("Fr"), None),
            row(Some(9.0), Some("Fr"), None),
        ];
        let keep_id = roster[1].player_id;
        let mut projected = HashMap::new();
        projected.insert(roster[0].player_id, 12.0);
        apply_projected_cam_v3(&mut roster, &projected);
        assert_eq!(roster[0].cam_v3, Some(12.0), "mapped row overwritten");
        assert_eq!(roster[1].cam_v3, Some(9.0), "unmapped row keeps cam_v3");
        assert_eq!(roster[1].player_id, keep_id);
    }

    /// Boot-critical: the meta JSON's feature order must match the
    /// compiled names. Skips when the model hasn't been trained yet.
    #[test]
    fn feature_names_match_meta_json() {
        let content = match std::fs::read_to_string(meta_path()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("skipping: roster_impact_model_meta.json not found");
                return;
            }
        };
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
        let names: Vec<String> = meta["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), ROSTER_IMPACT_NUM_FEATURES);
        for (i, (actual, expected)) in names
            .iter()
            .zip(ROSTER_IMPACT_FEATURE_NAMES.iter())
            .enumerate()
        {
            assert_eq!(actual, expected, "feature[{i}] mismatch");
        }
    }
}
