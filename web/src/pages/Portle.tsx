import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  Radar,
  RadarChart,
  PolarGrid,
  PolarAngleAxis,
  PolarRadiusAxis,
  ResponsiveContainer,
} from 'recharts';
import { fetchPlayers, fetchPlayerDetail, fetchPortleDaily, type PlayerRow } from '../api/client';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { useIsMobile } from '../components/useIsMobile';
import { PlayerPicker } from '../components/PlayerPicker';
import { GuessGrid, type GuessRow } from '../components/portle/GuessGrid';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';
import { resolveAxes } from '../components/radarAxes';
import { loadJson, saveJson } from '../lib/localStore';
import {
  MAX_GUESSES,
  MODE_LABELS,
  MODE_ORDER,
  buildShare,
  compareGuess,
  isCorrect,
  localDateKey,
  poolHasSeedKeys,
  practiceAnswerByNonce,
  puzzleKey,
  recordResult,
  EMPTY_STATS,
  type GameMode,
  type GameStatus,
  type MbStats,
  type SavedGame,
} from '../lib/portle';

const CONFIG_KEY = 'mb:config';
const STATS_KEY = 'mb:stats';
const stateKeyFor = (key: string) => `mb:state:${key}`;

// A fresh random seed for each practice game. Generated in event handlers (never
// during render), so every session/roll is genuinely random rather than walking
// a fixed sequence.
const makeSeed = () => Math.floor(Math.random() * 0x7fffffff);

type AnswerDetail = Awaited<ReturnType<typeof fetchPlayerDetail>>;

function initialMode(): GameMode {
  const cfg = loadJson<{ mode?: string }>(CONFIG_KEY, {});
  return (MODE_ORDER as readonly string[]).includes(cfg.mode ?? '')
    ? (cfg.mode as GameMode)
    : 'p5';
}

