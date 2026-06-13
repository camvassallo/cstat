// CamPom presentation helpers. The metric itself is `cam_gbpm_v3_psos` from
// the compute pipeline (see ROADMAP §4f and docs/campom_methodology.md). Here
// we just turn the raw score into the user-facing tier and color.

import { onOffColor } from "./onoff";

export type CampomTier =
  | "Elite"
  | "All-Conference"
  | "Quality starter"
  | "Rotation"
  | "Replacement"
  | "Below replacement";

export function campomTier(score: number | null | undefined): CampomTier | null {
  if (score == null) return null;
  if (score >= 20) return "Elite";
  if (score >= 15) return "All-Conference";
  if (score >= 10) return "Quality starter";
  if (score >= 5) return "Rotation";
  if (score >= 0) return "Replacement";
  return "Below replacement";
}

// Tailwind-ish class strings; same palette as the existing percentile chips.
export function campomTierColor(tier: CampomTier | null): string {
  switch (tier) {
    case "Elite":             return "bg-emerald-500/20 text-emerald-300 border-emerald-500/40";
    case "All-Conference":    return "bg-sky-500/20 text-sky-300 border-sky-500/40";
    case "Quality starter":   return "bg-blue-500/20 text-blue-300 border-blue-500/40";
    case "Rotation":          return "bg-slate-500/20 text-slate-300 border-slate-500/40";
    case "Replacement":       return "bg-amber-500/20 text-amber-300 border-amber-500/40";
    case "Below replacement": return "bg-rose-500/20 text-rose-300 border-rose-500/40";
    default:                  return "bg-slate-700/40 text-slate-400 border-slate-600/40";
  }
}

// --- O/D decomposition helpers -------------------------------------------
// cam_o + cam_d = campom, both on the same per-100 scale; cam_d is
// positive-good (defensive value added). Values arrive pre-gated by the API
// (±30 sanity envelope — a regression guard; the compute-side SOS allocation
// is bounded since the 2026-06-12 magnitude-share fix, so nulls here mean
// genuinely unusable rows, never a clipped fake number).

const signed = (v: number) => `${v > 0 ? "+" : ""}${v.toFixed(1)}`;

/// Compact inline split, e.g. "O +8.1 / D +4.2" — null when either half is
/// gated (they're only meaningful together).
export function campomSplit(o: number | null | undefined,
                            d: number | null | undefined): string | null {
  if (o == null || d == null) return null;
  return `O ${signed(o)} / D ${signed(d)}`;
}

/// Tooltip for CamPom cells: tier line plus the O/D split when available.
export function campomTitle(campom: number | null | undefined,
                            o?: number | null,
                            d?: number | null): string {
  const tier = campomTier(campom);
  let s = tier ? `${tier}.` : "";
  const split = campomSplit(o, d);
  if (split) {
    s += `${s ? " " : ""}Offense ${signed(o!)} / defense ${signed(d!)} per 100` +
      " (split hidden where numerically unstable).";
  }
  return s;
}

// Per-half saturation scale, tuned to each half's rotation-pool spread —
// O p05/p95 ≈ −4.6/+6.7 (scale 7), D ≈ −2.9/+3.8 (scale 4) — so a clearly
// good half reads clearly green/full without every starter pinning the scale.
// Shared by the color gradient and the diverging-bar fraction so the two
// saturate together.
const HALF_SCALE = { o: 7, d: 4 } as const;

/// Diverging red→gray→green for the O/D halves (shared gradient machinery
/// from onoff.ts).
export function campomHalfColor(v: number | null | undefined,
                                side: "o" | "d"): string {
  return onOffColor(v, HALF_SCALE[side]);
}

// Approximate D-I distribution of each half, fit to the documented rotation-
// pool p05/p95 (O: −4.6/+6.7, D: −2.9/+3.8) as a normal: mean = midpoint,
// sigma = span / (2 × 1.6449). The compute pipeline doesn't materialize a
// PERCENT_RANK for the O/D halves the way it does for box/rate stats, so this
// is a MODELED percentile (parametric estimate), not an exact rank — used only
// to drive the CPO/CPD percentile bars on the compare screen.
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

/// Modeled D-I percentile in [0, 1] for a CPO/CPD half, so the value can drive
/// a left-fill percentile bar consistent with the other compare-table rows.
/// Positive = good for both halves (CPD is positive-good). Null where gated.
/// Approximate — see `HALF_DIST`.
export function campomHalfPctile(v: number | null | undefined,
                                 side: "o" | "d"): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  const { mean, sigma } = HALF_DIST[side];
  const z = (v - mean) / sigma;
  return Math.max(0, Math.min(1, 0.5 * (1 + erf(z / Math.SQRT2))));
}
