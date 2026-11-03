import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import {
  fetchSchedule,
  fetchScheduleLive,
  type LiveScore,
  type ScheduleGame,
  type ScheduleResponse,
} from "../api/client";
import ScheduleRecord from "../components/ScheduleRecord";
import { setPageSeasons } from "../components/season";
import { usePageTitle } from "../components/usePageTitle";

/// One day's games, ranked by how worth watching they look before tipoff.
///
/// Date-driven rather than season-driven: the day lives in `?date=` so a slate
/// can be shared, and the navbar season picker does not apply here.

type SortKey = "watch" | "time";

/** Hour (Eastern) the default day rolls over at. Mirrors the API's
 *  `SCHEDULE_ROLLOVER_HOUR`: a late West Coast or Hawaii tip ends after
 *  midnight Eastern, and checking on it should not land on tomorrow. */
const ROLLOVER_HOUR = 6;

/** How often running scores refresh while a game on the night is unfinished.
 *  The API reuses one feed read for a minute, so polling faster gains nothing. */
const LIVE_POLL_MS = 60_000;

/** The day the page opens on, as `YYYY-MM-DD`: the US Eastern date six hours
 *  ago, so it rolls over at 6 AM Eastern. Game dates are Eastern, so a viewer
 *  in London at 1:30 AM is still on tonight's slate. `en-CA` formats as ISO
 *  year-month-day. */
function easternToday(): string {
  const shifted = new Date(Date.now() - ROLLOVER_HOUR * 3_600_000);
  return new Intl.DateTimeFormat("en-CA", {
    timeZone: "America/New_York",
  }).format(shifted);
}

/** The day before an ISO date, as `YYYY-MM-DD`. */
function dayBefore(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  const prev = new Date(Date.UTC(y, m - 1, d - 1));
  return prev.toISOString().slice(0, 10);
}

/** `2026-11-02` → `Monday, November 2`. Parsed as a calendar date, not an
 *  instant, so no timezone can move it to the neighbouring day. */