export default function Portle() {
  usePageTitle('Portle');
  const isMobile = useIsMobile();
  const { season } = useSeason();

  // Today's puzzle flips at the player's local midnight (Wordle convention).
  const dateKey = useMemo(() => localDateKey(new Date()), []);

  // A shared practice link carries `?seed=&mode=&season=` — the seed reproduces
  // the exact answer client-side without naming it, so friends play the same
  // round. Read once at mount to seed initial state.
  const [searchParams] = useSearchParams();
  const sharedSeedRaw = searchParams.get('seed');
  const sharedSeed =
    sharedSeedRaw != null && Number.isFinite(Number(sharedSeedRaw))
      ? Math.floor(Number(sharedSeedRaw))
      : null;
  const sharedMode = searchParams.get('mode');

  const [mode, setMode] = useState<GameMode>(() =>
    sharedMode && (MODE_ORDER as readonly string[]).includes(sharedMode)
      ? (sharedMode as GameMode)
      : initialMode(),
  );
  const [practice, setPractice] = useState(() => sharedSeed != null);
  const [practiceSeed, setPracticeSeed] = useState(() => sharedSeed ?? 0);

  const [pool, setPool] = useState<PlayerRow[]>([]);
  // Track which season `pool` belongs to so a mid-flight season change never
  // seeds a puzzle from the wrong season's players.
  const [poolSeason, setPoolSeason] = useState<number | null>(null);

  const [stats, setStats] = useState<MbStats>(() => loadJson(STATS_KEY, EMPTY_STATS));

  // The daily answer is chosen and frozen SERVER-side (issue #181), so every
  // client plays the identical puzzle and it never moves once pinned. We only
  // fetch the pinned `natstat_id` and resolve it in the already-loaded pool.
  // Each result is tagged with the `key` (mode:season:date) it belongs to, so a
  // stale in-flight response for a previous puzzle is ignored by comparison
  // rather than cleared with an in-effect setState (repo bans set-state-in-effect).
  // `{ natstat_id: null }` = no eligible players for that pool.
  const [dailyPin, setDailyPin] = useState<{ key: string; natstat_id: string | null } | null>(null);
  const [dailyPinErrorKey, setDailyPinErrorKey] = useState<string | null>(null);

  // ----- pool load (per season) -----
  useEffect(() => {
    let cancelled = false;
    fetchPlayers({ season, limit: 5000 })
      .then((r) => {
        if (cancelled) return;
        setPool(r.players);
        setPoolSeason(season);
      })
      .catch((err) => console.error('Portle pool load failed', err));
    return () => {
      cancelled = true;
    };
  }, [season]);

  const dailyKey = useMemo(() => puzzleKey(mode, season, dateKey), [mode, season, dateKey]);

  // ----- daily pin fetch (daily mode only; practice is a local random roll) -----
  useEffect(() => {
    if (practice) return;
    let cancelled = false;
    fetchPortleDaily(mode, season, dateKey)
      .then((r) => {
        if (!cancelled) setDailyPin({ key: dailyKey, natstat_id: r.natstat_id });
      })
      .catch(() => {
        if (!cancelled) setDailyPinErrorKey(dailyKey);
      });
    return () => {
      cancelled = true;
    };
  }, [practice, mode, season, dateKey, dailyKey]);

  const poolReady = poolSeason === season && pool.length > 0;
  // The daily answer is resolved by `natstat_id`, so the pool must carry it. If
  // the API is mid-deploy and hasn't started serving it (issue #181), fail closed
  // rather than render off an inconsistent pool.
  const seedReady = poolReady && poolHasSeedKeys(pool);
  // Only trust a pin/error tagged with the CURRENT puzzle key (ignore stale ones).
  const pinForToday = !practice && dailyPin?.key === dailyKey ? dailyPin : null;
  const pinErrorForToday = !practice && dailyPinErrorKey === dailyKey;
  const dailyReady = practice || pinForToday !== null;

  const answer = useMemo(() => {
    if (!seedReady) return null;
    if (practice) return practiceAnswerByNonce(pool, mode, practiceSeed);
    // Daily: resolve the server-pinned id against the loaded pool.
    if (!pinForToday || pinForToday.natstat_id == null) return null;
    return pool.find((p) => p.natstat_id === pinForToday.natstat_id) ?? null;
  }, [seedReady, practice, pool, mode, practiceSeed, pinForToday]);

  // Remounts the game (fresh state) whenever the puzzle identity changes.
  // Practice includes `mode` + the random seed so switching pools or rolling a
  // new game starts fresh; daily already carries mode+season in `dailyKey`.
  const gameKey = practice ? `practice:${mode}:${practiceSeed}` : dailyKey;

  const handleModeChange = (next: GameMode) => {
    if (next === mode) return;
    setMode(next);
    saveJson(CONFIG_KEY, { mode: next });
  };

  const selectPractice = (toPractice: boolean) => {
    if (toPractice === practice) return;
    setPractice(toPractice);
    if (toPractice) setPracticeSeed(makeSeed());
  };

  const recordDaily = (won: boolean, guesses: number, hintsUsed: number) => {
    const ns = recordResult(stats, { won, guesses, hintsUsed });
    setStats(ns);
    saveJson(STATS_KEY, ns);
  };

  const winPct = stats.played > 0 ? Math.round((stats.wins / stats.played) * 100) : 0;

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold text-gray-100">Portle</h1>
          <p className="text-sm text-gray-400">
            Guess the mystery player in {MAX_GUESSES} tries. Each guess reveals how it
            compares — <span className="text-emerald-300">green</span> = exact,{' '}
            <span className="text-amber-300">amber</span> = close / archetype overlap, and{' '}
            <span className="text-gray-300">▲ / ▼</span> point toward the answer.
          </p>
        </div>
        <div className="inline-flex overflow-hidden rounded-md border border-gray-700 text-sm">
          <button
            type="button"
            onClick={() => selectPractice(false)}
            className={`px-3 py-1.5 ${
              !practice ? 'bg-blue-600 text-white' : 'bg-gray-900 text-gray-300 hover:bg-gray-800'
            }`}
          >
            Daily
          </button>
          <button
            type="button"
            onClick={() => selectPractice(true)}
            className={`px-3 py-1.5 ${
              practice ? 'bg-blue-600 text-white' : 'bg-gray-900 text-gray-300 hover:bg-gray-800'
            }`}
          >
            Practice
          </button>
        </div>
      </div>

      {/* Mode selector */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs uppercase tracking-wide text-gray-500">Pool</span>
        <div className="inline-flex overflow-hidden rounded-md border border-gray-700 text-xs">
          {MODE_ORDER.map((m) => (
            <button
              key={m}
              type="button"
              onClick={() => handleModeChange(m)}
              className={`px-3 py-1 ${
                mode === m ? 'bg-blue-600 text-white' : 'bg-gray-900 text-gray-300 hover:bg-gray-800'
              }`}
            >
              {MODE_LABELS[m]}
            </button>
          ))}
        </div>
        <span className="text-xs text-gray-500">
          Season {season}
          {!practice && ` · ${dateKey}`}
        </span>
        {practice && (
          <button
            type="button"
            onClick={() => setPracticeSeed(makeSeed())}
            className="rounded border border-gray-600 bg-gray-800 px-3 py-1 text-xs text-gray-200 hover:bg-gray-700"
          >
            New game
          </button>
        )}
      </div>

      {!poolReady ? (
        <div className="text-sm text-gray-500">Loading players…</div>
      ) : !seedReady ? (
        <div className="rounded-lg border border-gray-700 bg-gray-800/60 p-6 text-sm text-gray-400">
          The player data is refreshing — today's puzzle will be here in a minute. Reload shortly.
        </div>
      ) : pinErrorForToday ? (
        <div className="rounded-lg border border-gray-700 bg-gray-800/60 p-6 text-sm text-gray-400">
          Couldn't load today's puzzle. Please reload in a moment.
        </div>
      ) : !dailyReady ? (
        <div className="text-sm text-gray-500">Loading today's puzzle…</div>
      ) : !answer ? (
        <div className="rounded-lg border border-gray-700 bg-gray-800/60 p-6 text-sm text-gray-400">
          Not enough players in this pool for {MODE_LABELS[mode]} · {season}. Try another mode
          or season.
        </div>
      ) : (
        <MysteryGame
          key={gameKey}
          answer={answer}
          pool={pool}
          season={season}
          mode={mode}
          isDaily={!practice}
          dateKey={dateKey}
          seed={practiceSeed}
          stateKey={stateKeyFor(dailyKey)}
          isMobile={isMobile}
          onDailyFinish={recordDaily}
          onPlayAgain={() => setPracticeSeed(makeSeed())}
        />
      )}

      {/* Stats (daily only) */}
      {!practice && stats.played > 0 && (
        <div className="flex flex-wrap gap-4 rounded-lg bg-gray-800/60 px-4 py-3 text-sm">
          <Stat label="Played" value={String(stats.played)} />
          <Stat label="Win %" value={`${winPct}%`} />
          <Stat label="Streak" value={String(stats.streak)} />
          <Stat label="Max streak" value={String(stats.maxStreak)} />
          <Stat label="Clean solves" value={String(stats.cleanWins ?? 0)} />
        </div>
      )}
    </div>
  );
}

