// Shared formatting for Coach-Above-Expectation (CAE) values, used by the
// /coaches leaderboard, the coach detail page, and the TeamDetail coach card.
//
// CAE is in AdjEM points (actual team AdjEM − roster-only projection). The
// HEADLINE value everywhere is the EB-shrunk rating; thin tenures shrink toward
// 0, so the scale below is intentionally tight — a shrunk +3 is already strong.

/** Signed, fixed-decimal CAE string: `+2.1`, `-0.8`, `—` for null. */
export function fmtCae(v: number | null | undefined, d = 1): string {
  if (v == null) return '—';
  const s = v.toFixed(d);
  // Values that round to zero render unsigned — avoids a stray "-0.0" on the
  // many near-zero CAE grades (e.g. -0.04 → "0.0", not "-0.0").
  if (Number(s) === 0) return (0).toFixed(d);
  return v > 0 ? `+${s}` : s;
}

/** Diverging green(over)→gray(neutral)→red(under) color on a shrunk CAE value.
 *  Saturates around ±4 AdjEM, the practical range of the shrunk rating. */
export function caeColor(v: number | null | undefined): string {
  if (v == null) return '#9ca3af'; // gray-400
  const t = Math.max(-1, Math.min(1, v / 4));
  if (t >= 0) {
    // gray-200 (#e5e7eb) → green-400 (#4ade80)
    const r = Math.round(0xe5 + (0x4a - 0xe5) * t);
    const g = Math.round(0xe7 + (0xde - 0xe7) * t);
    const b = Math.round(0xeb + (0x80 - 0xeb) * t);
    return `rgb(${r}, ${g}, ${b})`;
  }
  // gray-200 (#e5e7eb) → red-400 (#f87171)
  const a = -t;
  const r = Math.round(0xe5 + (0xf8 - 0xe5) * a);
  const g = Math.round(0xe7 + (0x71 - 0xe7) * a);
  const b = Math.round(0xeb + (0x71 - 0xeb) * a);
  return `rgb(${r}, ${g}, ${b})`;
}

/** Inclusive tenure span label: "2022–2026", or "2024" for a single season. */
export function tenureSpan(first: number, last: number): string {
  return first === last ? `${first}` : `${first}–${last}`;
}
