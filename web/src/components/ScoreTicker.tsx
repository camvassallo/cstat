import { useEffect, useMemo, useState } from 'react';
import {
  fetchTicker,
  type GameResult,
  type TickerResponse,
  type UpcomingTile,
} from '../api/client';
import { useSeason, seasonHref } from './season';
import { shortDate } from './format';
import { Link } from 'react-router-dom';

// Per-tile width including the gap between tiles. Used to estimate marquee
// duration so scroll speed stays roughly constant regardless of tile count
// (~50 px/s — ESPN-pace, leisurely enough to read on hover).
const TILE_WIDTH_PX = 184;
const SCROLL_SPEED_PX_PER_SEC = 50;

const TILE_CLASSES =
  'flex flex-col flex-shrink-0 w-44 px-3 py-1.5 rounded bg-gray-900 hover:bg-gray-800 border border-gray-800 transition-colors no-underline';

function PastTile({ g, season }: { g: GameResult; season: number }) {
  const homeWon =
    g.home_score != null && g.away_score != null && g.home_score > g.away_score;
  const awayWon =
    g.home_score != null && g.away_score != null && g.away_score > g.home_score;
  const home = g.home_team_name ?? '—';
  const away = g.away_team_name ?? '—';
  const body = (
    <>
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className={`truncate ${homeWon ? 'font-semibold text-gray-100' : 'text-gray-400'}`}>
          {home}
        </span>
        <span className={`font-mono ${homeWon ? 'font-bold text-gray-100' : 'text-gray-400'}`}>
          {g.home_score ?? '—'}
        </span>
      </div>
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className={`truncate ${awayWon ? 'font-semibold text-gray-100' : 'text-gray-400'}`}>
          {away}
        </span>
        <span className={`font-mono ${awayWon ? 'font-bold text-gray-100' : 'text-gray-400'}`}>
          {g.away_score ?? '—'}
        </span>
      </div>
      <div className="text-[10px] text-gray-500 mt-0.5">FINAL · {shortDate(g.game_date)}</div>
    </>
  );
  // Deep-link past tiles to Predict so the user lands on the matchup view —
  // the prior box score auto-surfaces in the Previous Matchups section. Falls
  // back to a static tile when team names are missing (Predict looks teams
  // up by name and would 404 on the placeholder "—"). team_id isn't needed
  // because the deep-link uses names, not IDs.
  const canDeepLink = g.home_team_name != null && g.away_team_name != null;
  if (!canDeepLink) return <div className={TILE_CLASSES}>{body}</div>;
  const venueParam = g.is_neutral_site ? '&venue=neutral' : '';
  const predictTo = `/predict?home=${encodeURIComponent(home)}&away=${encodeURIComponent(away)}${venueParam}`;
  return (
    <Link to={seasonHref(predictTo, season)} className={TILE_CLASSES}>
      {body}
    </Link>
  );
}

function UpcomingTileView({ g, season }: { g: UpcomingTile; season: number }) {
  const home = g.home_team_name ?? '—';
  const away = g.away_team_name ?? '—';
  const homeFavored = g.predicted_margin > 0;
  const favorite = homeFavored ? home : away;
  const spread = `−${Math.abs(g.predicted_margin).toFixed(1)}`;
  const winProb = homeFavored ? g.home_win_probability : 1 - g.home_win_probability;
  const winPct = `${Math.round(winProb * 100)}%`;
  // Build the deep link only when both team names resolved — Predict looks
  // teams up by name and would 404 on the placeholder "—". Pass `venue=neutral`
  // through to Predict when the schedule says so; otherwise the page would
  // default to home venue and disagree with the ticker's prediction.
  const canDeepLink = g.home_team_name != null && g.away_team_name != null;
  const venueParam = g.is_neutral_site ? '&venue=neutral' : '';
  const predictTo = canDeepLink
    ? `/predict?home=${encodeURIComponent(home)}&away=${encodeURIComponent(away)}${venueParam}`
    : null;
  const body = (
    <>
      <div className="text-xs text-gray-300 truncate">
        {home} <span className="text-gray-500">vs</span> {away}
      </div>
      <div className="text-[11px] text-gray-100 mt-0.5 truncate">
        <span className="font-semibold">{favorite}</span>{' '}
        <span className="font-mono text-blue-300">{spread}</span>{' '}
        <span className="text-gray-500">({winPct})</span>
      </div>
      <div className="text-[10px] text-gray-500 mt-0.5">UPCOMING · {shortDate(g.game_date)}</div>
    </>
  );
  if (!predictTo) return <div className={TILE_CLASSES}>{body}</div>;
  return (
    <Link to={seasonHref(predictTo, season)} className={TILE_CLASSES}>
      {body}
    </Link>
  );
}

