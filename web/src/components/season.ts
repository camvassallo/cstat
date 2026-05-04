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
export const AVAILABLE_SEASONS_FALLBACK: readonly number[] = [2026, 2025];

export type Season = number;

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
