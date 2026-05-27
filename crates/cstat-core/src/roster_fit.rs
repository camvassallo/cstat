//! Archetype-based roster fit scoring.
//!
//! **Status: kept as building blocks for the archetype visualization
//! layer (Phase 5b — 12-axis radial roster plot, Team Compare view).
//! Not consumed by any production scoring surface.** A Fit chip shipped
//! briefly on TransferPortal (v1, then v2) and was reverted after
//! `training/validate_archetype_balance.py` showed the chip's
//! balance-is-good prior has the wrong sign across 4,216 team-seasons.
//! Per-archetype value spread is real (~8 CamPom from Druid to
//! Fighter); concentration in high-value classes amplifies edge rather
//! than diluting it; using categorical archetypes to *score* a
//! candidate is strictly worse than ranking by the continuous CamPom
//! signal that informs the clustering in the first place. Full
//! rationale in `docs/archetype_balance_finding.md`.
//!
//! Two baselines preserved for visualization / debug use:
//! - **v1** ([`compute_fit_score`]): destination's source-season archetype
//!   distribution, looked up via `queries::get_team_archetype_index`.
//!   Matches what TeamDetail's Roster Archetypes panel shows.
//! - **v2** ([`fit_score_against_projected`]): destination's projected
//!   next-season distribution (returning − departures + arrivals +
//!   recruits + uncertain), with the candidate's own contribution
//!   subtracted out before scoring. Recruits and other archetype-less
//!   players have their minutes distributed across the 12 classes via
//!   the D-I prior. Requires running `compose_all_projections` upstream.
//!
//! If you are about to add a new product surface that uses these
//! functions to *score* players or teams (best-fit destination, fit-
//! adjusted Δrating, etc.), read the balance-finding doc first — the
//! prior these functions encode is empirically wrong on average.

use std::collections::HashMap;

use serde::Serialize;

use crate::queries::ArchetypeShare;
use crate::roster_features::PlayerRow;

