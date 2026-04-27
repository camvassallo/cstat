// Red → neutral → green gradient for a percentile in [0, 1]. Used by stat
// cells to colour values by how strong the player's rank is. Returns a
// muted gray when the percentile is null.
export function pctileTextColor(p: number | null | undefined): string {
  if (p == null) return '#6b7280'; // gray-500
  // Defensive clamp: PERCENT_RANK is bounded to [0, 1], but if a future
  // caller passes anything outside that range the lerp produces out-of-gamut
  // RGB. Clamp at the boundary so the gradient is always well-defined.
  const clamped = Math.max(0, Math.min(1, p));
  const red = [248, 113, 113];
  const mid = [229, 231, 235];
  const green = [74, 222, 128];
  const lerp = (a: number[], b: number[], t: number) =>
    a.map((av, i) => Math.round(av + (b[i] - av) * t));
  const c =
    clamped <= 0.5
      ? lerp(red, mid, clamped / 0.5)
      : lerp(mid, green, (clamped - 0.5) / 0.5);
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}
