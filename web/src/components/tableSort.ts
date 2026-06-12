export type SortDir = 'asc' | 'desc';

// AG Grid numeric comparator pinning null/blank cells to the BOTTOM in both
// sort directions (below the negative values). AG Grid negates comparator
// results on descending sort, so a naive "nulls last" (+1) floats blanks to
// the top exactly when sorting best-to-worst; the isDescending argument lets
// us pre-invert. Use for any sortable numeric column whose blanks mean
// "no data" rather than zero (CamPom, Adj On/Off, projections).
export function agNullsBottom(
  a: number | null | undefined,
  b: number | null | undefined,
  _nodeA?: unknown,
  _nodeB?: unknown,
  isDescending?: boolean,
): number {
  if (a == null && b == null) return 0;
  if (a == null) return isDescending ? -1 : 1;
  if (b == null) return isDescending ? 1 : -1;
  return a - b;
}

export function compareValues(a: unknown, b: unknown, dir: SortDir): number {
  if (a == null && b == null) return 0;
  if (a == null) return 1;
  if (b == null) return -1;
  if (typeof a === 'number' && typeof b === 'number') {
    return dir === 'asc' ? a - b : b - a;
  }
  const as = String(a).toLocaleLowerCase();
  const bs = String(b).toLocaleLowerCase();
  return dir === 'asc' ? as.localeCompare(bs) : bs.localeCompare(as);
}