/// Scored fit of a candidate's archetype against a destination roster.
///
/// `raw` lives in `[-1.0, +1.0]`: +1.0 means the candidate's primary class
/// is completely missing from the destination (gap_strength = 1.0); -1.0
/// means the destination is over-indexed by 2.5×+ in that class
/// (saturated redundancy). Around 0 = roster-neutral.
#[derive(Debug, Clone, Serialize)]
pub struct FitScore {
    pub raw: f64,
    pub tier: FitTier,
    /// One-line human-readable summary — "Fills Cleric gap", "Stacks
    /// Wizard rotation", "Roster-neutral". The frontend renders this in
    /// the tooltip directly so we don't duplicate threshold logic on
    /// both sides.
    pub label: String,
    /// Destination's index ratio for the candidate's primary class
    /// (team_share ÷ d1_share). 0.0 = completely missing class; 1.0 =
    /// at league average; >1.0 = over-indexed. Exposed so callers can
    /// surface raw context in tooltips without recomputing.
    pub primary_index: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FitTier {
    StrongFit,
    GoodFit,
    Neutral,
    SomeRedundancy,
    Redundant,
}

/// Compute the fit score of a candidate's archetype against a destination's
/// current archetype distribution.
///
/// `team_dist` is the destination's per-class shares as returned by
/// `queries::get_team_archetype_index` (or its bulk multi-team variant).
/// Classes the destination has zero minutes in won't appear in `team_dist`
/// — we treat their index as 0.0 (full gap).
pub fn compute_fit_score(
    primary: &str,
    secondary: Option<&str>,
    team_dist: &[ArchetypeShare],
) -> FitScore {
    let primary_index = lookup_index(team_dist, primary);
    let primary_signal = signal_for_index(primary_index);

    let (secondary_signal, secondary_index) = match secondary {
        Some(sec) if !sec.is_empty() && sec != primary => {
            let idx = lookup_index(team_dist, sec);
            (signal_for_index(idx), Some(idx))
        }
        _ => (0.0, None),
    };

    // Primary 1.0× + secondary 0.5× matches the Identity/Gaps SQL
    // weighting. Divide by 1.5 so the combined signal stays in [-1, 1].
    let raw = (primary_signal + 0.5 * secondary_signal) / 1.5;
    let tier = classify(raw);
    let label = build_label(primary, primary_index, secondary, secondary_index, raw);

    FitScore {
        raw,
        tier,
        label,
        primary_index,
    }
}

/// Returns the destination's index ratio for `class`, or 0.0 if the class
/// is absent from `team_dist` (= the team has no minutes there, which
/// semantically is a maximum-strength gap).
fn lookup_index(team_dist: &[ArchetypeShare], class: &str) -> f64 {
    team_dist
        .iter()
        .find(|s| s.primary_class == class)
        .and_then(|s| s.index)
        .unwrap_or(0.0)
}

/// Map a team's index ratio for one class into a per-class signal in
/// `[-1.0, +1.0]`.
///
/// `index = 0.0` (totally missing class) → +1.0 (maximum gap).
/// `index = 1.0` (at league average) → 0.0.
/// `index = 2.5` (over-indexed by 2.5×) → -1.0 (saturated redundancy).
///
/// The denominator `1.5` on the redundancy side is calibrated against the
/// TeamDetail Identity threshold (≥1.3× surfaces as Identity); a team at
/// the 1.3 threshold already reads as `-0.20` redundancy, and runs of
/// 2.0× — common for true Identity stacks — read as `-0.67`. Heavy
/// stacks (Duke's three Wizards, say) saturate at -1.0 around 2.5×.
fn signal_for_index(idx: f64) -> f64 {
    if !idx.is_finite() {
        return 0.0;
    }
    if idx < 1.0 {
        (1.0 - idx).clamp(0.0, 1.0)
    } else {
        -((idx - 1.0) / 1.5).clamp(0.0, 1.0)
    }
}

fn classify(raw: f64) -> FitTier {
    if raw >= 0.40 {
        FitTier::StrongFit
    } else if raw >= 0.10 {
        FitTier::GoodFit
    } else if raw > -0.10 {
        FitTier::Neutral
    } else if raw > -0.40 {
        FitTier::SomeRedundancy
    } else {
        FitTier::Redundant
    }
}

/// Build a one-line label describing the dominant story.
///
/// Thresholds (`<= 0.85` for "fills", `>= 1.15` for "stacks") are aligned
/// with the *tier* boundaries — i.e., the cutoffs where `raw` itself
/// crosses ±0.10 for primary-only contributions — so the label always
/// tracks the chip color. Using the TeamDetail Identity threshold
/// (≥1.30×) here instead would leave a dead zone in 1.15–1.30 where
/// the chip reads SomeRedundancy but the label said "Roster-neutral."
///
/// Priority ordering: primary is the heavier-weighted signal, so if
/// it's the side underweighted (or overweighted), name primary's class.
/// Secondary only carries the label when primary is on the wrong side
/// of 1.0 (the league average) and secondary has the opposite story.
/// "Fills missing X" is reserved for genuinely absent classes
/// (index < 0.15) since that reads differently from "underweighted."
fn build_label(
    primary: &str,
    primary_index: f64,
    secondary: Option<&str>,
    secondary_index: Option<f64>,
    raw: f64,
) -> String {
    if raw >= 0.10 && primary_index <= 0.85 {
        return if primary_index < 0.15 {
            format!("Fills missing {primary}")
        } else {
            format!("Fills {primary} gap")
        };
    }
    if raw <= -0.10 && primary_index >= 1.15 {
        return format!("Stacks {primary} rotation");
    }

    // Primary doesn't carry the story (it's at-or-near league average
    // for the direction `raw` reflects). Check secondary.
    if let (Some(sec), Some(sec_idx)) = (secondary, secondary_index) {
        if raw >= 0.10 && sec_idx <= 0.85 {
            return if sec_idx < 0.15 {
                format!("Secondary fills missing {sec}")
            } else {
                format!("Secondary fills {sec} gap")
            };
        }
        if raw <= -0.10 && sec_idx >= 1.15 {
            return format!("Secondary stacks {sec} rotation");
        }
    }

    "Roster-neutral".to_string()
}

/// Build a per-class minutes map for the destination's projected
/// next-season roster.
///
/// Each archetype-bearing player contributes `total_min` to their primary
/// class and `0.5 * total_min` to their secondary (if any) — mirroring
/// the weighting in `queries::get_team_archetype_index`.
///
/// Players with no archetype assignment (synthesized recruits, sub-D1
/// arrivals) contribute `total_min * d1_shares[class]` to each class so
/// the distribution stays *unbiased* under archetype-less minutes
/// instead of leaving those minutes uncounted (which would systematically
/// inflate every class's perceived gap on teams with big incoming HS
/// classes). When `d1_shares` is empty the dispersal silently no-ops —
/// callers paying for the D-I-shares query are expected to provide a
/// populated map.
pub fn build_projected_class_minutes(
    roster: &[PlayerRow],
    d1_shares: &HashMap<String, f64>,
) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for p in roster {
        let m = p.total_min;
        if m <= 0.0 || !m.is_finite() {
            continue;
        }
        match p.primary_class.as_deref() {
            Some(primary) => {
                *out.entry(primary.to_string()).or_insert(0.0) += m;
                if let Some(sec) = p.secondary_class.as_deref()
                    && !sec.is_empty()
                    && sec != primary
                {
                    *out.entry(sec.to_string()).or_insert(0.0) += 0.5 * m;
                }
            }
            None => {
                for (cls, share) in d1_shares {
                    if *share > 0.0 {
                        *out.entry(cls.clone()).or_insert(0.0) += m * share;
                    }
                }
            }
        }
    }
    out
}

