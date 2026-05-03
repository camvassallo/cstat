import { type ComponentProps } from 'react';
import { Link, useLocation } from 'react-router-dom';
import { DEFAULT_SEASON, parseSeason } from './season';

/** Drop-in replacement for `react-router` `Link` that preserves the current
 *  season query param on outbound navigation. So clicking a team name on the
 *  2025 Rankings page lands you on `/teams/:id?season=2025` instead of
 *  silently snapping back to the default season. Accepts the same `to` shapes
 *  as `Link` (string or LocationDescriptor).
 *
 *  Lives in its own .tsx file (separate from `season.ts`) so Vite's
 *  `react-refresh/only-export-components` lint rule is satisfied. */
type LinkProps = ComponentProps<typeof Link>;

export function SeasonLink({ to, ...rest }: LinkProps) {
  const location = useLocation();
  const currentSeason = parseSeason(
    new URLSearchParams(location.search).get('season'),
  );

  // Default season uses no query param — outbound links match.
  if (currentSeason === DEFAULT_SEASON) {
    return <Link to={to} {...rest} />;
  }

  const seasonStr = String(currentSeason);

  if (typeof to === 'string') {
    // Don't clobber a season already in the destination string.
    const [pathPart, queryPart = ''] = to.split('?');
    const params = new URLSearchParams(queryPart);
    if (!params.has('season')) params.set('season', seasonStr);
    const qs = params.toString();
    return <Link to={qs ? `${pathPart}?${qs}` : pathPart} {...rest} />;
  }

  // Object-shaped `to` ({ pathname, search, ... }).
  const search = to.search ?? '';
  const params = new URLSearchParams(
    search.startsWith('?') ? search.slice(1) : search,
  );
  if (!params.has('season')) params.set('season', seasonStr);
  return (
    <Link
      to={{ ...to, search: `?${params.toString()}` }}
      {...rest}
    />
  );
}
