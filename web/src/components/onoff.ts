// Shared on/off rendering helpers (PBP item A). Used by the TeamDetail roster
// table and the Players / Transfers ranking grids so the net-swing color,
// formatting, and tooltip stay identical across surfaces.

/// The on/off fields a row needs to render the swing + its tooltip. Every
/// consumer (RosterEntry, PlayerRow, EnrichedTransfer) carries these.
export interface OnOffFields {
  net_on_off: number | null;
  on_net_rtg: number | null;
  off_net_rtg: number | null;
  on_off_source: string | null;
  on_off_off_poss: number | null;
}

/// Net on/off swing → red (negative) / gray (≈0) / green (positive), saturating
/// around ±12 per-100 (a large rotation swing). Mirrors the ValueWithPctile
/// gray→green / gray→red gradient used elsewhere on the roster table.
export function onOffColor(net: number | null | undefined): string {
  if (net == null) return '#6b7280'; // gray-500
  const t = Math.max(-1, Math.min(1, net / 12));
  const gray = [229, 231, 235];
  const target = t >= 0 ? [74, 222, 128] : [248, 113, 113];
  const lerp = (a: number, b: number) => Math.round(a + (b - a) * Math.abs(t));
  return `rgb(${lerp(gray[0], target[0])}, ${lerp(gray[1], target[1])}, ${lerp(gray[2], target[2])})`;
}

/// Signed one-decimal rating, "—" for null.
export const signedRtg = (v: number | null | undefined) =>
  v == null ? '—' : `${v > 0 ? '+' : ''}${v.toFixed(1)}`;

/// Tooltip: on vs off net rating per 100 poss + small-sample / replay caveats.
export function onOffTitle(p: OnOffFields): string {
  let s = `Team net rating per 100 poss: on ${signedRtg(p.on_net_rtg)} vs off ${signedRtg(p.off_net_rtg)}.`;
  if (p.on_off_off_poss != null && p.on_off_off_poss < 100) s += ' Small off-court sample.';
  if (p.on_off_source === 'replay') s += ' Lineups replay-estimated (~86%).';
  return s;
}
