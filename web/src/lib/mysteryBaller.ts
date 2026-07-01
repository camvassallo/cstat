// Mystery Baller — pure game logic (no React, no DOM). Kept side-effect-free
// so it's trivially unit-testable and reused by both the daily and practice
// modes. The page layer (pages/MysteryBaller.tsx) owns fetching, state, and
// persistence; everything deterministic lives here.

import type { PlayerRow } from '../api/client';

// ---------------------------------------------------------------------------
// Config / eligibility
// ---------------------------------------------------------------------------

/** Branded title for the shareable result (the page header just says "Mystery
 *  Baller" — CamPom is already in the navbar). Uses the same ` · ` separator as
 *  the rest of the share header. */
export const GAME_TITLE = 'CamPom · Mystery Baller';

export type GameMode = 'p5' | 'starters' | 'all';

export const MODE_LABELS: Record<GameMode, string> = {
  p5: 'Power 5',
  starters: 'Starters',
  all: 'All D-I',
};

export const MODE_ORDER: readonly GameMode[] = ['p5', 'starters', 'all'];

export const MAX_GUESSES = 10;

// Exact NatStat conference codes for the power conferences, verified against
// the live `/api/players` data (2026): ACC / Big Ten / Big 12 / SEC + Big
// East. Pac-12 no longer fields a men's league, so there's no PAC12 code.
const P5_CONFERENCES = new Set(['ACC', 'BIG10', 'BIG12', 'SEC', 'BIGEAST']);

// Minutes floors on top of the API's GP>=5 & MPG>=10 gate, so answers are
// recognizable. P5 lifts it modestly (drop deep-bench power-conf players);
// Starters lifts it further to genuine starter minutes.
const P5_MIN_MPG = 20;
const STARTER_MIN_MPG = 24;

/** A player is answerable only if we have the fields the grid + reveal lean
 *  on: a CamPom value and a primary archetype. (Guesses aren't restricted —
 *  the search box can surface anyone; only the hidden answer must be rich.) */
export function isAnswerable(p: PlayerRow): boolean {
  return p.campom != null && p.primary_class != null;
}

/** Filter a loaded season pool down to the eligible answer set for a mode. */
export function filterPool(pool: PlayerRow[], mode: GameMode): PlayerRow[] {
  return pool.filter((p) => {
    if (!isAnswerable(p)) return false;
    if (mode === 'p5') {
      return (
        p.conference != null &&
        P5_CONFERENCES.has(p.conference) &&
        p.minutes_per_game != null &&
        p.minutes_per_game >= P5_MIN_MPG
      );
    }
    if (mode === 'starters') {
      return p.minutes_per_game != null && p.minutes_per_game >= STARTER_MIN_MPG;
    }
    return true;
  });
}

// ---------------------------------------------------------------------------
// Daily seed
// ---------------------------------------------------------------------------

/** FNV-1a 32-bit hash → unsigned int. Deterministic, tiny, good enough to
 *  spread dates across a pool with no clustering. */