/// v2 baseline: score a candidate against a destination's *projected*
/// next-season archetype distribution, with the candidate's own
/// contribution subtracted out so we measure their *marginal* fit.
///
/// `team_class_minutes` is the destination roster's per-class weighted
/// minutes (see [`build_projected_class_minutes`]). `candidate_minutes`
/// is the candidate's `total_min` on the projected roster (= rank-slot
/// canonical MPG × GP after `roster_projection::project_rotation`); pass
/// 0.0 to skip self-exclusion when the candidate isn't on the roster.
/// `d1_shares` is the D-I prior used to compute the `index` ratio.
///
/// Internally clones the minutes map, subtracts the candidate's primary
/// (1.0×) + secondary (0.5×) contribution, materializes into a
/// `Vec<ArchetypeShare>`, then delegates to [`compute_fit_score`] so all
/// tier/label logic stays in one place.
pub fn fit_score_against_projected(
    candidate_primary: &str,
    candidate_secondary: Option<&str>,
    candidate_minutes: f64,
    team_class_minutes: &HashMap<String, f64>,
    d1_shares: &HashMap<String, f64>,
) -> FitScore {
    let mut adjusted: HashMap<String, f64> = team_class_minutes.clone();
    if candidate_minutes > 0.0 && candidate_minutes.is_finite() {
        if let Some(m) = adjusted.get_mut(candidate_primary) {
            *m = (*m - candidate_minutes).max(0.0);
        }
        if let Some(sec) = candidate_secondary
            && !sec.is_empty()
            && sec != candidate_primary
            && let Some(m) = adjusted.get_mut(sec)
        {
            *m = (*m - 0.5 * candidate_minutes).max(0.0);
        }
    }
    let total: f64 = adjusted.values().sum();
    let dist: Vec<ArchetypeShare> = adjusted
        .into_iter()
        .map(|(class, minutes)| {
            let team_share = if total > 0.0 { minutes / total } else { 0.0 };
            let d1_share = d1_shares.get(&class).copied().unwrap_or(0.0);
            let index = if d1_share > 0.0 && minutes > 0.0 {
                Some(team_share / d1_share)
            } else {
                None
            };
            ArchetypeShare {
                primary_class: class,
                team_count: 0,
                team_minutes: minutes,
                team_share,
                d1_share,
                index,
            }
        })
        .collect();
    compute_fit_score(candidate_primary, candidate_secondary, &dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(class: &str, index: f64) -> ArchetypeShare {
        ArchetypeShare {
            primary_class: class.to_string(),
            team_count: 1,
            team_minutes: 100.0,
            team_share: 0.1,
            d1_share: 0.1,
            index: Some(index),
        }
    }

    #[test]
    fn missing_primary_class_is_full_gap() {
        // Team has no Cleric — candidate Cleric fills a 100% gap.
        let dist = vec![share("Wizard", 1.5), share("Sorcerer", 1.2)];
        let fit = compute_fit_score("Cleric", None, &dist);
        assert!(fit.raw > 0.65, "expected strong fit, got {}", fit.raw);
        assert_eq!(fit.tier, FitTier::StrongFit);
        assert!(fit.label.contains("missing Cleric"));
        assert_eq!(fit.primary_index, 0.0);
    }

    #[test]
    fn heavy_redundancy_is_strong_negative() {
        // Team is 3× over-indexed in Wizard — another Wizard saturates
        // redundancy.
        let dist = vec![share("Wizard", 3.0)];
        let fit = compute_fit_score("Wizard", None, &dist);
        assert!(
            fit.raw < -0.65,
            "expected strong redundancy, got {}",
            fit.raw
        );
        assert_eq!(fit.tier, FitTier::Redundant);
        assert!(fit.label.contains("Stacks Wizard"));
    }

    #[test]
    fn neutral_index_is_near_zero() {
        let dist = vec![share("Wizard", 1.0)];
        let fit = compute_fit_score("Wizard", None, &dist);
        assert!(fit.raw.abs() < 0.01, "expected ~0, got {}", fit.raw);
        assert_eq!(fit.tier, FitTier::Neutral);
        assert_eq!(fit.label, "Roster-neutral");
    }

    #[test]
    fn secondary_class_contributes_at_half_weight() {
        // Both primary and secondary fill totally-missing gaps. Combined
        // signal should be (1.0 + 0.5 * 1.0) / 1.5 = 1.0.
        let dist = vec![share("Wizard", 1.0)]; // unrelated class present
        let primary_only = compute_fit_score("Cleric", None, &dist);
        let with_secondary = compute_fit_score("Cleric", Some("Paladin"), &dist);
        // Primary alone: signal = 1.0, raw = 1.0 / 1.5 ≈ 0.667.
        assert!((primary_only.raw - 0.6667).abs() < 0.01);
        // Secondary added: raw climbs to 1.0 (saturated).
        assert!((with_secondary.raw - 1.0).abs() < 0.01);
    }

    #[test]
    fn secondary_softens_primary_redundancy() {
        // Primary heavily over-indexed; secondary fills a gap. Net
        // should land between full redundancy and neutral, with
        // primary still dominating.
        let dist = vec![share("Wizard", 2.5), share("Bard", 1.0)];
        let fit = compute_fit_score("Wizard", Some("Cleric"), &dist);
        // Primary signal: -1.0, secondary: +1.0 (Cleric missing).
        // Combined: (-1.0 + 0.5) / 1.5 = -0.333.
        assert!((fit.raw - (-0.333)).abs() < 0.01, "got {}", fit.raw);
        assert_eq!(fit.tier, FitTier::SomeRedundancy);
    }

    #[test]
    fn secondary_matching_primary_is_ignored() {
        // Defensive: if 247 / archetype model emits identical primary
        // and secondary, don't double-count.
        let dist = vec![share("Wizard", 1.5)];
        let fit_a = compute_fit_score("Wizard", Some("Wizard"), &dist);
        let fit_b = compute_fit_score("Wizard", None, &dist);
        assert!((fit_a.raw - fit_b.raw).abs() < 1e-9);
    }

    #[test]
    fn good_fit_tier_for_moderate_gap() {
        // Team is underweighted in Cleric (index 0.5) — not a missing
        // class, but a real gap.
        let dist = vec![share("Cleric", 0.5)];
        let fit = compute_fit_score("Cleric", None, &dist);
        // signal = 0.5, raw = 0.5 / 1.5 ≈ 0.333.
        assert!((fit.raw - 0.333).abs() < 0.01, "got {}", fit.raw);
        assert_eq!(fit.tier, FitTier::GoodFit);
        assert!(fit.label.contains("Cleric gap"));
    }

    #[test]
    fn signal_clamps_above_saturation_index() {
        // index = 5.0 still produces -1.0, not -2.67. Same for
        // negative-direction extremes.
        assert!((signal_for_index(5.0) - (-1.0)).abs() < 1e-9);
        assert!((signal_for_index(0.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nan_or_inf_index_is_neutral() {
        // Defensive — `ArchetypeShare.index` can be None upstream; the
        // caller's `unwrap_or(0.0)` handles that. This test pins the
        // direct-NaN safety net inside signal_for_index.
        assert_eq!(signal_for_index(f64::NAN), 0.0);
        assert_eq!(signal_for_index(f64::INFINITY), 0.0);
    }

    #[test]
    fn label_tracks_tier_in_mild_redundancy_window() {
        // Regression: primary_index 1.25 used to fall through to
        // "Roster-neutral" even though raw -0.111 is SomeRedundancy.
        // The label thresholds (≥1.15 / ≤0.85) now match the ±0.10
        // tier boundaries so the chip color and the text agree.
        let dist = vec![share("Wizard", 1.25)];
        let fit = compute_fit_score("Wizard", None, &dist);
        // signal = -(0.25/1.5) = -0.167, raw = -0.111.
        assert!((fit.raw - (-0.111)).abs() < 0.01, "got raw {}", fit.raw);
        assert_eq!(fit.tier, FitTier::SomeRedundancy);
        assert!(
            fit.label.contains("Stacks Wizard"),
            "expected 'Stacks Wizard' label, got {}",
            fit.label
        );
    }

    #[test]
    fn label_tracks_tier_in_mild_gap_window() {
        // Mirror of the above: primary_index 0.80 is GoodFit territory
        // (raw 0.133). Label should call out the gap.
        let dist = vec![share("Cleric", 0.80)];
        let fit = compute_fit_score("Cleric", None, &dist);
        assert!((fit.raw - 0.133).abs() < 0.01, "got raw {}", fit.raw);
        assert_eq!(fit.tier, FitTier::GoodFit);
        assert!(
            fit.label.contains("Cleric gap"),
            "expected 'Cleric gap' label, got {}",
            fit.label
        );
    }

    #[test]
    fn label_stays_neutral_when_no_class_is_overweighted() {
        // Edge case: both classes mildly over-indexed (1.10/1.05)
        // — combined raw stays inside the neutral band (-0.10, +0.10),
        // so chip is Neutral and label should agree.
        let dist = vec![share("Wizard", 1.10), share("Bard", 1.05)];
        let fit = compute_fit_score("Wizard", Some("Bard"), &dist);
        assert_eq!(fit.tier, FitTier::Neutral);
        assert_eq!(fit.label, "Roster-neutral");
    }

    #[test]
    fn empty_distribution_treats_every_class_as_gap() {
        // Edge case: a destination with no archetype coverage (no
        // player_archetypes rows). Every class shows as missing →
        // candidate primary scores as maximum gap. Defensible default
        // for sub-D1 transitions or seasons where archetypes haven't
        // been trained yet.
        let fit = compute_fit_score("Wizard", Some("Bard"), &[]);
        assert!((fit.raw - 1.0).abs() < 0.01);
        assert_eq!(fit.tier, FitTier::StrongFit);
    }

    // -----------------------------------------------------------------
    // v2: build_projected_class_minutes + fit_score_against_projected
    // -----------------------------------------------------------------

    use uuid::Uuid;

    /// PlayerRow helper for v2 tests. Only fields the v2 path reads are
    /// populated; everything else gets defaults.
    fn pr(total_min: f64, primary: Option<&str>, secondary: Option<&str>) -> PlayerRow {
        PlayerRow {
            player_id: Uuid::new_v4(),
            total_min,
            mpg: 24.0,
            ppg: None,
            rpg: None,
            apg: None,
            spg: None,
            bpg: None,
            topg: None,
            ts: None,
            efg: None,
            usg: None,
            ast_pct: None,
            tov_pct: None,
            orb_pct: None,
            drb_pct: None,
            stl_pct: None,
            blk_pct: None,
            ft_rate: None,
            primary_class: primary.map(str::to_string),
            secondary_class: secondary.map(str::to_string),
            class_year: None,
            cam_v3: None,
        }
    }

    fn d1_prior() -> HashMap<String, f64> {
        // Uniform 12-class prior — handy for dispersal-symmetry assertions.
        let classes = [
            "Barbarian",
            "Bard",
            "Cleric",
            "Druid",
            "Fighter",
            "Monk",
            "Paladin",
            "Ranger",
            "Rogue",
            "Sorcerer",
            "Warlock",
            "Wizard",
        ];
        classes
            .iter()
            .map(|c| (c.to_string(), 1.0 / 12.0))
            .collect()
    }

    #[test]
    fn projected_minutes_sums_primary_plus_half_secondary() {
        // Single player: 300 min primary Wizard, 0.5× secondary Bard
        // → Wizard 300, Bard 150.
        let roster = vec![pr(300.0, Some("Wizard"), Some("Bard"))];
        let d1 = d1_prior();
        let dist = build_projected_class_minutes(&roster, &d1);
        assert!((dist.get("Wizard").copied().unwrap_or(0.0) - 300.0).abs() < 1e-9);
        assert!((dist.get("Bard").copied().unwrap_or(0.0) - 150.0).abs() < 1e-9);
        // No other class should have minutes — this player has an
        // archetype, so the D-I-prior dispersal branch doesn't fire.
        assert!(!dist.contains_key("Cleric"));
    }

    #[test]
    fn projected_minutes_disperses_archetype_less_via_d1_prior() {
        // A recruit (no archetype) with 300 min should add equal mass to
        // each of the 12 classes under a uniform D-I prior (25 min each).
        let roster = vec![pr(300.0, None, None)];
        let d1 = d1_prior();
        let dist = build_projected_class_minutes(&roster, &d1);
        for cls in d1.keys() {
            let got = dist.get(cls).copied().unwrap_or(0.0);
            assert!(
                (got - 25.0).abs() < 1e-9,
                "expected 25.0 minutes per class under uniform prior, got {got} for {cls}",
            );
        }
    }

    #[test]
    fn projected_minutes_skips_secondary_when_equal_to_primary() {
        // Defensive: if the archetype model emits identical primary +
        // secondary, the secondary 0.5× contribution should be skipped
        // (same dedup behavior compute_fit_score uses on the candidate).
        let roster = vec![pr(200.0, Some("Wizard"), Some("Wizard"))];
        let d1 = d1_prior();
        let dist = build_projected_class_minutes(&roster, &d1);
        // Only the primary 1.0× contribution should count.
        assert!((dist.get("Wizard").copied().unwrap_or(0.0) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn self_exclusion_unwinds_to_empty_when_candidate_is_sole_member() {
        // A Wizard candidate at a team where the only Wizard minutes
        // *are* this candidate. After self-exclusion the Wizard bucket
        // empties, so the fit reads as the maximum gap — same as v1's
        // "completely missing class" case (raw ≈ 1.0, StrongFit).
        let candidate_min = 800.0;
        let team_minutes: HashMap<String, f64> = [
            ("Wizard".to_string(), 800.0), // = candidate's own contribution
            ("Bard".to_string(), 600.0),
            ("Cleric".to_string(), 400.0),
        ]
        .into_iter()
        .collect();
        let d1: HashMap<String, f64> = [
            ("Wizard".to_string(), 0.10),
            ("Bard".to_string(), 0.10),
            ("Cleric".to_string(), 0.10),
        ]
        .into_iter()
        .collect();
        let fit = fit_score_against_projected("Wizard", None, candidate_min, &team_minutes, &d1);
        // Wizard bucket now 0 → index 0 (missing class) → +1.0 signal.
        assert!(fit.raw > 0.65, "expected strong fit, got {}", fit.raw);
        assert_eq!(fit.tier, FitTier::StrongFit);
        assert!(fit.label.contains("Wizard"));
    }

    #[test]
    fn self_exclusion_leaves_redundancy_when_other_wizards_remain() {
        // 4 Wizards on the roster, one of them is the candidate. After
        // peeling out the candidate's contribution the other 3 still
        // make Wizard heavily over-indexed → SomeRedundancy / Redundant.
        let candidate_min = 800.0;
        let team_minutes: HashMap<String, f64> = [
            ("Wizard".to_string(), 3200.0), // 4 × 800
            ("Bard".to_string(), 200.0),
        ]
        .into_iter()
        .collect();
        let d1: HashMap<String, f64> = [("Wizard".to_string(), 0.10), ("Bard".to_string(), 0.10)]
            .into_iter()
            .collect();
        let fit = fit_score_against_projected("Wizard", None, candidate_min, &team_minutes, &d1);
        // After subtraction: Wizard 2400, Bard 200 → team_share Wizard
        // ≈ 0.923, index ≈ 9.23 → saturated redundancy.
        assert!(fit.raw < -0.4, "expected redundancy, got {}", fit.raw);
        assert!(
            matches!(fit.tier, FitTier::Redundant | FitTier::SomeRedundancy),
            "tier was {:?}",
            fit.tier
        );
    }

    #[test]
    fn self_exclusion_with_zero_minutes_is_noop() {
        // Defensive: a candidate we couldn't resolve to a projected
        // PlayerRow (no rank-slot MPG known) passes candidate_minutes=0.
        // The score should equal what compute_fit_score returns for the
        // raw distribution.
        let team_minutes: HashMap<String, f64> =
            [("Wizard".to_string(), 1000.0), ("Bard".to_string(), 500.0)]
                .into_iter()
                .collect();
        let d1: HashMap<String, f64> = [("Wizard".to_string(), 0.10), ("Bard".to_string(), 0.10)]
            .into_iter()
            .collect();
        let fit_v2 = fit_score_against_projected("Cleric", None, 0.0, &team_minutes, &d1);
        // Cleric is absent → 100% gap → strong fit.
        assert!(fit_v2.raw > 0.65);
        assert_eq!(fit_v2.tier, FitTier::StrongFit);
    }

    #[test]
    fn d1_prior_dispersal_does_not_bias_one_class() {
        // Regression: a team whose projected roster is mostly recruits
        // shouldn't score later transfers (with known archetype) as
        // having "fills missing X" against every class. With a uniform
        // D-I prior, the recruits add equal mass everywhere → no class
        // looks systematically gapped purely by recruit dispersal.
        let roster: Vec<PlayerRow> = (0..5).map(|_| pr(600.0, None, None)).collect();
        let d1 = d1_prior();
        let class_min = build_projected_class_minutes(&roster, &d1);
        let total: f64 = class_min.values().sum();
        // Every class should be at the league-average share (1/12).
        for cls in d1.keys() {
            let share = class_min.get(cls).copied().unwrap_or(0.0) / total;
            assert!(
                (share - 1.0 / 12.0).abs() < 1e-9,
                "class {cls} share is {share}, expected 1/12",
            );
        }
    }
}
