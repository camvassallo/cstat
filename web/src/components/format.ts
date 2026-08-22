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

/// Format a height in inches as feet-and-inches (`6'11"`). Returns null when
/// the height is missing, so callers can drop the field rather than print a
/// placeholder.
///
/// Four other copies of this two-line function already exist — `PlayerDetail`,
/// `PlayerProgression`, `PlayerCompare` and `portle`'s `fmtHeight` — and they
/// disagree in small ways (portle rounds the inches remainder, the rest floor
/// it; the null handling differs three ways). This is the shared one for new
/// callers; converging the existing four is a follow-up (#315) rather than a
/// drive-by, since two of them feed snapshot-tested surfaces.
///
/// Rounds the TOTAL inches before splitting, rather than rounding the
/// remainder the way `portle` does. Rounding the remainder can carry past 11
/// and print a height that does not exist: 71.6 floors to 5 feet but rounds
/// the remainder to 12, giving `5'12"` instead of `6'0"`. `players.height_inches`
/// is an INT column so nothing hits that today, but this is the copy #315
/// points the other four at, and one of them is fed from a different source.
export function heightString(inches: number | null | undefined): string | null {
  if (inches == null || !Number.isFinite(inches)) return null;
  const total = Math.round(inches);
  return `${Math.floor(total / 12)}'${total % 12}"`;
}
