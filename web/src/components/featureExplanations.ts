/// Per-feature direction lookups used by the Keys-to-the-Game panel to
/// translate raw diff signs into a data-faithful "which team has the edge"
/// axis, avoiding the sign artifacts that ablation-based attribution can
/// produce. Once TreeSHAP lands (ROADMAP §4b) the model's signed
/// contribution becomes the authoritative direction and these helpers can
/// retire.
///
/// Tooltips that referenced this file's old `FEATURE_EXPLANATIONS` and
/// `GROUP_EXPLANATIONS` dictionaries were removed when the keys + matchup
/// panels became self-explanatory; the dictionaries went with them.

/// Features where a NEGATIVE diff value favors the home team — i.e. stats
/// where lower is better for the offense (turnovers committed). Defensive
/// stats are already sign-flipped in
/// `crates/cstat-core/src/features.rs::build_game_features`, so they don't
/// need to appear here.
const INVERTED_FEATURES: ReadonlySet<string> = new Set([
  'diff_turnover_pct', // offensive TOV%; lower = better
  'diff_w_topg', // turnovers per game; lower = better
  'diff_w_tov_pct', // roster TOV rate; lower = better
]);

/// Features that aren't diffs at all — 0/1 indicators that don't favor
/// either team directionally.
export const FLAG_FEATURES: ReadonlySet<string> = new Set(['venue', 'is_conference_game']);

/// Returns `+1` if a positive feature value favors the home team,
/// `-1` if a negative value favors home, `0` if the feature is a flag
/// indicator with no directional advantage. Drives the keys panel's
/// data-faithful team attribution.
export function homeAdvantageSign(name: string, value: number): number {
  if (FLAG_FEATURES.has(name)) return 0;
  if (value === 0) return 0;
  const sign = value > 0 ? 1 : -1;
  return INVERTED_FEATURES.has(name) ? -sign : sign;
}
