// Mixed-scale percentage formatters used across stat tables.
//
// Some pss / torvik fields arrive as fractions (0–1) and render with ×100
// scaling (AST%, TOV%, USG%, TS%, eFG%, FT Rate). Others already arrive as
// percent points (0–100) and render as-is (ORB%, DRB%, STL%, BLK%). Keeping
// both helpers in one module so the convention has a single source of truth.

export const fracPct = (v: number | null | undefined) =>
  v != null ? (v * 100).toFixed(1) : '—';

export const pointPct = (v: number | null | undefined) =>
  v != null ? v.toFixed(1) : '—';

const MONTH_ABBREV = [
  'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
  'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
];

/// Format an ISO date (YYYY-MM-DD) as "Mon D" (e.g. "Feb 21"). Used by the
/// score ticker and Previous Matchups card so date display stays consistent
/// across surfaces. Leaves the input untouched if it doesn't match the
/// expected shape.
export function shortDate(iso: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
  if (!m) return iso;
  return `${MONTH_ABBREV[Number(m[2]) - 1]} ${Number(m[3])}`;
}

/// Format a projected matchup score as `Winner W — Loser L`, with the
/// winner first regardless of which side hosted. Used by Predict.tsx,
/// ScoreTicker upcoming tiles, and TeamDetail Projected column so all
/// three surfaces read identically.
///
/// Caveat: totals model has backtest MAE ~13.6 (vs ~8.2 for margin), so
/// these are KenPom-style approximations, not Vegas-precision picks.
export function formatProjectedScore(
  homeScore: number,
  awayScore: number,
  homeName: string,
  awayName: string,
): string {
  if (homeScore >= awayScore) {
    return `${homeName} ${homeScore} — ${awayName} ${awayScore}`;
  }
  return `${awayName} ${awayScore} — ${homeName} ${homeScore}`;
}

/// Compact form of `formatProjectedScore` for tight cells (TeamDetail
/// Projected column). Always orders by team perspective: returns
/// `team-team_score, opp-opp_score` separated by an em-dash. Caller's
/// own perspective (the team whose schedule this is) goes first.
export function formatScorePair(teamScore: number, oppScore: number): string {
  return `${teamScore}–${oppScore}`;
}