export function hash32(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Local calendar date as `YYYY-MM-DD`. Local (not UTC) so "today's puzzle"
 *  flips at the player's midnight, the Wordle convention. */
export function localDateKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/** Stable identifier for one daily puzzle — the persistence + seed key. Same
 *  (mode, season, date) → same puzzle for everyone in that mode. */
export function puzzleKey(mode: GameMode, season: number, dateKey: string): string {
  return `${mode}:${season}:${dateKey}`;
}

/** Pick the daily answer: seed an index into the mode-filtered pool sorted by
 *  a per-player rendezvous hash (min of `hash32(salt:player_id)`).
 *  Returns null when the eligible pool is empty. */
export function dailyAnswer(
  pool: PlayerRow[],
  mode: GameMode,
  season: number,
  dateKey: string,
): PlayerRow | null {
  return pickByHash(filterPool(pool, mode), puzzleKey(mode, season, dateKey));
}

/** Pick a random answer for practice mode. `rng` is injectable for tests;
 *  defaults to Math.random. Returns null when the eligible pool is empty. */
export function randomAnswer(
  pool: PlayerRow[],
  mode: GameMode,
  rng: () => number = Math.random,
): PlayerRow | null {
  const eligible = filterPool(pool, mode);
  if (eligible.length === 0) return null;
  return eligible[Math.floor(rng() * eligible.length)];
}

/** Deterministic practice pick keyed by a seed (random per game, or from a
 *  shared link). Same rendezvous-hash scheme as the daily so the pick is stable
 *  against unrelated pool churn. Returns null when the eligible pool is empty. */
export function practiceAnswerByNonce(
  pool: PlayerRow[],
  mode: GameMode,
  nonce: number,
): PlayerRow | null {
  return pickByHash(filterPool(pool, mode), `practice:${nonce}`);
}

/** Rendezvous ("highest random weight") selection: hash `salt:player_id` for
 *  every candidate and keep the minimum, tie-broken by `player_id`. Unlike
 *  `hash(salt) % length`, adding/removing OTHER candidates doesn't change the
 *  winner — only the winner entering/leaving the pool does — so the daily answer
 *  stays put across filter tweaks and roster churn. Order-independent. */
function pickByHash(eligible: PlayerRow[], salt: string): PlayerRow | null {
  if (eligible.length === 0) return null;
  let best = eligible[0];
  let bestH = hash32(`${salt}:${best.player_id}`);
  for (let i = 1; i < eligible.length; i++) {
    const p = eligible[i];
    const h = hash32(`${salt}:${p.player_id}`);
    if (h < bestH || (h === bestH && p.player_id < best.player_id)) {
      best = p;
      bestH = h;
    }
  }
  return best;
}

// ---------------------------------------------------------------------------
// Guess feedback
// ---------------------------------------------------------------------------

export type CellState = 'hit' | 'close' | 'miss';
export type Arrow = 'up' | 'down' | null;

export interface GuessCell {
  key: string;
  label: string;
  /** Rendered value for this guess (e.g. "SEC", "18.4"). */
  display: string;
  state: CellState;
  /** For numeric/ordinal columns: which way the ANSWER lies from the guess. */
  arrow: Arrow;
}

const CLASS_ORDINAL: Record<string, number> = {
  fr: 1,
  freshman: 1,
  so: 2,
  soph: 2,
  sophomore: 2,
  jr: 3,
  junior: 3,
  sr: 4,
  senior: 4,
  gr: 5,
  grad: 5,
  graduate: 5,
};

function classOrdinal(cy: string | null): number | null {
  if (!cy) return null;
  const k = cy.trim().toLowerCase();
  return CLASS_ORDINAL[k] ?? null;
}

/** Exact-match categorical cell (team / conference / position). */
function categoricalCell(
  key: string,
  label: string,
  guessVal: string | null,
  answerVal: string | null,
): GuessCell {
  const state: CellState =
    guessVal != null && answerVal != null && guessVal === answerVal ? 'hit' : 'miss';
  return { key, label, display: guessVal ?? '—', state, arrow: null };
}

/** Numeric cell with a "close" band and a direction arrow toward the answer. */
function numericCell(
  key: string,
  label: string,
  guessVal: number | null,
  answerVal: number | null,
  opts: { tight: number; band: number; format: (v: number) => string },
): GuessCell {
  if (guessVal == null || answerVal == null) {
    return { key, label, display: guessVal != null ? opts.format(guessVal) : '—', state: 'miss', arrow: null };
  }
  const diff = answerVal - guessVal;
  const ad = Math.abs(diff);
  const state: CellState = ad <= opts.tight ? 'hit' : ad <= opts.band ? 'close' : 'miss';
  const arrow: Arrow = state === 'hit' ? null : diff > 0 ? 'up' : 'down';
  return { key, label, display: opts.format(guessVal), state, arrow };
}

/** Archetype cell: primary match = hit; any primary/secondary overlap = close;
 *  else miss. */
function archetypeCell(guess: PlayerRow, answer: PlayerRow): GuessCell {
  const gp = guess.primary_class;
  const gs = guess.secondary_class;
  const ap = answer.primary_class;
  const as = answer.secondary_class;
  let state: CellState = 'miss';
  if (gp != null && ap != null && gp === ap) {
    state = 'hit';
  } else if (
    (gp != null && (gp === ap || gp === as)) ||
    (gs != null && (gs === ap || gs === as))
  ) {
    state = 'close';
  }
  return { key: 'archetype', label: 'Archetype', display: gp ?? '—', state, arrow: null };
}

function classYearCell(guess: PlayerRow, answer: PlayerRow): GuessCell {
  const g = classOrdinal(guess.class_year);
  const a = classOrdinal(answer.class_year);
  if (g == null || a == null) {
    return { key: 'class', label: 'Class', display: guess.class_year ?? '—', state: 'miss', arrow: null };
  }
  const state: CellState = g === a ? 'hit' : 'miss';
  const arrow: Arrow = state === 'hit' ? null : a > g ? 'up' : 'down';
  return { key: 'class', label: 'Class', display: guess.class_year ?? '—', state, arrow };
}

const fmt1 = (v: number) => v.toFixed(1);
// usage_rate is stored as a fraction (0.297 = 29.7%); show it as a whole-number
// percent to match the rest of the app.
const fmtUsage = (v: number) => `${Math.round(v * 100)}%`;
// Height inches → feet'inches" (e.g. 81 → 6'9").
const fmtHeight = (v: number) => `${Math.floor(v / 12)}'${Math.round(v % 12)}"`;

/** Column headers, in the same order `compareGuess` emits cells. Shared by the
 *  grid header row. */
export const GUESS_COLUMNS: ReadonlyArray<{ key: string; label: string }> = [
  { key: 'team', label: 'Team' },
  { key: 'conference', label: 'Conf' },
  { key: 'class', label: 'Class' },
  { key: 'height', label: 'Height' },
  { key: 'archetype', label: 'Archetype' },
  { key: 'ppg', label: 'PPG' },
  { key: 'usage', label: 'Usage' },
  { key: 'campom', label: 'CamPom' },
];

/** Build the full row of attribute cells comparing a guess to the answer.
 *  Column order here is the render + share order. */
export function compareGuess(guess: PlayerRow, answer: PlayerRow): GuessCell[] {
  return [
    categoricalCell('team', 'Team', guess.team_name, answer.team_name),
    categoricalCell('conference', 'Conf', guess.conference, answer.conference),
    classYearCell(guess, answer),
    numericCell('height', 'Height', guess.height_inches, answer.height_inches, {
      // Within 1 inch = hit, within 3 = close.
      tight: 1,
      band: 3,
      format: fmtHeight,
    }),
    archetypeCell(guess, answer),
    numericCell('ppg', 'PPG', guess.ppg, answer.ppg, { tight: 0.5, band: 2, format: fmt1 }),
    numericCell('usage', 'Usage', guess.usage_rate, answer.usage_rate, {
      // Fraction scale: within 1 pp = hit, within 3 pp = close.
      tight: 0.01,
      band: 0.03,
      format: fmtUsage,
    }),
    numericCell('campom', 'CamPom', guess.campom, answer.campom, {
      tight: 0.5,
      band: 2,
      format: fmt1,
    }),
  ];
}

export function isCorrect(guess: PlayerRow, answer: PlayerRow): boolean {
  return guess.player_id === answer.player_id;
}

// ---------------------------------------------------------------------------
// Share string
// ---------------------------------------------------------------------------

const SHARE_EMOJI: Record<CellState, string> = {
  hit: '🟩',
  close: '🟨',
  miss: '⬛',
};

/** Wordle-style shareable result. No answer leak — only the colored grid.
 *  Header names the scope (the daily date, or "Practice") so a practice run
 *  can't pass as the daily; hinted solves are annotated so a clean solve
 *  stays visibly clean. */
export function buildShare(
  rows: GuessCell[][],
  opts: {
    mode: GameMode;
    won: boolean;
    url?: string;
    hintsUsed?: number;
    daily?: boolean;
    dateKey?: string;
  },
): string {
  const score = opts.won ? `${rows.length}/${MAX_GUESSES}` : `X/${MAX_GUESSES}`;
  const n = opts.hintsUsed ?? 0;
  const hint = n > 0 ? ` (${n} hint${n > 1 ? 's' : ''})` : '';
  const scope = opts.daily === false ? 'Practice' : opts.dateKey ?? 'Daily';
  const header = `${GAME_TITLE} · ${scope} · ${MODE_LABELS[opts.mode]} · ${score}${hint}`;
  const grid = rows.map((r) => r.map((c) => SHARE_EMOJI[c.state]).join('')).join('\n');
  return opts.url ? `${header}\n${grid}\n${opts.url}` : `${header}\n${grid}`;
}

// ---------------------------------------------------------------------------
// Persistence shapes
// ---------------------------------------------------------------------------

export type GameStatus = 'playing' | 'won' | 'lost';

/** Per-puzzle saved state (daily mode only). */
export interface SavedGame {
  /** Guessed player ids, in order. */
  guesses: string[];
  status: GameStatus;
  /** Reveal-panel hint keys opened while the game was still in play. */
  hints: string[];
}

export interface MbStats {
  played: number;
  wins: number;
  streak: number;
  maxStreak: number;
  /** Wins with zero hints opened — the "clean solve" counter. */
  cleanWins: number;
  /** Wins bucketed by guess count; index 0 = solved in 1, index 7 = solved in 8. */
  dist: number[];
}

export const EMPTY_STATS: MbStats = {
  played: 0,
  wins: 0,
  streak: 0,
  maxStreak: 0,
  cleanWins: 0,
  dist: new Array(MAX_GUESSES).fill(0),
};

/** Fold a finished daily result into the running stats (pure — the caller
 *  persists the return value and guards against double-recording a puzzle).
 *  `hintsUsed` defaults to 0 so older callers/tests stay valid. */
export function recordResult(
  stats: MbStats,
  outcome: { won: boolean; guesses: number; hintsUsed?: number },
): MbStats {
  const dist = stats.dist.slice();
  if (outcome.won && outcome.guesses >= 1 && outcome.guesses <= MAX_GUESSES) {
    dist[outcome.guesses - 1] += 1;
  }
  const streak = outcome.won ? stats.streak + 1 : 0;
  const clean = outcome.won && (outcome.hintsUsed ?? 0) === 0;
  return {
    played: stats.played + 1,
    wins: stats.wins + (outcome.won ? 1 : 0),
    streak,
    maxStreak: Math.max(stats.maxStreak, streak),
    cleanWins: (stats.cleanWins ?? 0) + (clean ? 1 : 0),
    dist: dist,
  };
}
