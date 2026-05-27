//! Archetype-based roster fit scoring.
//!
//! Layers on top of the Identity/Gaps index (`queries::get_team_archetype_index`)
//! that already powers the TeamDetail page. For a candidate player with a
//! known primary (and optionally secondary) archetype class, this module
//! scores how well they fit a given destination roster — positive when they
//! fill a class the team is underweighted in, negative when they pile onto a
//! class the team is already overweighted in.
//!
//! Baseline is the destination's **current-season** archetype distribution.
//! That matches what users see on TeamDetail and avoids the recruit-archetype
//! gap (synthesized freshman PlayerRows carry `primary_class: None`) that
//! would bias any projected-roster distribution. Future v2 can hook into
//! projected rosters once recruit archetype assignment is solved.

use serde::Serialize;

use crate::queries::ArchetypeShare;

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
/// Priority ordering when both primary and secondary classes have notable
/// indices: primary always wins (it's the heavier-weighted signal) unless
/// the primary is near-neutral and the secondary has a strong story to
/// tell. "Fills missing X" is reserved for genuinely absent classes
/// (index < 0.15) since that reads differently from "underweighted."
fn build_label(
    primary: &str,
    primary_index: f64,
    secondary: Option<&str>,
    secondary_index: Option<f64>,
    raw: f64,
) -> String {
    let primary_strong_gap = primary_index < 0.70;
    let primary_strong_red = primary_index > 1.30;

    if raw >= 0.10 && primary_strong_gap {
        return if primary_index < 0.15 {
            format!("Fills missing {primary}")
        } else {
            format!("Fills {primary} gap")
        };
    }
    if raw <= -0.10 && primary_strong_red {
        return format!("Stacks {primary} rotation");
    }

    // Primary near-neutral — check secondary for a story worth surfacing.
    if let (Some(sec), Some(sec_idx)) = (secondary, secondary_index) {
        if raw >= 0.10 && sec_idx < 0.70 {
            return if sec_idx < 0.15 {
                format!("Secondary fills missing {sec}")
            } else {
                format!("Secondary fills {sec} gap")
            };
        }
        if raw <= -0.10 && sec_idx > 1.30 {
            return format!("Secondary stacks {sec} rotation");
        }
    }

    "Roster-neutral".to_string()
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
}
