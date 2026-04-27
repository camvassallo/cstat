// CamPom presentation helpers. The metric itself is `cam_gbpm_v3_psos` from
// the compute pipeline (see ROADMAP §4f and docs/campom_methodology.md). Here
// we just turn the raw score into the user-facing tier and color.

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

export function formatCampom(score: number | null | undefined): string {
  if (score == null) return "—";
  return score.toFixed(1);
}

export function formatCampomPct(pct: number | null | undefined): string {
  if (pct == null) return "—";
  return `${(pct * 100).toFixed(0)}`;
}