/** One puzzle's playable state. Remounted (via `key`) whenever the puzzle
 *  identity changes, so daily-restore and practice-reset both fall out of
 *  `useState` initializers — no state-syncing effects. */
function MysteryGame({
  answer,
  pool,
  season,
  mode,
  isDaily,
  dateKey,
  seed,
  stateKey,
  isMobile,
  onDailyFinish,
  onPlayAgain,
}: {
  answer: PlayerRow;
  pool: PlayerRow[];
  season: number;
  mode: GameMode;
  isDaily: boolean;
  dateKey: string;
  seed: number;
  stateKey: string;
  isMobile: boolean;
  onDailyFinish: (won: boolean, guesses: number, hintsUsed: number) => void;
  onPlayAgain: () => void;
}) {
  const [guessedRows, setGuessedRows] = useState<PlayerRow[]>(() => {
    if (!isDaily) return [];
    const saved = loadJson<SavedGame | null>(stateKey, null);
    if (!saved || saved.guesses.length === 0) return [];
    const byId = new Map(pool.map((p) => [p.player_id, p]));
    return saved.guesses.map((id) => byId.get(id)).filter((p): p is PlayerRow => !!p);
  });
  const [status, setStatus] = useState<GameStatus>(() =>
    isDaily ? loadJson<SavedGame | null>(stateKey, null)?.status ?? 'playing' : 'playing',
  );
  // Reveal-panel hints opened while playing — using one costs a "clean solve".
  const [openHints, setOpenHints] = useState<Set<string>>(() => {
    if (!isDaily) return new Set();
    return new Set(loadJson<SavedGame | null>(stateKey, null)?.hints ?? []);
  });
  const [answerDetail, setAnswerDetail] = useState<AnswerDetail | null>(null);
  const [copied, setCopied] = useState(false);

  const persist = (rows: PlayerRow[], st: GameStatus, hints: Set<string>) => {
    if (!isDaily) return;
    saveJson<SavedGame>(stateKey, {
      guesses: rows.map((r) => r.player_id),
      status: st,
      hints: [...hints],
    });
  };

  // Reveal panel inputs (radar + shot diet). One fetch per puzzle.
  useEffect(() => {
    let cancelled = false;
    fetchPlayerDetail(answer.player_id, season)
      .then((d) => {
        if (!cancelled) setAnswerDetail(d);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [answer.player_id, season]);

  const handleGuess = (player: PlayerRow) => {
    if (status !== 'playing') return;
    if (guessedRows.some((g) => g.player_id === player.player_id)) return;
    const nextRows = [...guessedRows, player];
    const won = isCorrect(player, answer);
    const lost = !won && nextRows.length >= MAX_GUESSES;
    const nextStatus: GameStatus = won ? 'won' : lost ? 'lost' : 'playing';
    setGuessedRows(nextRows);
    setStatus(nextStatus);
    persist(nextRows, nextStatus, openHints);
    if (isDaily && nextStatus !== 'playing') {
      onDailyFinish(won, nextRows.length, openHints.size);
    }
  };

  const openHint = (keyName: string) => {
    if (status !== 'playing' || openHints.has(keyName)) return;
    const next = new Set(openHints).add(keyName);
    setOpenHints(next);
    persist(guessedRows, status, next);
  };

  const handleGiveUp = () => {
    if (status !== 'playing') return;
    setStatus('lost');
    persist(guessedRows, 'lost', openHints);
    if (isDaily) onDailyFinish(false, guessedRows.length, openHints.size);
  };

  const guessRows: GuessRow[] = useMemo(
    () => guessedRows.map((g) => ({ player: g, cells: compareGuess(g, answer) })),
    [guessedRows, answer],
  );

  const radarData = useMemo(() => {
    if (!answerDetail) return [];
    return resolveAxes({
      season_stats: answerDetail.season_stats,
      percentiles: answerDetail.percentiles,
      torvik_stats: answerDetail.torvik_stats,
    }).map((a) => ({ stat: a.stat, v: a.value }));
  }, [answerDetail]);

  const solved = status !== 'playing';
  const won = status === 'won';
  const guessesLeft = MAX_GUESSES - guessedRows.length;
  const hintsUsed = openHints.size;
  const answerHref = `/players/${answer.player_id}?season=${season}`;

  const handleShare = () => {
    // Daily shares the bare page (everyone gets today's puzzle). Practice
    // encodes mode+season+seed so a friend replays the exact same answer —
    // the seed reproduces the pick client-side without naming the player.
    const base = `${window.location.origin}/portle`;
    const url = isDaily
      ? base
      : `${base}?${new URLSearchParams({ mode, season: String(season), seed: String(seed) }).toString()}`;
    const text = buildShare(
      guessRows.map((r) => r.cells),
      { mode, won, hintsUsed, daily: isDaily, dateKey, url },
    );
    navigator.clipboard
      ?.writeText(text)
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
  };

  // Reveal-panel bodies (shared by the in-play dropdowns and the post-game
  // auto-expanded view).
  const radarBody =
    radarData.length > 0 ? (
      <div className="min-h-0 flex-1" style={{ minHeight: isMobile ? 240 : 300 }}>
        <ResponsiveContainer width="100%" height="100%">
          <RadarChart data={radarData} outerRadius="80%" margin={{ top: 12, right: 24, bottom: 12, left: 24 }}>
            <PolarGrid stroke="#475569" />
            <PolarAngleAxis dataKey="stat" tick={{ fill: '#9ca3af', fontSize: 11 }} />
            <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
            <Radar dataKey="v" stroke="#3b82f6" fill="#3b82f6" fillOpacity={0.35} />
          </RadarChart>
        </ResponsiveContainer>
      </div>
    ) : (
      <div className="flex min-h-[240px] flex-1 items-center justify-center text-xs text-gray-600">
        Loading profile…
      </div>
    );

  const shotBody = answerDetail?.torvik_stats ? (
    <div className="space-y-3">
      <div className="flex justify-center">
        <ShotDietCourt torvik={answerDetail.torvik_stats} />
      </div>
      <ShotDistributionBar torvik={answerDetail.torvik_stats} />
    </div>
  ) : (
    <div className="flex h-[220px] items-center justify-center text-xs text-gray-600">
      No shot data
    </div>
  );

  return (
    <>
      {/* Mystery player header */}
      <div className="flex items-center justify-between rounded-lg bg-gray-800 px-4 py-3">
        <span className="text-sm font-semibold text-gray-300">Mystery player</span>
        <span className="text-base font-bold text-gray-100">
          {solved ? (
            <>
              <a
                href={answerHref}
                target="_blank"
                rel="noopener noreferrer"
                className="hover:text-blue-300 hover:underline"
              >
                {answer.name}
              </a>
              {answer.team_name ? (
                <span className="ml-1 text-sm font-normal text-gray-400">· {answer.team_name}</span>
              ) : null}
            </>
          ) : (
            '???'
          )}
        </span>
      </div>

      {/* Optional hint reveals (each opened while playing costs a clean solve) */}
      <div className="grid gap-3 lg:grid-cols-2">
        <HintPanel
          label="Scouting radar"
          revealed={solved || openHints.has('radar')}
          used={openHints.has('radar')}
          locked={solved}
          onReveal={() => openHint('radar')}
        >
          {radarBody}
        </HintPanel>
        <HintPanel
          label="Shot diet"
          revealed={solved || openHints.has('shot')}
          used={openHints.has('shot')}
          locked={solved}
          onReveal={() => openHint('shot')}
        >
          {shotBody}
        </HintPanel>
      </div>

      {/* Guess input + status */}
      {!solved ? (
        <div className="mt-4 space-y-2">
          <PlayerPicker
            season={season}
            onPick={handleGuess}
            existingIds={guessedRows.map((g) => g.player_id)}
            placeholder="Guess a player by name…"
            hideStats
          />
          <div className="flex items-center justify-between">
            <span className="text-xs text-gray-500">
              {guessesLeft} {guessesLeft === 1 ? 'guess' : 'guesses'} left
            </span>
            <button
              type="button"
              onClick={handleGiveUp}
              className="rounded border border-gray-700 px-3 py-1 text-xs text-gray-400 hover:border-rose-500/50 hover:text-rose-300"
            >
              Give up
            </button>
          </div>
        </div>
      ) : (
        <div
          className={`mt-4 rounded-lg border p-4 ${
            won ? 'border-emerald-500/40 bg-emerald-600/15' : 'border-rose-500/40 bg-rose-600/15'
          }`}
        >
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="text-sm text-gray-100">
              {won ? (
                <>
                  Solved in {guessedRows.length}{' '}
                  {guessedRows.length === 1 ? 'guess' : 'guesses'}
                  {hintsUsed === 0 ? (
                    <span className="text-emerald-300"> · clean, no hints</span>
                  ) : (
                    <span className="text-amber-300">
                      {' '}· {hintsUsed} {hintsUsed === 1 ? 'hint' : 'hints'}
                    </span>
                  )}
                  ! It was{' '}
                  <a
                    href={answerHref}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="font-bold hover:text-blue-300 hover:underline"
                  >
                    {answer.name}
                  </a>
                  .
                </>
              ) : (
                <>
                  The answer was{' '}
                  <a
                    href={answerHref}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="font-bold hover:text-blue-300 hover:underline"
                  >
                    {answer.name}
                  </a>
                  {answer.team_name ? ` (${answer.team_name})` : ''}.
                </>
              )}
            </div>
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={handleShare}
                className="rounded bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-500"
              >
                {copied ? 'Copied!' : 'Share'}
              </button>
              {!isDaily && (
                <button
                  type="button"
                  onClick={onPlayAgain}
                  className="rounded border border-gray-600 bg-gray-800 px-3 py-1.5 text-sm text-gray-200 hover:bg-gray-700"
                >
                  Play again
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Guess grid */}
      <div className="mt-4">
        <GuessGrid rows={guessRows} season={season} />
      </div>
    </>
  );
}

/** A reveal panel that stays collapsed until the player opts in (spending a
 *  "clean solve"), then shows its chart. After the game ends (`locked`) it is
 *  shown outright with no toggle and no cost. */
function HintPanel({
  label,
  revealed,
  used,
  locked,
  onReveal,
  children,
}: {
  label: string;
  revealed: boolean;
  used: boolean;
  locked: boolean;
  onReveal: () => void;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col rounded-lg bg-gray-800 p-4">
      <div className="mb-2 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-300">{label}</h2>
        {used && !locked && (
          <span className="text-[10px] uppercase tracking-wide text-amber-300">hint used</span>
        )}
      </div>
      {revealed ? (
        <div className="flex min-h-0 flex-1 flex-col">{children}</div>
      ) : (
        <button
          type="button"
          onClick={onReveal}
          className="flex h-[220px] w-full flex-col items-center justify-center rounded border border-dashed border-gray-600 text-sm text-gray-400 hover:border-gray-500 hover:text-gray-200"
        >
          <span className="font-medium">Reveal {label.toLowerCase()}</span>
          <span className="mt-1 text-xs text-gray-500">Costs your clean solve</span>
        </button>
      )}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      <span className="text-lg font-bold text-gray-100">{value}</span>
      <span className="text-xs text-gray-500">{label}</span>
    </div>
  );
}
