/// Direction & flag lookups used by the Keys-to-the-Game panel.
///
/// Why this still exists after TreeSHAP: SHAP signs are the *model's*
/// answer ("which team did the model attribute this feature toward").
/// The keys panel is a user-facing stats narrative, so it names the
/// leader by *data* direction (which team has the better raw stat) and
/// uses TreeSHAP magnitudes only for importance weighting. The two can
/// disagree on legitimate non-monotonic features — e.g. Purdue vs
/// Michigan defense, where the data clearly favors Michigan but the
/// model can still attribute the feature toward Purdue. Without this
/// lookup the panel would mislabel the leader on those cases.

/// Features where a NEGATIVE diff value favors the home team — i.e. stats
/// where lower is better. Defensive stats are already sign-flipped in
/// `crates/cstat-core/src/features.rs::build_game_features`, so they
/// don't need to appear here.
const INVERTED_FEATURES: ReadonlySet<string> = new Set([
  'diff_turnover_pct', // offensive TOV%; lower = better
  'diff_w_topg', // turnovers per game; lower = better
  'diff_w_tov_pct', // roster TOV rate; lower = better
  // Player-minutes stddev. Lower = more balanced rotation = better depth
  // (KenPom-style "bench minutes" intuition). Star-heavy lineups can win
  // with high stddev too, but the keys panel uses the simpler convention;
  // the model still picks up matchup-specific nuance via SHAP magnitude.
  'diff_minutes_stddev',
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
