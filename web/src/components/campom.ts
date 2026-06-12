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

/// Diverging red→gray→green for the O/D halves (shared gradient machinery
/// from onoff.ts). Saturation tuned to each half's rotation-pool spread —
/// O p05/p95 ≈ −4.6/+6.7 (scale 7), D ≈ −2.9/+3.8 (scale 4) — so a clearly
/// good half reads clearly green without every starter pinning the scale.
export function campomHalfColor(v: number | null | undefined,
                                side: "o" | "d"): string {
  return onOffColor(v, side === "o" ? 7 : 4);
}
