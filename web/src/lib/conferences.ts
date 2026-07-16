// Conference display names.
//
// The DB stores terse conference codes in NatStat's vocabulary (`BIG12`,
// `A-SUN`, `SOCON`, …). Those are the join/filter keys the rest of the app
// relies on, but they read poorly in the UI. This is the single source of
// truth for turning a stored code into the commonly accepted public name
// (the style ESPN / Sports Reference use), so every page renders conferences
// the same way. Unknown codes fall through to the raw value, and a null /
// blank conference renders as "Independent".

export const CONFERENCE_NAMES: Record<string, string> = {
  ACC: 'ACC',
  'A-10': 'Atlantic 10',
  'A-EAST': 'America East',
  AMER: 'American',
  'A-SUN': 'ASUN',
  BIG10: 'Big Ten',
  BIG12: 'Big 12',
  BIGEAST: 'Big East',
  BIGSKY: 'Big Sky',
  BIGSOUTH: 'Big South',
  BIGWEST: 'Big West',
  CAA: 'CAA',
  'C-USA': 'C-USA',
  HL: 'Horizon',
  IND: 'Independent',
  IVY: 'Ivy League',
  MAAC: 'MAAC',
  MAC: 'MAC',
  MEAC: 'MEAC',
  MVC: 'Missouri Valley',
  MWC: 'Mountain West',
  NEC: 'Northeast',
  OVC: 'Ohio Valley',
  'PAC-12': 'Pac-12',
  PL: 'Patriot',
  SEC: 'SEC',
  SLC: 'Southland',
  SOCON: 'Southern',
  SUMMIT: 'Summit',
  SUNBELT: 'Sun Belt',
  SWAC: 'SWAC',
  WAC: 'WAC',
  WCC: 'West Coast',
};

/** Display label for a stored conference code. Null/blank → "Independent". */
export function conferenceLabel(code: string | null | undefined): string {
  if (code == null || code === '') return 'Independent';
  return CONFERENCE_NAMES[code] ?? code;
}

/**
 * Search haystack for a conference: both the raw code and its display name,
 * lower-cased, so a user can type either "big 12" or "big12".
 */
export function conferenceSearchText(code: string | null | undefined): string {
  if (code == null || code === '') return 'independent';
  return `${code} ${conferenceLabel(code)}`.toLowerCase();
}
