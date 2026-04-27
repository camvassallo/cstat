// Defensive clamp: PERCENT_RANK is bounded to [0, 1], but if a future
// caller passes anything outside that range the lerp produces out-of-gamut
// RGB. Clamp at the boundary so the gradient is always well-defined.
function lerp3(a: number[], b: number[], t: number) {
  return a.map((av, i) => Math.round(av + (b[i] - av) * t));
}

// Red → neutral → green gradient. The neutral midpoint blends into the
// page background so dense tables (Players, Roster) don't read as a
// Christmas tree — only outliers stand out.
export function pctileTextColor(p: number | null | undefined): string {
  if (p == null) return '#6b7280'; // gray-500
  const clamped = Math.max(0, Math.min(1, p));
  const red = [248, 113, 113];
  const mid = [229, 231, 235];
  const green = [74, 222, 128];
  const c =
    clamped <= 0.5
      ? lerp3(red, mid, clamped / 0.5)
      : lerp3(mid, green, (clamped - 0.5) / 0.5);
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}

// Red → yellow → green gradient. Same shape as `pctileTextColor` but with
// a saturated midpoint, so middle-of-pack values still read as colored.
// Use this for sparse pages (e.g. landing rankings) where one column wants
// to carry the visual weight; avoid on dense tables where every column gets
// a tint and the saturation would compete.
export function pctileTextColorVivid(p: number | null | undefined): string {
  if (p == null) return '#6b7280'; // gray-500
  const clamped = Math.max(0, Math.min(1, p));
  const red = [248, 113, 113];
  const yellow = [250, 204, 21]; // amber-400
  const green = [74, 222, 128];
  const c =
    clamped <= 0.5
      ? lerp3(red, yellow, clamped / 0.5)
      : lerp3(yellow, green, (clamped - 0.5) / 0.5);
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}
