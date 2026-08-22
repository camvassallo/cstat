// Slot tokens for the Compare page's `?ids=` param.
//
// A slot is either a bare `<uuid>` — rendered in whatever season the request
// carries — or `<uuid>@<year>`, which pins that one column to its own year.
// `/api/players/compare` has accepted both since #291; this module is the
// frontend half, and it is a plain string library on purpose: the parsing is
// the part worth unit-testing, and it stays out of the page's render path.
//
// Two invariants the page leans on:
//
//  1. A UUID contains no `@`, so a single `split` on it is unambiguous. Same
//     reasoning as the server's `parse_compare_slots`.
//  2. The MODE round-trips through the tokens themselves rather than a second
//     query param. Cross-year mode pins every slot, so "any token carries an
//     `@year`" is exactly "this link was built in cross-year mode" — and a
//     single-season URL keeps the shape it has today, byte for byte.

/** One `?ids=` entry, split into its parts. `season` is null for a bare UUID
 *  (and for a malformed suffix, which the server would reject anyway). */
export interface CompareSlotRef {
  /** The token exactly as it appeared, so a slot nobody edited round-trips
   *  unchanged. */
  raw: string;
  id: string;
  season: number | null;
}

/** Same bounds as `parseSeason` in `components/season.ts` — accept any
 *  plausibly-shaped year rather than only the ones the API has told us about,
 *  so a shared link to an older season survives a cold render. */
function parseSlotSeason(raw: string): number | null {
  const n = Number(raw.trim());
  if (!Number.isInteger(n) || n < 2000 || n > 2100) return null;
  return n;
}

export function parseSlotToken(token: string): CompareSlotRef {
  const raw = token.trim();
  const at = raw.indexOf('@');
  if (at < 0) return { raw, id: raw, season: null };
  return {
    raw,
    id: raw.slice(0, at).trim(),
    season: parseSlotSeason(raw.slice(at + 1)),
  };
}

/** Split a raw `?ids=` value into tokens. Empty entries are dropped, matching
 *  the server's tolerance for `a,,b`. */
export function parseCompareIds(idsCsv: string): string[] {
  return idsCsv
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

export function slotToken(id: string, season: number | null): string {
  return season == null ? id : `${id}@${season}`;
}

/** True when the token list was built in cross-year mode. */
export function idsHaveSlotSeasons(tokens: string[]): boolean {
  return tokens.some((t) => parseSlotToken(t).season != null);
}

/** Entering cross-year mode: pin every unpinned slot to the season it is
 *  already being rendered in, so the switch changes nothing on screen and the
 *  mode is immediately readable back off the URL. */
export function pinSlotSeasons(tokens: string[], season: number): string[] {
  return tokens.map((t) => {
    const slot = parseSlotToken(t);
    return slotToken(slot.id, slot.season ?? season);
  });
}

/** Leaving cross-year mode: drop every per-slot year. Two slots that were the
 *  same player in different years collapse onto one token here — the whole
 *  point of single-season mode is that there is one year — so dedup rather
 *  than leave a duplicate column that the server would answer twice. */
export function clearSlotSeasons(tokens: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tokens) {
    const { id } = parseSlotToken(t);
    if (seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

/** Repoint one slot at a different year, leaving every other slot alone. */
export function setSlotSeason(
  tokens: string[],
  index: number,
  season: number,
): string[] {
  return tokens.map((t, i) =>
    i === index ? slotToken(parseSlotToken(t).id, season) : t,
  );
}
