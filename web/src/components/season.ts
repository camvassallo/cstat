import { useCallback, useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { fetchSeasons } from '../api/client';

// Static fallback for the seasons dropdown. Used for the first paint and if
// `/api/seasons` is unreachable. The API is the source of truth once it
// responds — adding a new season to the DB doesn't require a frontend rebuild
// any more.
//
// File-shape note: keep this module .ts (no JSX), separate from
// `SeasonLink.tsx`. Vite's `react-refresh/only-export-components` lint rule
// rejects mixing component + non-component exports in a `.tsx` file.
export const AVAILABLE_SEASONS_FALLBACK: readonly number[] = [2026, 2025, 2024];

export type Season = number;

// ---------------------------------------------------------------------------
// Projectable seasons (the "Future" tab + team projection ledger)
// ---------------------------------------------------------------------------
// Shared by the `/projected` grid and the team projection ledger so both
// publish the SAME season list to the navbar picker — including the upcoming
// forecast year, which `/api/seasons` (games-only) can never carry.

/** Earliest cstat-season the projection pipeline can target. The backend
 *  composes from `year − 1` and needs that base season's
 *  `trajectory_oof_predictions`, which start at target-season 2016. */
export const EARLIEST_PROJECTABLE_YEAR = 2016;

/** The upcoming (not-yet-played) projection target = newest-played + 1
 *  (e.g. 2027 in 2026). Uses the static fallback's newest year so every surface
 *  agrees without waiting on a `/api/seasons` fetch. */
export function upcomingProjectionSeason(): Season {
  return AVAILABLE_SEASONS_FALLBACK[0] + 1;
}

/** Every projectable cstat-season, newest first: the upcoming forecast down to
 *  {@link EARLIEST_PROJECTABLE_YEAR}. The list the projection surfaces hand to
 *  the navbar season picker. */
export function projectableSeasons(): Season[] {
  const ys: Season[] = [];
  for (let y = upcomingProjectionSeason(); y >= EARLIEST_PROJECTABLE_YEAR; y--) {
    ys.push(y);
  }
  return ys;
}

export const DEFAULT_SEASON: Season = AVAILABLE_SEASONS_FALLBACK[0];

/** Module-level cache so a season selector and a SeasonLink in the same render
 *  don't each fire a /seasons fetch. Populated by `useAvailableSeasons` after
 *  its first successful response. */
let cachedSeasons: number[] | null = null;
let cachedDefault: number | null = null;

/** Read-only accessor for the current default season. Prefers the API's
 *  default once it's been fetched; falls back to the static constant during
 *  the first paint or if the API is unreachable. Used by non-hook code paths
 *  (e.g. `SeasonLink`) that can't call `useAvailableSeasons`. */
export function getDefaultSeason(): Season {
  return cachedDefault ?? DEFAULT_SEASON;
}

/** Append `?season=N` to an in-app path when the season differs from the
 *  default. Use for imperative navigation (`navigate(seasonHref('/teams/x', s))`).
 *  For declarative links inside JSX, prefer `<SeasonLink>`. */
export function seasonHref(path: string, season: Season): string {
  const def = cachedDefault ?? DEFAULT_SEASON;
  if (season === def) return path;
  const [pathPart, queryPart = ''] = path.split('?');
  const params = new URLSearchParams(queryPart);
  if (!params.has('season')) params.set('season', String(season));
  return `${pathPart}?${params.toString()}`;
}

/** Validate `?season=` from the URL. Accepts any plausibly-shaped year so a
 *  copy-pasted link to a season the API knows about (but the static fallback
 *  doesn't) still works on cold render. The dropdown still constrains
 *  user-facing choice to the API list once it loads. */
export function parseSeason(raw: string | null): Season {
  const def = cachedDefault ?? DEFAULT_SEASON;
  if (!raw) return def;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 2000 || n > 2100) return def;
  return n;
}

/** Read the current season from `?season=` (URL is the source of truth so
 *  links and refreshes carry it) and expose a setter that preserves all other
 *  search params. Falls back to the default when the param is missing or
 *  unrecognized. */
export function useSeason(): {
  season: Season;
  setSeason: (s: Season) => void;
} {
  const [params, setParams] = useSearchParams();
  const season = parseSeason(params.get('season'));

  const setSeason = useCallback(
    (next: Season) => {
      const newParams = new URLSearchParams(params);
      const def = cachedDefault ?? DEFAULT_SEASON;
      if (next === def) {
        // Keep URLs clean for the current season — no `?season=2026` clutter.
        newParams.delete('season');
      } else {
        newParams.set('season', String(next));
      }
      setParams(newParams, { replace: false });
    },
    [params, setParams],
  );

  return { season, setSeason };
}

// ---------------------------------------------------------------------------
// Page-scoped season override
// ---------------------------------------------------------------------------
// Detail pages (player, team) call `setPageSeasons(...)` once they know which
// seasons their entity has data in. The site-wide season selector reads this
// value and constrains the dropdown to those years; null means "use the
// global list". Stored as module state + a tiny pub/sub so the selector,
// which lives in `<Layout>` outside the detail page's tree, can react.
//
// The page is responsible for calling `setPageSeasons(null)` on unmount to
// release the override — done via the cleanup return of `useEffect`.

let pageSeasons: readonly number[] | null = null;
const pageSeasonsListeners = new Set<() => void>();

export function setPageSeasons(seasons: readonly number[] | null): void {
  // Compare by reference + length to avoid spamming subscribers on identical
  // updates (e.g. same array re-set on every render of a detail page).
  if (pageSeasons === seasons) return;
  const prev = pageSeasons;
  if (
    seasons != null &&
    prev != null &&
    seasons.length === prev.length &&
    seasons.every((s, i) => s === prev[i])
  ) {
    return;
  }
  pageSeasons = seasons;
  pageSeasonsListeners.forEach((l) => l());
}

/** Subscribe to page-scoped season list changes. Returns the current
 *  override or null when no page has set one. */
export function usePageSeasons(): readonly number[] | null {
  const [snapshot, setSnapshot] = useState<readonly number[] | null>(pageSeasons);
  useEffect(() => {
    const listener = () => setSnapshot(pageSeasons);
    pageSeasonsListeners.add(listener);
    // Sync up after subscribing in case the value changed between initial
    // render and effect mount.
    listener();
    return () => {
      pageSeasonsListeners.delete(listener);
    };
  }, []);
  return snapshot;
}

/** Fetch the list of seasons present in the DB. Returns the cached list
 *  immediately and refreshes from the API on first mount. The fallback array
 *  is used until the API responds (or forever, if it doesn't). */
export function useAvailableSeasons(): { seasons: readonly number[]; defaultSeason: number } {
  const [seasons, setSeasons] = useState<readonly number[]>(
    cachedSeasons ?? AVAILABLE_SEASONS_FALLBACK,
  );
  const [def, setDef] = useState<number>(cachedDefault ?? DEFAULT_SEASON);

  useEffect(() => {
    let cancelled = false;
    fetchSeasons()
      .then((res) => {
        if (cancelled) return;
        if (res.seasons.length === 0) return; // empty DB, keep fallback
        cachedSeasons = res.seasons;
        cachedDefault = res.default ?? res.seasons[0];
        setSeasons(res.seasons);
        setDef(cachedDefault);
      })
      .catch(() => {
        // Stay on the fallback — the dropdown still works, just with an older
        // hardcoded list. No need to surface the error to users.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return { seasons, defaultSeason: def };
}
