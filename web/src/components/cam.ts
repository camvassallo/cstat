// CAM presentation helpers. CAM is Camalytics' player-value metric; the
// underlying column is `cam_gbpm_v3_psos` from the compute pipeline. The O/D
// halves display as CAMO and CAMD. Here we turn the raw score into the
// user-facing tier, color, and tooltip.
//
// Coloring runs on the shared site scale (red → orange → yellow → blue →
// green) so CAM reads the same way as the percentile bars and every other
// graded surface — see `scale.ts`.

import {
  BAND_CHIP_CLASS,
  BAND_EMPTY_CHIP_CLASS,
  bandIndex,
  signedBandTextColor,
  signedToUnit,
} from './scale';

export type CamTier =
  | 'Elite'
  | 'All-Conference'
  | 'Quality starter'
  | 'Rotation'
  | 'Replacement'
  | 'Below replacement';

export function camTier(score: number | null | undefined): CamTier | null {
  if (score == null) return null;
  if (score >= 20) return 'Elite';
  if (score >= 15) return 'All-Conference';
  if (score >= 10) return 'Quality starter';
  if (score >= 5) return 'Rotation';
  if (score >= 0) return 'Replacement';
  return 'Below replacement';
}

// Six tiers over the five-color scale. The bottom five map one-to-one onto the
// bands (red → green); Elite is separated from All-Conference by emphasis
// rather than a sixth hue — a solid green fill instead of a tint — so the top
// tier pops in a dense table without leaving the site's color vocabulary.
const TIER_BAND: Record<CamTier, number> = {
  'Below replacement': 0,
  Replacement: 1,
  Rotation: 2,
  'Quality starter': 3,
  'All-Conference': 4,
  Elite: 4,
};

export function camTierColor(tier: CamTier | null): string {
  if (tier == null) return BAND_EMPTY_CHIP_CLASS;
  if (tier === 'Elite') return 'bg-green-500 text-gray-950 border-green-400 font-semibold';
  return BAND_CHIP_CLASS[TIER_BAND[tier]];
}

// --- O/D decomposition helpers -------------------------------------------
// CAMO + CAMD = CAM, both on the same per-100 scale; CAMD is positive-good
// (defensive value added). Values arrive pre-gated by the API (±30 sanity
// envelope — a regression guard; the compute-side SOS allocation is bounded
// since the 2026-06-12 magnitude-share fix, so nulls here mean genuinely
// unusable rows, never a clipped fake number).

const signed = (v: number) => `${v > 0 ? '+' : ''}${v.toFixed(1)}`;

/// Compact inline split, e.g. "O +8.1 / D +4.2" — null when either half is
/// gated (they're only meaningful together).
export function camSplit(
  o: number | null | undefined,
  d: number | null | undefined,
): string | null {
  if (o == null || d == null) return null;
  return `O ${signed(o)} / D ${signed(d)}`;
}

/// Tooltip for CAM cells: tier line plus the O/D split when available.
export function camTitle(
  cam: number | null | undefined,
  o?: number | null,
  d?: number | null,
): string {
  const tier = camTier(cam);
  let s = tier ? `${tier}.` : '';
  if (camSplit(o, d)) {
    s += `${s ? ' ' : ''}CAMO ${signed(o!)} / CAMD ${signed(d!)} per 100.`;
  }
  return s;
}

// Per-half saturation scale, tuned to each half's rotation-pool spread —
// O p05/p95 ≈ −4.6/+6.7 (scale 7), D ≈ −2.9/+3.8 (scale 4) — so a clearly
// good half reads clearly green without every starter pinning the scale.
// Shared by the color bands and the diverging-bar fraction so the two
// saturate together.
const HALF_SCALE = { o: 7, d: 4 } as const;

/// CAMO / CAMD color on the shared five-band site scale.
export function camHalfColor(
  v: number | null | undefined,
  side: 'o' | 'd',
): string {
  return signedBandTextColor(v, HALF_SCALE[side]);
}

/// Band index (0–4) for a CAMO/CAMD half — for surfaces that need the bucket
/// itself (bar fills, chips) rather than a text color.
export function camHalfBand(v: number, side: 'o' | 'd'): number {
  return bandIndex(signedToUnit(v, HALF_SCALE[side]));
}

// Approximate D-I distribution of each half, fit to the documented rotation-
// pool p05/p95 (O: −4.6/+6.7, D: −2.9/+3.8) as a normal: mean = midpoint,
// sigma = span / (2 × 1.6449). The compute pipeline doesn't materialize a
// PERCENT_RANK for the O/D halves the way it does for box/rate stats, so this
// is a MODELED percentile (parametric estimate), not an exact rank — used only
// to drive the CAMO/CAMD percentile bars on the compare screen.
const HALF_DIST = {
  o: { mean: 1.05, sigma: 3.435 },
  d: { mean: 0.45, sigma: 2.036 },
} as const;

// erf via Abramowitz & Stegun 7.1.26 (max abs error ~1.5e-7) — enough for a bar.
function erf(x: number): number {
  const sign = x < 0 ? -1 : 1;
  const ax = Math.abs(x);
  const t = 1 / (1 + 0.3275911 * ax);
  const y =
    1 -
    ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t +
      0.254829592) *
      t *
      Math.exp(-ax * ax);
  return sign * y;
}

/// Modeled D-I percentile in [0, 1] for a CAMO/CAMD half, so the value can
/// drive a left-fill percentile bar consistent with the other compare-table
/// rows. Positive = good for both halves (CAMD is positive-good). Null where
/// gated. Approximate — see `HALF_DIST`.
export function camHalfPctile(
  v: number | null | undefined,
  side: 'o' | 'd',
): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  const { mean, sigma } = HALF_DIST[side];
  const z = (v - mean) / sigma;
  return Math.max(0, Math.min(1, 0.5 * (1 + erf(z / Math.SQRT2))));
}
