// Per-slot seasons for the Predict page's Time Machine mode.
//
// The Compare page pins a season onto each `?ids=` token; Predict has only two
// slots and they are already named separately in the URL, so its per-slot years
// are their own params: `?home=…&home_season=2015&away=…&away_season=2026`.
// This module is the pure half — parsing, request shaping, labelling — kept out
// of the page's render path because it is the part worth unit-testing.
//
// Two invariants the page leans on, both mirroring `compareSlots`:
//
//  1. The MODE round-trips through the params themselves rather than a second
//     `?mode=`. Cross-year writes both years, so "either year param is present"
//     is exactly "this link was built in cross-year mode" — and a single-season
//     URL keeps the shape the ticker and schedule rows already build, byte for
//     byte.
//  2. A request that names neither year is the legacy single-season request,
//     unchanged on the wire. See `toRequest`.

import type { PredictRequest, Venue } from '../api/client';

/** Same bounds as `parseSeason` in `components/season.ts` — accept any
 *  plausibly-shaped year rather than only the ones the API has told us about,
 *  so a shared link to an older season survives a cold render. Anything else
 *  reads as absent, which lands that slot back on the site-wide season exactly
 *  as the backend's own fallback does. */
export function parseSlotSeasonParam(raw: string | null): number | null {
  if (!raw) return null;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 2000 || n > 2100) return null;
  return n;
}

/** A prediction the page is asking for. Both years are always resolved here,
 *  even in single-season mode where they are equal, so every consumer —
 *  request, dedupe key, panel labelling — reads them from one place. */
export interface Matchup {
  home: string;
  away: string;
  venue: Venue;
  homeSeason: number;
  awaySeason: number;
  /** Empty when unset. Ignored entirely in cross-year mode. */
  asOfDate: string;
  crossYear: boolean;
}

/** Which year each side came from, and whether to print it. Threaded to the
 *  result panels as one object rather than three loose props so a panel cannot
 *  end up holding the seasons without the flag that says to show them — a name
 *  that silently drops its year is this mode's whole failure mode. */
export interface SlotYears {
  home: number;
  away: number;
  /** True exactly when the two slots disagree about the year. One switch
   *  behind every year-suffixed name on the page. */
  show: boolean;
}

export function slotYears(m: Matchup): SlotYears {
  return {
    home: m.homeSeason,
    away: m.awaySeason,
    show: m.homeSeason !== m.awaySeason,
  };
}

/** The one team-name format for cross-year mode: `2015 Duke`. Settled here
 *  rather than per component so the headline, the score line, the stat table
 *  and the roster panels cannot drift into three different conventions — and so
 *  2015 Duke vs 2026 Duke reads as two teams rather than a typo.
 *
 *  Year first because that is how a season's team is named out loud — the 2015
 *  Duke team, 1996 Kentucky — and because the score line puts a number straight
 *  after the name: `2026 Duke 71` reads cleanly where `Duke 2026 71` runs two
 *  numbers together. */
export function teamLabel(name: string, season: number, show: boolean): string {
  return show ? `${season} ${name}` : name;
}

/** Cross-year sends both years explicitly; single-season sends the one `season`
 *  param this page has always sent, so its request stays byte-for-byte what it
 *  was. `as_of_date` is dropped cross-year rather than passed through: the
 *  point-in-time cohort is built inside a single season and the backend 400s on
 *  the combination, so sending it could only produce an error. */
export function toRequest(m: Matchup): PredictRequest {
  return m.crossYear
    ? {
        home: m.home,
        away: m.away,
        venue: m.venue,
        homeSeason: m.homeSeason,
        awaySeason: m.awaySeason,
      }
    : {
        home: m.home,
        away: m.away,
        venue: m.venue,
        season: m.homeSeason,
        asOfDate: m.asOfDate || undefined,
      };
}

/** Identity of a request, so the URL-driven effect can tell whether it is
 *  looking at something the form already fetched — a cross-year submit writes
 *  its params to the URL, and without this that write returns as a second,
 *  identical request. */
export function matchupKey(m: Matchup): string {
  return [
    m.home,
    m.away,
    m.venue,
    m.homeSeason,
    m.awaySeason,
    // Not part of a cross-year request's identity — `toRequest` drops it.
    m.crossYear ? '' : m.asOfDate,
    // The mode itself, because the two build different requests. On equal
    // years those two requests happen to come back the same today, so leaving
    // it out would be a dedupe that is correct only by coincidence — and the
    // coincidence would be worth re-checking on every future change to either
    // side of the wire.
    m.crossYear,
  ].join('|');
}
