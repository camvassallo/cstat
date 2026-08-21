// The site's canonical quality scale: red → orange → yellow → blue → green.
//
// This ramp already drove the PlayerDetail percentile bars; it now also drives
// the CAM family (tier chips, CAMO/CAMD halves) so the headline metric is
// colored the same way as everything else on the page.
//
// The scale is BANDED, not continuously interpolated, and deliberately so:
// yellow and blue sit near-opposite on the color wheel, so any blend between
// them — in RGB or in a perceptual space — passes through a muddy olive-gray.
// Five discrete steps read cleanly and match how the percentile bars have
// always looked.
//
// Two variants of each band: `-500` for filled bars and solid chips (read
// against the page background), `-400` for text and thin glyphs (which need
// more lift against the dark surface).

/** Band fills — Tailwind red/orange/yellow/blue/green-500, worst → best. */
export const BAND_FILL = ['#ef4444', '#f97316', '#eab308', '#3b82f6', '#22c55e'] as const;

/** Band text colors — the `-400` variants, for numerals on a dark surface. */
export const BAND_TEXT = ['#f87171', '#fb923c', '#facc15', '#60a5fa', '#4ade80'] as const;

/** Tailwind class names for the filled bars, parallel to `BAND_FILL`. */
export const BAND_BAR_CLASS = [
  'bg-red-500',
  'bg-orange-500',
  'bg-yellow-500',
  'bg-blue-500',
  'bg-green-500',
] as const;

/** Tinted chip classes (fill + text + border) per band, for inline badges. */
export const BAND_CHIP_CLASS = [
  'bg-red-500/20 text-red-300 border-red-500/40',
  'bg-orange-500/20 text-orange-300 border-orange-500/40',
  'bg-yellow-500/20 text-yellow-300 border-yellow-500/40',
  'bg-blue-500/20 text-blue-300 border-blue-500/40',
  'bg-green-500/20 text-green-300 border-green-500/40',
] as const;

/** Neutral treatments for a missing value, so every surface renders "no data"
 *  identically rather than each picking its own gray. */
export const BAND_EMPTY_TEXT = '#6b7280'; // gray-500
export const BAND_EMPTY_BAR_CLASS = 'bg-gray-600';
export const BAND_EMPTY_CHIP_CLASS = 'bg-slate-700/40 text-slate-400 border-slate-600/40';

/** Band index (0 = worst … 4 = best) for a normalized 0–1 score. Cut points
 *  are the 20/40/60/80 thresholds the percentile bars have always used. */
export function bandIndex(t: number): 0 | 1 | 2 | 3 | 4 {
  const c = Math.max(0, Math.min(1, t));
  if (c >= 0.8) return 4;
  if (c >= 0.6) return 3;
  if (c >= 0.4) return 2;
  if (c >= 0.2) return 1;
  return 0;
}

/** Text color for a normalized 0–1 score. */
export function bandTextColor(t: number | null | undefined): string {
  if (t == null || !Number.isFinite(t)) return BAND_EMPTY_TEXT;
  return BAND_TEXT[bandIndex(t)];
}

/** Bar class for a normalized 0–1 score. */
export function bandBarClass(t: number | null | undefined): string {
  if (t == null || !Number.isFinite(t)) return BAND_EMPTY_BAR_CLASS;
  return BAND_BAR_CLASS[bandIndex(t)];
}

/** Chip classes for a normalized 0–1 score. */
export function bandChipClass(t: number | null | undefined): string {
  if (t == null || !Number.isFinite(t)) return BAND_EMPTY_CHIP_CLASS;
  return BAND_CHIP_CLASS[bandIndex(t)];
}

/** Map a signed value on a ±`scale` axis into the 0–1 band domain, so a
 *  diverging metric (CAMO/CAMD, on/off swing) lands on the same five colors
 *  as a percentile. 0 sits at the middle (yellow) band. */
export function signedToUnit(v: number, scale: number): number {
  return 0.5 + 0.5 * Math.max(-1, Math.min(1, v / scale));
}

/** Text color for a signed value saturating at ±`scale`. */
export function signedBandTextColor(
  v: number | null | undefined,
  scale: number,
): string {
  if (v == null || !Number.isFinite(v)) return BAND_EMPTY_TEXT;
  return BAND_TEXT[bandIndex(signedToUnit(v, scale))];
}
