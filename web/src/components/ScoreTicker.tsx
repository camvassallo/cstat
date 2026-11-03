import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  fetchTicker,
  type GameResult,
  type TickerResponse,
  type UpcomingTile,
} from '../api/client';
import { useSeason, seasonHref } from './season';
import { shortDate } from './format';
import { Link } from 'react-router-dom';

const TILE_CLASSES =
  'flex flex-col flex-shrink-0 snap-start w-44 px-3 py-1.5 rounded bg-gray-900 hover:bg-gray-800 border border-gray-800 transition-colors no-underline';

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
  //
  // For completed games, carry `as_of_date = game_date − 1` so the Predict
  // page reproduces the honest pre-game projection rather than serving a
  // current-state forecast — same shape TeamDetail's schedule row uses.
  // Date arithmetic mirrors ScheduleRow at TeamDetail.tsx: parse as UTC
  // midnight, subtract one day, slice the ISO date.
  const canDeepLink = g.home_team_name != null && g.away_team_name != null;
  if (!canDeepLink) return <div className={TILE_CLASSES}>{body}</div>;
  const venueParam = g.is_neutral_site ? '&venue=neutral' : '';
  let asOfParam = '';
  if (g.game_date) {
    const d = new Date(`${g.game_date}T00:00:00Z`);
    d.setUTCDate(d.getUTCDate() - 1);
    asOfParam = `&as_of_date=${d.toISOString().slice(0, 10)}`;
  }
  const predictTo = `/predict?home=${encodeURIComponent(home)}&away=${encodeURIComponent(away)}${venueParam}${asOfParam}`;
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
  // Projected final, winner first, matching the post-game tile shape so
  // upcoming and past read with the same visual structure.
  const winnerName = homeFavored ? home : away;
  const loserName = homeFavored ? away : home;
  const winnerScore = homeFavored ? g.predicted_home_score : g.predicted_away_score;
  const loserScore = homeFavored ? g.predicted_away_score : g.predicted_home_score;
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
        <span className="font-semibold">{winnerName}</span>{' '}
        <span className="font-mono text-gray-100">{winnerScore}</span>
        <span className="text-gray-600 mx-1">—</span>
        <span>{loserName}</span>{' '}
        <span className="font-mono text-gray-400">{loserScore}</span>
      </div>
      <div className="text-[11px] text-gray-400 mt-0.5 truncate">
        <span className="font-medium">{favorite}</span>{' '}
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

/** Scorebug at the top of the Rankings homepage: upcoming games first (more
 *  news-worthy), then recent finals, in a strip the reader scrolls themself —
 *  swipe or trackpad, or the arrows on a wider screen, which appear only on a
 *  side with more tiles to see. Hidden entirely if there's nothing to show. */
export function ScoreTicker() {
  const { season } = useSeason();
  const [data, setData] = useState<TickerResponse | null>(null);
  const stripRef = useRef<HTMLDivElement>(null);
  const [canScroll, setCanScroll] = useState({ left: false, right: false });

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

  // Combine upcoming + past into a single ordered list.
  const entries = useMemo<TickerEntry[]>(() => {
    if (!data) return [];
    return [
      ...data.upcoming.map((g): TickerEntry => ({ kind: 'upcoming', data: g })),
      ...data.past.map((g): TickerEntry => ({ kind: 'past', data: g })),
    ];
  }, [data]);

  const measure = useCallback(() => {
    const el = stripRef.current;
    if (!el) return;
    setCanScroll({
      left: el.scrollLeft > 1,
      right: el.scrollLeft + el.clientWidth < el.scrollWidth - 1,
    });
  }, []);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [entries, measure]);

  if (!data || entries.length === 0) return null;

  // A page of tiles less one, so the last tile seen stays in view as context.
  const scrollBy = (dir: 1 | -1) => {
    const el = stripRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * Math.max(el.clientWidth - 184, 184), behavior: 'smooth' });
  };
  const arrow = (dir: 1 | -1) => (
    <button
      type="button"
      onClick={() => scrollBy(dir)}
      aria-label={dir === 1 ? 'Scroll games right' : 'Scroll games left'}
      className={`hidden sm:flex absolute inset-y-0 ${
        dir === 1 ? 'right-0 bg-gradient-to-l' : 'left-0 bg-gradient-to-r'
      } from-gray-950 via-gray-950/90 to-transparent w-10 items-center justify-center text-gray-300 hover:text-white text-lg`}
    >
      {dir === 1 ? '›' : '‹'}
    </button>
  );

  return (
    <div className="relative bg-gray-950 border border-gray-800 rounded-md">
      <div
        ref={stripRef}
        onScroll={measure}
        className="flex items-stretch gap-2 px-3 sm:px-6 py-2 overflow-x-auto snap-x snap-mandatory scroll-px-3 sm:scroll-px-6 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
      >
        {entries.map((e, i) =>
          e.kind === 'upcoming' ? (
            <UpcomingTileView key={`u-${i}`} g={e.data} season={season} />
          ) : (
            <PastTile key={`p-${i}`} g={e.data} season={season} />
          ),
        )}
      </div>
      {canScroll.left && arrow(-1)}
      {canScroll.right && arrow(1)}
    </div>
  );
}
