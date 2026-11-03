import { useEffect, useState } from 'react';
import { fetchScheduleLive, type LiveScore, type ScheduleGame } from '../api/client';

/** Hour (Eastern) the default day rolls over at. Mirrors the API's
 *  `SCHEDULE_ROLLOVER_HOUR`: a late West Coast or Hawaii tip ends after
 *  midnight Eastern, and checking on it should not land on tomorrow. */
export const ROLLOVER_HOUR = 6;

/** How often running scores refresh while a game on the night is unfinished.
 *  The API reuses one scoreboard read for 20 seconds, so this is about as
 *  fresh as polling can get without asking for the same copy twice. */
const LIVE_POLL_MS = 30_000;

/** Tonight, as `YYYY-MM-DD`: the US Eastern date six hours ago, so it rolls
 *  over at 6 AM Eastern. Game dates are Eastern, so a viewer in London at
 *  1:30 AM is still on tonight's slate. `en-CA` formats as ISO year-month-day. */
export function easternToday(): string {
  const shifted = new Date(Date.now() - ROLLOVER_HOUR * 3_600_000);
  return new Intl.DateTimeFormat('en-CA', { timeZone: 'America/New_York' }).format(shifted);
}

/** The day before an ISO date, as `YYYY-MM-DD`. */
function dayBefore(iso: string): string {
  const [y, m, d] = iso.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d - 1)).toISOString().slice(0, 10);
}

const hasResult = (g: ScheduleGame) => g.home_score != null && g.away_score != null;

/** Running scores for the night of `date`, keyed by `game_id`, or null before
 *  the first read. Polls only for tonight (or, before the rollover, last
 *  night) and only while some game on it is unfinished; pauses while the tab
 *  is hidden and catches up as soon as it is shown again. `games` is the
 *  night's slate, or null while it loads. */
export function useLiveScores(
  date: string,
  games: ScheduleGame[] | null,
): Record<string, LiveScore> | null {
  const [live, setLive] = useState<{ date: string; games: Record<string, LiveScore> } | null>(
    null,
  );
  const today = easternToday();
  const isLiveNight = date === today || date === dayBefore(today);
  const liveGames = live && live.date === date ? live.games : null;
  const hasPending = !!games?.some((g) => !hasResult(g));
  const allSettled =
    !!liveGames && !!games?.every((g) => hasResult(g) || liveGames[g.game_id]?.state === 'final');
  const shouldPoll = isLiveNight && hasPending && !allSettled;

  useEffect(() => {
    if (!shouldPoll) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = () => {
      if (document.hidden) return;
      fetchScheduleLive(date)
        .then((r) => {
          // The feed was unreachable: keep what is on screen.
          if (!cancelled && !r.unavailable) setLive({ date, games: r.games });
        })
        // A missed refresh keeps the last scores; the next tick tries again.
        .catch(() => {})
        .finally(() => {
          if (!cancelled) timer = setTimeout(tick, LIVE_POLL_MS);
        });
    };
    const onVisible = () => {
      if (document.hidden || cancelled) return;
      clearTimeout(timer);
      tick();
    };
    tick();
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [date, shouldPoll]);

  return liveGames;
}