type TickerEntry =
  | { kind: 'past'; data: GameResult }
  | { kind: 'upcoming'; data: UpcomingTile };

/** Auto-scrolling marquee at the top of the Rankings homepage. Upcoming
 *  games come first (more news-worthy), then recent finals. Hovering the
 *  strip pauses the animation so users can read; honors `prefers-reduced-
 *  motion` via the keyframe in `index.css`. Animates whenever there are at
 *  least 2 tiles regardless of viewport — the duplicated-track marquee
 *  loops seamlessly even when content fits. Hidden entirely if there's
 *  nothing to show. */
export function ScoreTicker() {
  const { season } = useSeason();
  const [data, setData] = useState<TickerResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetchTicker({ season, past: 8, future: 8 })
      .then((r) => {
        if (!cancelled) setData(r);
      })
      .catch(() => {
        if (!cancelled) setData(null);
      });
    return () => {
      cancelled = true;
    };
  }, [season]);

  // Combine upcoming + past into a single ordered list. Memoised so the
  // animation track doesn't reset on unrelated re-renders.
  const entries = useMemo<TickerEntry[]>(() => {
    if (!data) return [];
    return [
      ...data.upcoming.map((g): TickerEntry => ({ kind: 'upcoming', data: g })),
      ...data.past.map((g): TickerEntry => ({ kind: 'past', data: g })),
    ];
  }, [data]);

  // Animate whenever there are at least 2 tiles. Speed-scaled per tile count
  // so a 4-tile strip and a 16-tile strip both feel like ESPN-pace. With the
  // tiles duplicated and a `-50%` translate, the second copy lands exactly
  // where the first started → seamless loop regardless of whether the
  // single-copy width exceeds the viewport.
  const shouldAnimate = entries.length >= 2;
  const duration = useMemo(() => {
    const oneCopyWidth = entries.length * TILE_WIDTH_PX;
    return `${Math.max(20, Math.round(oneCopyWidth / SCROLL_SPEED_PX_PER_SEC))}s`;
  }, [entries.length]);

  if (!data || entries.length === 0) return null;

  const renderEntry = (e: TickerEntry, key: string) =>
    e.kind === 'upcoming' ? (
      <UpcomingTileView key={key} g={e.data} season={season} />
    ) : (
      <PastTile key={key} g={e.data} season={season} />
    );

  return (
    // Container: `overflow-hidden` clips the marquee normally. Under reduced-
    // motion the CSS keyframe is disabled (see `index.css`), so the track
    // would sit frozen at translateX(0) and clip every tile past the first
    // few. `motion-reduce:overflow-x-auto` flips to manual scroll for those
    // users; the duplicate-track copy below is also hidden under reduced-
    // motion so they don't see the same tile list twice.
    <div className="bg-gray-950 border border-gray-800 rounded-md overflow-hidden motion-reduce:overflow-x-auto">
      <div
        className={`flex items-stretch gap-2 px-3 sm:px-6 py-2 w-max ${
          shouldAnimate ? 'ticker-track' : ''
        }`}
        style={shouldAnimate ? ({ '--ticker-duration': duration } as React.CSSProperties) : undefined}
      >
        {entries.map((e, i) => renderEntry(e, `a-${i}`))}
        {shouldAnimate && (
          // `display: contents` means this wrapper is invisible to flex
          // layout (gap-2 still applies between each tile and its neighbors).
          // `motion-reduce:hidden` collapses the wrapper to `display: none`
          // under reduced-motion so the duplicate copy disappears entirely.
          <div className="contents motion-reduce:hidden">
            {entries.map((e, i) => renderEntry(e, `b-${i}`))}
          </div>
        )}
      </div>
    </div>
  );
}
