/// Per-feature flags used by the Keys-to-the-Game panel.
///
/// The `homeAdvantageSign` data-direction lookup that used to live here
/// retired when TreeSHAP replaced ablation-based attribution: SHAP
/// contributions carry the model's authoritative direction natively, so
/// the keys panel reads signs straight off `FeatureContribution.contribution`.

/// Features that aren't diffs at all — 0/1 indicators that don't favor
/// either team directionally. Used to switch the headline phrasing from
/// "(gap of …)" to "(home court factor)" / "(conference matchup)" etc.
export const FLAG_FEATURES: ReadonlySet<string> = new Set(['venue', 'is_conference_game']);
