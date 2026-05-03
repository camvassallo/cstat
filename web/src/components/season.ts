import { useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';

// Site-wide season selector. Two values today; adding 2024+ later is one entry
// and a re-ingest. Order matters — newest first so the dropdown reads top-down
// from current → historical.
//
// File-shape note: keep this module .ts (no JSX), separate from
// `SeasonLink.tsx`. Vite's `react-refresh/only-export-components` lint rule
// rejects mixing component + non-component exports in a `.tsx` file.
export const AVAILABLE_SEASONS = [2026, 2025] as const;
export type Season = (typeof AVAILABLE_SEASONS)[number];

export const DEFAULT_SEASON: Season = AVAILABLE_SEASONS[0];

/** Append `?season=N` to an in-app path when the season differs from the
 *  default. Use for imperative navigation (`navigate(seasonHref('/teams/x', s))`).
 *  For declarative links inside JSX, prefer `<SeasonLink>`. */
export function seasonHref(path: string, season: Season): string {
  if (season === DEFAULT_SEASON) return path;
  const [pathPart, queryPart = ''] = path.split('?');
  const params = new URLSearchParams(queryPart);
  if (!params.has('season')) params.set('season', String(season));
  return `${pathPart}?${params.toString()}`;
}

export function parseSeason(raw: string | null): Season {
  if (!raw) return DEFAULT_SEASON;
  const n = Number(raw);
  return (AVAILABLE_SEASONS as readonly number[]).includes(n)
    ? (n as Season)
    : DEFAULT_SEASON;
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
      if (next === DEFAULT_SEASON) {
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