function longDate(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString("en-US", {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
}

/** `19:30:00` → `7:30 PM`. */
function clock(t: string): string {
  const [hh, mm] = t.split(":").map(Number);
  const suffix = hh >= 12 ? "PM" : "AM";
  const h12 = hh % 12 === 0 ? 12 : hh % 12;
  return `${h12}:${String(mm).padStart(2, "0")} ${suffix}`;
}

function watchClass(w: number): string {
  if (w >= 65) return "bg-emerald-600/80 text-white";
  if (w >= 40) return "bg-emerald-800/70 text-emerald-100";
  if (w >= 20) return "bg-gray-700 text-gray-200";
  return "bg-gray-800 text-gray-400";
}

const WATCH_HELP =
  "How worth watching this game looks before tipoff, from 0 to 100. It is high when two good teams are expected to play a close game. It is not graded on a curve: on a quiet night, the best game can still score low.";

function TeamCell({
  id,
  name,
  rank,
  season,
  won,
}: {
  id: string | null;
  name: string | null;
  rank: number | null;
  season: number;
  won: boolean;
}) {
  // No team row: usually a program outside Division I, but also a D-I team the
  // schedule could not be matched to, so the label claims neither.
  const label = name ?? "Unlisted";
  return (
    <span className="whitespace-nowrap">
      {rank != null && (
        <span className="text-gray-500 text-xs mr-1 tabular-nums">{rank}</span>
      )}
      {id ? (
        <Link
          to={`/teams/${id}?season=${season}`}
          className={`hover:underline ${won ? "font-semibold text-gray-100" : "text-gray-200"}`}
        >
          {label}
        </Link>
      ) : (
        <span
          className="text-gray-400"
          title={
            name
              ? undefined
              : "Not a team we track, usually a program outside Division I."
          }
        >
          {label}
        </span>
      )}
    </span>
  );
}

function PredictionCell({ g }: { g: ScheduleGame }) {
  if (g.predicted_margin == null || g.home_win_probability == null) {
    return <span className="text-gray-600">—</span>;
  }
  const homeFav = g.predicted_margin >= 0;
  const fav = (homeFav ? g.home_team_name : g.away_team_name) ?? "TBD";
  const p = homeFav ? g.home_win_probability : 1 - g.home_win_probability;
  const pct = `${Math.round(p * 100)}%`;
  const hasScore =
    g.predicted_home_score != null && g.predicted_away_score != null;
  const title =
    g.prediction_basis === "preseason"
      ? "From preseason projections: at least one of these teams had not played before this game, so there is no projected score."
      : g.prediction_basis === "pregame"
        ? "Projected before tipoff."
        : undefined;
  let body: string;
  if (hasScore) {
    const hi = Math.max(g.predicted_home_score!, g.predicted_away_score!);
    const lo = Math.min(g.predicted_home_score!, g.predicted_away_score!);
    body = `${fav} ${hi}-${lo}`;
  } else {
    body = `${fav} by ${Math.abs(g.predicted_margin).toFixed(1)}`;
  }
  return (
    <span className="whitespace-nowrap" title={title}>
      {body} <span className="text-gray-400">({pct})</span>
      {g.prediction_basis === "preseason" && (
        <span className="ml-1 text-[10px] uppercase tracking-wide text-indigo-300/80">
          pre
        </span>
      )}
    </span>
  );
}

/** The score a row shows: the stored result once the nightly has it, else the
 *  live feed's running or just-final score, else none. */
function shownScore(
  g: ScheduleGame,
  live?: LiveScore,
): [number, number] | null {
  if (g.home_score != null && g.away_score != null)
    return [g.home_score, g.away_score];
  if (
    live &&
    live.state !== "scheduled" &&
    live.home_score != null &&
    live.away_score != null
  ) {
    return [live.home_score, live.away_score];
  }
  return null;
}

function TimeCell({ g, live }: { g: ScheduleGame; live?: LiveScore }) {
  if (g.home_score != null && g.away_score != null) {
    const ot = g.overtime ? ` (${g.overtime === "Y" ? "OT" : g.overtime})` : "";
    return <span className="text-gray-400 whitespace-nowrap">Final{ot}</span>;
  }
  if (live?.state === "live") {
    return (
      <span className="whitespace-nowrap tabular-nums text-red-300">
        <span className="mr-1 inline-block h-1.5 w-1.5 rounded-full bg-red-500 align-middle animate-pulse" />
        {live.detail ?? "Live"}
      </span>
    );
  }
  if (live?.state === "final") {
    return (
      <span className="text-gray-400 whitespace-nowrap">
        {live.detail ?? "Final"}
      </span>
    );
  }
  if (live?.detail)
    return <span className="text-amber-300/90">{live.detail}</span>;
  if (!g.tip_time) return <span className="text-gray-500">TBA</span>;
  return (
    <span className="whitespace-nowrap tabular-nums">{clock(g.tip_time)}</span>
  );
}

/** Where to watch. Blank when no outlet is listed, which is common for games
 *  a week or more out: networks are announced late. */
function NetworkCell({ g }: { g: ScheduleGame }) {
  if (!g.network) return null;
  return <span className="text-gray-400">{g.network}</span>;
}

function GameRow({
  g,
  season,
  live,
}: {
  g: ScheduleGame;
  season: number;
  live?: LiveScore;
}) {
  const score = shownScore(g, live);
  const [homeScore, awayScore] = score ?? [null, null];
  // Bold a winner only once the game is over; a running lead is not a result.
  const over =
    (g.home_score != null && g.away_score != null) || live?.state === "final";
  const homeWon = over && score != null && score[0] > score[1];
  const awayWon = over && score != null && score[1] > score[0];
  const inProgress = !over && score != null;
  const scoreClass = (won: boolean) =>
    inProgress ? "text-gray-100" : won ? "font-semibold" : "text-gray-400";
  return (
    <tr className="border-b border-gray-800 hover:bg-gray-800/40 align-top">
      <td className="py-2 px-2 text-center">
        {g.watchability != null ? (
          <span
            className={`inline-block min-w-[3rem] px-1.5 py-0.5 rounded text-xs font-semibold tabular-nums ${watchClass(g.watchability)}`}
            title={WATCH_HELP}
          >
            {g.watchability.toFixed(1)}
          </span>
        ) : (
          <span className="text-gray-600">—</span>
        )}
      </td>
      <td className="py-2 px-2">
        <div className="flex flex-col gap-0.5">
          <div className="flex items-baseline justify-between gap-3">
            <TeamCell
              id={g.away_team_id}
              name={g.away_team_name}
              rank={g.away_rank}
              season={season}
              won={awayWon}
            />
            {score && (
              <span className={`tabular-nums ${scoreClass(awayWon)}`}>
                {awayScore}
              </span>
            )}
          </div>
          <div className="flex items-baseline justify-between gap-3">
            <span className="whitespace-nowrap">
              <span className="text-gray-500 text-xs mr-1">
                {g.is_neutral_site ? "vs." : "at"}
              </span>
              <TeamCell
                id={g.home_team_id}
                name={g.home_team_name}
                rank={g.home_rank}
                season={season}
                won={homeWon}
              />
            </span>
            {score && (
              <span className={`tabular-nums ${scoreClass(homeWon)}`}>
                {homeScore}
              </span>
            )}
          </div>
          {/* Phones: projection and time fold under the matchup instead of
              sitting in columns off the right edge. */}
          <div className="sm:hidden text-xs text-gray-400 flex flex-wrap gap-x-2 mt-0.5">
            <PredictionCell g={g} />
            <span>·</span>
            <TimeCell g={g} live={live} />
            {g.network && (
              <>
                <span>·</span>
                <NetworkCell g={g} />
              </>
            )}
          </div>
        </div>
      </td>
      <td className="py-2 px-2 hidden sm:table-cell">
        <PredictionCell g={g} />
      </td>
      <td className="py-2 px-2 hidden sm:table-cell">
        <TimeCell g={g} live={live} />
        <div className="text-xs max-w-[11rem]">
          <NetworkCell g={g} />
        </div>
      </td>
      <td className="py-2 px-2 hidden md:table-cell text-gray-400">
        {g.venue_location && (
          <div className="text-gray-300">{g.venue_location}</div>
        )}
        {g.venue && <div className="text-xs">{g.venue}</div>}
      </td>
    </tr>
  );
}

/** Tip-time sort key: scheduled games by time, unscheduled after them. A tip
 *  before the rollover hour belongs to the end of the night (a 3 AM game is
 *  the day's last, not its first), so its hour is pushed past 24. */
function timeKey(g: ScheduleGame): string {
  if (!g.tip_time) return "99";
  const hour = Number(g.tip_time.slice(0, 2));
  return hour < ROLLOVER_HOUR
    ? `${hour + 24}${g.tip_time.slice(2)}`
    : g.tip_time;
}

export default function Schedule() {
  const [params, setParams] = useSearchParams();
  const date = params.get("date") ?? easternToday();
  const sort: SortKey = params.get("sort") === "time" ? "time" : "watch";
  const [data, setData] = useState<ScheduleResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [live, setLive] = useState<{
    date: string;
    games: Record<string, LiveScore>;
  } | null>(null);

  usePageTitle(`Schedule · ${longDate(date)}`);

  useEffect(() => {
    setPageSeasons(null);
  }, []);

  useEffect(() => {
    let cancelled = false;
    fetchSchedule(date)
      .then((r) => {
        if (cancelled) return;
        setError(null);
        setData(r);
      })
      .catch((e) => {
        if (cancelled) return;
        setData(null);
        setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [date]);

  const setParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(params);
    if (value == null) next.delete(key);
    else next.set(key, value);
    setParams(next);
  };
  const goTo = (d: string) => setParam("date", d === easternToday() ? null : d);

  // A response for a different day may still be held while the next one
  // loads. Nothing reads it under the new date: not the table, and not the
  // arrows, whose neighbours would be the old day's.
  const loaded = data && data.date === date ? data : null;

  // Running scores, only for tonight (or, before the rollover, last night) and
  // only while some game on it has no stored result. Polling pauses while the
  // tab is hidden and catches up as soon as it is shown again.
  const today = easternToday();
  const isLiveNight = date === today || date === dayBefore(today);
  const hasPending = !!loaded?.games.some(
    (g) => g.home_score == null || g.away_score == null,
  );
  const liveGames = live && live.date === date ? live.games : null;
  const allSettled =
    !!liveGames &&
    !!loaded?.games.every(
      (g) =>
        (g.home_score != null && g.away_score != null) ||
        liveGames[g.game_id]?.state === "final",
    );
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
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [date, shouldPoll]);

  const games = useMemo(() => {
    if (!loaded) return null;
    const rows = [...loaded.games];
    if (sort === "watch") {
      rows.sort(
        (a, b) =>
          (b.watchability ?? -1) - (a.watchability ?? -1) ||
          timeKey(a).localeCompare(timeKey(b)),
      );
    } else {
      rows.sort((a, b) => timeKey(a).localeCompare(timeKey(b)));
    }
    return rows;
  }, [loaded, sort]);

  const navBtn =
    "px-2.5 py-1 rounded border border-gray-700 text-sm text-gray-300 hover:bg-gray-800 disabled:opacity-40 disabled:hover:bg-transparent";
  const sortBtn = (key: SortKey, label: string) => (
    <button
      type="button"
      onClick={() => setParam("sort", key === "watch" ? null : key)}
      className={`px-2.5 py-1 text-xs rounded ${
        sort === key
          ? "bg-gray-700 text-white"
          : "text-gray-400 hover:text-gray-200"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div>
      <h1 className="text-2xl font-bold">Schedule</h1>
      <p className="text-sm text-gray-400 mt-1 mb-4">
        Every game on the day, with a projected result and how worth watching it
        looks before tipoff. Times are Eastern.
      </p>

      <div className="flex flex-wrap items-center gap-2 mb-4">
        <button
          type="button"
          className={navBtn}
          disabled={!loaded?.prev_date}
          onClick={() => loaded?.prev_date && goTo(loaded.prev_date)}
          aria-label="Previous day with games"
        >
          ‹
        </button>
        <input
          type="date"
          value={date}
          onChange={(e) => e.target.value && goTo(e.target.value)}
          className="bg-gray-800 border border-gray-700 rounded px-2 py-1 text-sm text-gray-200 [color-scheme:dark]"
          aria-label="Pick a date"
        />
        <button
          type="button"
          className={navBtn}
          disabled={!loaded?.next_date}
          onClick={() => loaded?.next_date && goTo(loaded.next_date)}
          aria-label="Next day with games"
        >
          ›
        </button>
        {date !== easternToday() && (
          <button
            type="button"
            className={navBtn}
            onClick={() => goTo(easternToday())}
          >
            Today
          </button>
        )}
        <div className="ml-auto flex items-center gap-1 bg-gray-800/60 rounded p-0.5">
          {sortBtn("watch", "Most watchable")}
          {sortBtn("time", "By time")}
        </div>
      </div>

      <ScheduleRecord date={date} />

      <h2 className="text-lg font-semibold text-gray-200 mb-2">
        {longDate(date)}
        {games && (
          <span className="ml-2 text-sm font-normal text-gray-400">
            {games.length} {games.length === 1 ? "game" : "games"}
          </span>
        )}
      </h2>

      {error ? (
        <div className="p-4 text-amber-300/90 text-sm">
          Could not load this day&rsquo;s games.
        </div>
      ) : !games ? (
        <div className="text-gray-400 p-4">Loading…</div>
      ) : games.length === 0 ? (
        <div className="p-4 text-gray-400 text-sm">
          No games on this day.
          {loaded?.next_date && (
            <>
              {" "}
              <button
                type="button"
                className="underline"
                onClick={() => goTo(loaded.next_date!)}
              >
                Next games: {longDate(loaded.next_date)}
              </button>
            </>
          )}
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700">
                <th
                  className="py-2 px-2 text-center font-medium"
                  title={WATCH_HELP}
                >
                  Watch
                </th>
                <th className="py-2 px-2 text-left font-medium">Game</th>
                <th className="py-2 px-2 text-left font-medium hidden sm:table-cell">
                  Projection
                </th>
                <th className="py-2 px-2 text-left font-medium hidden sm:table-cell">
                  Time / TV
                </th>
                <th className="py-2 px-2 text-left font-medium hidden md:table-cell">
                  Location
                </th>
              </tr>
            </thead>
            <tbody>
              {games.map((g) => (
                <GameRow
                  key={g.game_id}
                  g={g}
                  season={loaded!.season}
                  live={liveGames?.[g.game_id]}
                />
              ))}
            </tbody>
          </table>
          <p className="text-xs text-gray-500 mt-3">
            Numbers before a team are its current rank. &ldquo;Pre&rdquo; marks
            a projection made from preseason expectations because a team had not
            played before this game.
          </p>
        </div>
      )}
    </div>
  );
}
