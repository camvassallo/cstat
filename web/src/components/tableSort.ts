export type SortDir = 'asc' | 'desc';

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
