// Shared shot-efficiency gradient used by per-player `ShotDietCourt`
// and team-aggregate `TeamShotDiet`. Lives in its own non-component
// file so the React refresh plugin doesn't trip on mixed component /
// non-component exports.

/// Saturated red → yellow → green gradient mapping a normalized
/// percentile (0..1) to a Tailwind-ish CSS color. Below 0.5 walks
/// red→yellow; above 0.5 walks yellow→green. Returns muted slate
/// for missing input so unscored zones don't shout.
export const efficiencyColor = (pctile: number | null | undefined) => {
  if (pctile == null) return '#4b5563';
  const p = Math.max(0, Math.min(1, pctile));
  if (p <= 0.5) {
    const t = p / 0.5;
    const r = Math.round(239 + (250 - 239) * t);
    const g = Math.round(68 + (204 - 68) * t);
    const b = Math.round(68 + (21 - 68) * t);
    return `rgb(${r},${g},${b})`;
  }
  const t = (p - 0.5) / 0.5;
  const r = Math.round(250 + (34 - 250) * t);
  const g = Math.round(204 + (211 - 204) * t);
  const b = Math.round(21 + (103 - 21) * t);
  return `rgb(${r},${g},${b})`;
};
