import { useEffect, useMemo, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import { conferenceLabel } from '../lib/conferences';
import {
  seasonHref,
  setPageSeasons,
  useAvailableSeasons,
  useSeason,
} from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import {
  Radar,
  RadarChart,
  PolarGrid,
  PolarAngleAxis,
  PolarRadiusAxis,
  ResponsiveContainer,
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  CartesianGrid,
  Legend,
} from 'recharts';
import {
  fetchPlayerCompare,
  type ComparePlayerResolved,
  type ComparePlayerUnavailable,
} from '../api/client';
import {
  clearSlotSeasons,
  idsHaveSlotSeasons,
  parseCompareIds,
  pinSlotSeasons,
  setSlotSeason,
  slotToken,
} from '../lib/compareSlots';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';
import { PlayerPicker } from '../components/PlayerPicker';
import { camTier, camTierColor, camHalfPctile } from '../components/cam';
import { ClassTooltip } from '../components/Archetype';
import { classColor, provisionalMeta } from '../components/archetypeColors';
import { useIsMobile } from '../components/useIsMobile';
import { resolveAxes } from '../components/radarAxes';
import { RadarAxisTooltip } from '../components/RadarAxisTooltip';
import { RadarTick } from '../components/RadarTick';
import { useDismissOnOutside } from '../components/useDismissOnOutside';
import ModeToggle from '../components/ModeToggle';

const PLAYER_COLORS = ['#3b82f6', '#f97316', '#22c55e', '#a855f7'];
const MAX_PLAYERS = 4;

/// Single-season | cross-year. `season` is the default, and in it this page
/// behaves exactly as it did before cross-year existed — same URL shape, same
/// global `?season=`, unavailable slots dropped rather than rendered.
type CompareMode = 'season' | 'year';

/// Wording is fixed by `ModeToggle`'s cross-year contract, so this page,
/// Predict and the comparable-players list all read the same. "Any year" over
/// "cross-year": the latter is how we talk about the work, not what the reader
/// is choosing between.
const MODE_OPTIONS = [
  { value: 'year' as const, label: 'Any year', title: 'Give each player their own season' },
  { value: 'season' as const, label: 'Season', title: 'Compare within one season' },
];

/// The empty override that hides the navbar season picker. A module constant
/// rather than a fresh `[]` per render — `setPageSeasons` de-dupes by value,
/// but there is no reason to hand it the work.
const NO_GLOBAL_SEASONS: readonly number[] = [];

/// A rendered column plus the `ids` token that asked for it. `slotId` is the
/// column's identity everywhere — key, removal, ordering. `player.id` is the
/// id the slot RESOLVED to, which is neither the requested UUID nor the
/// `<uuid>@<year>` token in the URL, and is not even unique per column: two
/// slots naming different seasons of the same human collapse onto one row
/// when both are pointed at the same year.
///
/// `color` rides on the entry instead of being read off `PLAYER_COLORS[i]` at
/// each use site, because in cross-year mode the rendered columns and the
/// stat-row cells are no longer the same list — an unavailable slot occupies a
/// column but contributes no cells, so indexing both by position would paint a
/// player's header and their numbers different colors.
/// `slotIndex` is the position in the `ids` param, and it is what removal and
/// the season pickers address. Not `slotId`: two tokens in a pasted link can
/// be byte-identical, and filtering by value would take out both columns.
type SlotKey = { slotId: string; slotIndex: number };
type FetchedSlot = (ComparePlayerResolved | ComparePlayerUnavailable) & SlotKey;
type SlotDecoration = SlotKey & { color: string };
type ResolvedSlot = ComparePlayerResolved & SlotDecoration;
type UnavailableSlot = ComparePlayerUnavailable & SlotDecoration;
type CompareSlotEntry = ResolvedSlot | UnavailableSlot;

const isResolved = (s: CompareSlotEntry): s is ResolvedSlot => s.available;

const fmt = (v: number | null | undefined, d = 1) =>
  v != null && Number.isFinite(v) ? v.toFixed(d) : '—';
const pct = (v: number | null | undefined) =>
  v != null && Number.isFinite(v) ? (v * 100).toFixed(1) + '%' : '—';
const pctVal = (v: number | null | undefined) =>
  v != null && Number.isFinite(v) ? `${v.toFixed(1)}%` : '—';

// Delta formatters for advantage chips (always render absolute value).
const dFmt1 = (n: number) => n.toFixed(1);
const dFmt2 = (n: number) => n.toFixed(2);
const dFmtPpFrac = (n: number) => (n * 100).toFixed(1) + 'pp';
const dFmtPpDirect = (n: number) => n.toFixed(1) + 'pp';

function heightString(inches: number | null | undefined) {
  if (inches == null) return null;
  return `${Math.floor(inches / 12)}'${inches % 12}"`;
}

type ChipTier = 'EDGE' | 'ADVANTAGE' | 'DOMINANT';

interface ChipInfo {
  tier: ChipTier;
  delta: string;
}

const CHIP_TIERS: Record<ChipTier, { label: string; classes: string; minGap: number }> = {
  EDGE: {
    label: 'EDGE',
    classes: 'bg-blue-900/50 text-blue-200 ring-1 ring-blue-500/40',
    minGap: 0.05,
  },
  ADVANTAGE: {
    label: 'ADV',
    classes: 'bg-amber-900/50 text-amber-200 ring-1 ring-amber-500/50',
    minGap: 0.15,
  },
  DOMINANT: {
    label: 'DOM',
    classes: 'bg-rose-900/60 text-rose-200 ring-1 ring-rose-500/60',
    minGap: 0.3,
  },
};

function tierForGap(gap: number): ChipTier | null {
  if (gap >= CHIP_TIERS.DOMINANT.minGap) return 'DOMINANT';
  if (gap >= CHIP_TIERS.ADVANTAGE.minGap) return 'ADVANTAGE';
  if (gap >= CHIP_TIERS.EDGE.minGap) return 'EDGE';
  return null;
}

function Chip({ tier, delta }: ChipInfo) {
  const cfg = CHIP_TIERS[tier];
  return (
    <span
      className={`inline-flex items-center gap-1 text-xs font-bold px-1.5 py-0.5 rounded leading-none ${cfg.classes}`}
      title={`${tier}${delta ? ` — +${delta} over runner-up` : ''}`}
    >
      {cfg.label}
      {delta && <span className="font-normal opacity-90">+{delta}</span>}
    </span>
  );
}

interface StatCellProps {
  value: string;
  pctile?: number | null;
  color: string;
  chip?: ChipInfo | null;
  /// Optional cell tooltip — used by CAMO/CAMD to flag the modeled percentile.
  title?: string;
  /// Swap the primary and secondary figures: percentile large, raw value
  /// beneath. Set for every cell once the compared columns span more than one
  /// season — see `spansSeasons` in the page body for why.
  leadWithPctile?: boolean;
}

function StatCell({ value, pctile, color, chip, title, leadWithPctile }: StatCellProps) {
  const p = pctile != null ? Math.max(0, Math.min(1, pctile)) : null;
  // No percentile means there is nothing to lead with — a cross-year cell
  // whose stat has no percentile (an unranked column, a missing row) still
  // shows the raw number rather than an em dash where the number was.
  const lead = leadWithPctile && p != null;
  return (
    <div title={title}>
      <div className="flex items-center justify-end gap-1.5">
        {chip && <Chip {...chip} />}
        <span className="font-medium text-sm">
          {lead ? Math.round(p * 100) : value}
        </span>
      </div>
      {lead && (
        <div className="text-[11px] text-gray-500 text-right leading-tight">{value}</div>
      )}
      {p != null && (
        <div className="mt-1 h-1 bg-gray-700 rounded overflow-hidden">
          <div
            className="h-1 rounded"
            style={{ width: `${Math.round(p * 100)}%`, background: color }}
          />
        </div>
      )}
    </div>
  );
}

interface StatRow {
  label: string;
  cells: StatCellProps[];
  raws?: (number | null | undefined)[];
  deltaFmt?: (n: number) => string;
}

function chipsForRow(row: StatRow): (ChipInfo | null)[] {
  const empty = row.cells.map(() => null as ChipInfo | null);
  const pcts = row.cells.map((c) =>
    c.pctile != null && Number.isFinite(c.pctile)
      ? Math.max(0, Math.min(1, c.pctile))
      : null,
  );
  const valid = pcts
    .map((p, i) => (p != null ? i : -1))
    .filter((i) => i >= 0);
  if (valid.length < 2) return empty;
  const sorted = [...valid].sort((a, b) => pcts[b]! - pcts[a]!);
  const leader = sorted[0];
  const runnerUp = sorted[1];
  const gap = pcts[leader]! - pcts[runnerUp]!;
  const tier = tierForGap(gap);
  if (!tier) return empty;
  const lr = row.raws?.[leader];
  const rr = row.raws?.[runnerUp];
  let delta = '';
  if (lr != null && rr != null && Number.isFinite(lr) && Number.isFinite(rr)) {
    const fmtFn = row.deltaFmt ?? dFmt1;
    delta = fmtFn(Math.abs(lr - rr));
  }
  return row.cells.map((_, i) =>
    i === leader ? { tier, delta } : null,
  );
}

function StatTable({
  title,
  rows,
  showChips,
  leadWithPctile = false,
}: {
  title: string;
  rows: StatRow[];
  showChips: boolean;
  leadWithPctile?: boolean;
}) {
  if (rows.length === 0) return null;
  const cols = rows[0].cells.length;
  return (
    <div className="bg-gray-800 rounded-lg p-5">
      <h2 className="text-lg font-bold mb-3">
        {title}
        {leadWithPctile && (
          <span className="block sm:inline sm:ml-2 text-xs font-normal text-gray-500">
            percentile, raw value below
          </span>
        )}
      </h2>
      <table className="w-full">
        <tbody>
          {rows.map((row, i) => {
            const chips = showChips ? chipsForRow(row) : row.cells.map(() => null);
            return (
              <tr key={i} className="border-b border-gray-700/60 last:border-0">
                <td className="py-2 pr-3 text-xs text-gray-400 w-24">{row.label}</td>
                {row.cells.map((cell, j) => (
                  <td
                    key={j}
                    className="py-2 px-2 text-right align-top"
                    style={{ width: `${(100 - 24) / cols}%` }}
                  >
                    <StatCell {...cell} chip={chips[j]} leadWithPctile={leadWithPctile} />
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function PlayerHeader({
  p,
  color,
  onRemove,
  showSeason,
}: {
  p: ComparePlayerResolved;
  color: string;
  onRemove: () => void;
  /// Cross-year: a name and a school are no longer enough to identify a
  /// column. Two of them can be the same player two years apart, and a
  /// transfer's school only means something alongside the year.
  showSeason: boolean;
}) {
  const { player } = p;
  const campom = p.torvik_stats?.campom ?? null;
  const campomPct = p.torvik_stats?.campom_pct ?? null;
  const tier = camTier(campom);
  const pctStr = campomPct != null ? Math.round(campomPct * 100) : null;
  const arch = p.archetype;
  const primaryClassColor = arch ? classColor(arch.primary_class) : null;
  return (
    <div
      className="bg-gray-800 rounded-lg p-4 flex items-start justify-between gap-3 border-l-4"
      style={{ borderLeftColor: color }}
    >
      <div className="min-w-0">
        {/* Anchor these on the SLOT's season, not the page's. A slot can be
            written `<uuid>@<year>`, and `player.id` is then the UUID for THAT
            year — a link carrying the page season would resolve the id back to
            a different season, or 404 on a player who wasn't in D-I then.
            `seasonHref` still omits the param on the default season, so
            same-season links are unchanged. */}
        <Link
          to={seasonHref(`/players/${player.id}`, p.season)}
          className="text-base font-bold hover:underline block truncate"
        >
          {player.name}
        </Link>
        <div className="text-xs text-gray-400 truncate">
          {player.team_id ? (
            <Link
              to={seasonHref(`/teams/${player.team_id}`, p.season)}
              className="hover:underline"
            >
              {player.team_name}
            </Link>
          ) : (
            player.team_name ?? 'Unknown'
          )}
          {showSeason && <span className="text-gray-300"> {p.season}</span>}
          {player.conference && (
            <span className="text-gray-500"> · {conferenceLabel(player.conference)}</span>
          )}
        </div>
        <div className="text-xs text-gray-500 mt-0.5 truncate">
          {[
            player.position,
            player.class_year,
            heightString(player.height_inches),
            p.season_stats?.games_played != null ? `${p.season_stats.games_played} GP` : null,
          ]
            .filter(Boolean)
            .join(' · ') || '—'}
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {arch && (
            <span
              className={`inline-flex items-center gap-1 text-xs font-bold uppercase tracking-wide ${
                provisionalMeta(arch).provisional ? 'opacity-70' : ''
              }`}
            >
              <ClassTooltip cls={arch.primary_class} extra={provisionalMeta(arch).note ?? undefined}>
                <span style={{ color: primaryClassColor ?? undefined }}>
                  {arch.primary_class}
                </span>
              </ClassTooltip>
              {arch.secondary_class && (
                <ClassTooltip cls={arch.secondary_class}>
                  <span
                    className="opacity-70"
                    style={{ color: classColor(arch.secondary_class) }}
                  >
                    / {arch.secondary_class}
                  </span>
                </ClassTooltip>
              )}
              {provisionalMeta(arch).shortYear && (
                <span className="text-gray-500 lowercase font-normal tracking-normal">
                  {provisionalMeta(arch).shortYear}
                </span>
              )}
            </span>
          )}
          {campom != null && (
            <span
              className={`inline-flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5 px-2 py-0.5 rounded border text-xs ${camTierColor(tier)}`}
              title="CAM: composite player valuation"
            >
              <span className="uppercase tracking-wide opacity-70">CAM</span>
              <span className="font-bold">{campom.toFixed(1)}</span>
              {pctStr != null && <span className="opacity-80 whitespace-nowrap">{pctStr} pct</span>}
              {tier && <span className="opacity-80 whitespace-nowrap">· {tier}</span>}
            </span>
          )}
        </div>
      </div>
      <button
        onClick={onRemove}
        className="text-gray-500 hover:text-red-400 text-lg leading-none px-1"
        aria-label="Remove player"
      >
        ×
      </button>
    </div>
  );
}

/// The column for a slot whose player has no row in the year it was pointed
/// at. Cross-year only: single-season mode drops such a slot, which is the
/// behaviour it has always had. Rendering it is the point — "not in Division I
/// in 2015" is a real, common answer to a cross-year question, and a silently
/// missing fourth column is not.
function UnavailableHeader({
  p,
  color,
  onRemove,
}: {
  p: ComparePlayerUnavailable;
  color: string;
  onRemove: () => void;
}) {
  return (
    <div
      className="bg-gray-800/50 rounded-lg p-4 flex items-start justify-between gap-3 border-l-4 border-dashed"
      style={{ borderLeftColor: color }}
    >
      <div className="min-w-0">
        <div className="text-base font-bold text-gray-300 truncate">
          {p.requested_name ?? 'This player'}
        </div>
        <div className="text-xs text-gray-500 mt-1">
          No Division I season in {p.season}.
        </div>
        {p.available_seasons.length > 0 && (
          <div className="text-xs text-gray-500 mt-1">
            Played in {p.available_seasons.slice().sort((a, b) => a - b).join(', ')}.
          </div>
        )}
      </div>
      <button
        onClick={onRemove}
        className="text-gray-500 hover:text-red-400 text-lg leading-none px-1"
        aria-label="Remove player"
      >
        ×
      </button>
    </div>
  );
}

/// The per-slot year control. Constrained to the seasons the player actually
/// has (`available_seasons`, `natstat_id ∪ torvik_pid`), so a year that cannot
/// resolve is never offered — the empty column is reachable only from a
/// pasted link, never from this menu.
function SlotSeasonSelect({
  season,
  seasons,
  color,
  label,
  onChange,
}: {
  season: number;
  seasons: number[];
  color: string;
  label: string;
  onChange: (next: number) => void;
}) {
  // A slot pointed at a year outside the list (a pasted link) still has to
  // show the year it is on, or the menu would silently claim otherwise.
  const options = seasons.includes(season) ? seasons : [season, ...seasons];
  return (
    <select
      value={season}
      onChange={(e) => onChange(Number(e.target.value))}
      aria-label={`Season for ${label}`}
      className="bg-gray-800 border rounded px-1 py-0.5 text-xs text-gray-200 focus:outline-none focus:border-blue-500"
      style={{ borderColor: color }}
    >
      {options
        .slice()
        .sort((a, b) => b - a)
        .map((y) => (
          <option key={y} value={y}>
            {y}
          </option>
        ))}
    </select>
  );
}

export default function PlayerCompare() {
  const { season } = useSeason();
  usePageTitle('Compare Players');
  const [searchParams, setSearchParams] = useSearchParams();
  const idsCsv = searchParams.get('ids') ?? '';
  const ids = useMemo(() => parseCompareIds(idsCsv), [idsCsv]);

  // The mode is READ OFF the tokens rather than stored beside them. Cross-year
  // pins every slot, so "some token carries an `@year`" is exactly "this link
  // was built in cross-year mode" — no second query param, and a single-season
  // URL keeps the shape it has today. The sync is one-way: it can only turn
  // cross-year ON (a pasted link arriving at an already-mounted page), because
  // leaving the mode strips the years, which reads correctly as single-season.
  const [mode, setMode] = useState<CompareMode>(() =>
    idsHaveSlotSeasons(parseCompareIds(idsCsv)) ? 'year' : 'season',
  );
  useEffect(() => {
    if (idsHaveSlotSeasons(ids)) setMode('year');
  }, [ids]);
  const crossYear = mode === 'year';

  // Cross-year: every slot carries its own year, so one site-wide season means
  // nothing. Publishing an empty list hides the navbar picker. The unmount
  // cleanup is what stops it staying hidden on whatever the user opens next —
  // the override is module state, not page state.
  useEffect(() => {
    setPageSeasons(crossYear ? NO_GLOBAL_SEASONS : null);
    return () => setPageSeasons(null);
  }, [crossYear]);

  const { seasons: allSeasons } = useAvailableSeasons();
  // Which season the ADD box searches, cross-year only. `players` rows are
  // season-scoped, so a search in 2026 can never surface a player whose last
  // season was 2015 — without its own control, half the players a cross-year
  // comparison exists for would be unreachable. A new slot lands on this year.
  const [searchSeason, setSearchSeason] = useState(season);

  const [slots, setSlots] = useState<FetchedSlot[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showChips, setShowChips] = useState(true);
  const [selectedAxis, setSelectedAxis] = useState<string | null>(null);
  const radarRef = useDismissOnOutside(selectedAxis !== null, () =>
    setSelectedAxis(null),
  );
  const isMobile = useIsMobile();
  // Phones can't fit 3–4 side-by-side comparison columns legibly, so cap the
  // picker at 2 on mobile. Non-destructive: a shared deep-link with more ids
  // still resolves and renders — we only stop the picker from adding more.
  const maxPlayers = isMobile ? 2 : MAX_PLAYERS;

  useEffect(() => {
    if (ids.length === 0) {
      setSlots([]);
      return;
    }
    setLoading(true);
    setError(null);
    fetchPlayerCompare(ids, season)
      .then((r) => {
        // The API returns exactly one entry per `ids` slot, in request order:
        // a slot that doesn't resolve comes back unavailable rather than
        // disappearing, so the array already lines up with `ids` by index.
        // Index is the only safe join — a slot may be written `<uuid>@<year>`,
        // which equals no id in the payload, and cross-season resolution can
        // hand back a different UUID than was asked for.
        setSlots(
          r.players.map((p, i) => ({
            ...p,
            slotId: ids[i] ?? p.requested_id,
            slotIndex: i,
          })),
        );
      })
      .catch((e) => setError(e.message ?? 'Failed to load comparison'))
      .finally(() => setLoading(false));
  }, [ids, season]);

  // Unavailable slots are rendered in cross-year mode and dropped in
  // single-season mode, which is the behaviour that page has always had.
  // Colors are assigned over the RENDERED list, so single-season mode still
  // starts at blue no matter which slot failed to resolve.
  const visibleSlots = useMemo<CompareSlotEntry[]>(() => {
    const kept = crossYear ? slots : slots.filter((s) => s.available);
    return kept.map((s, i) => ({ ...s, color: PLAYER_COLORS[i % PLAYER_COLORS.length] }));
  }, [slots, crossYear]);
  const players = useMemo(() => visibleSlots.filter(isResolved), [visibleSlots]);
  const droppedSlots = useMemo(
    () => (crossYear ? [] : slots.filter((s) => !s.available)),
    [slots, crossYear],
  );

  // Lead with percentiles once the columns disagree about the year. Raw rates
  // drift materially across eras — league TS% and eFG% are both up more than
  // three points since 2015, TOV% down nearly three — so a raw-vs-raw row a
  // decade wide quietly compares two different baselines. The within-season
  // percentile each row already carries IS the era-relative view.
  // Resolved columns only: a slot with no row contributes no cells to compare,
  // and one lone player next to an empty column is not a cross-era reading.
  const spansSeasons = useMemo(
    () => new Set(players.map((p) => p.season)).size > 1,
    [players],
  );

  // Preserve any other search params (notably `season`) when rewriting `ids`.
  const updateIds = (next: string[]) => {
    setSearchParams((prev) => {
      const p = new URLSearchParams(prev);
      if (next.length === 0) p.delete('ids');
      else p.set('ids', next.join(','));
      return p;
    });
  };

  const changeMode = (next: CompareMode) => {
    if (next === mode) return;
    if (next === 'year') {
      // Pin every slot to the season it is already rendered in, so switching
      // changes nothing on screen — the years become editable, not different.
      setSearchSeason(season);
      updateIds(pinSlotSeasons(ids, season));
    } else {
      updateIds(clearSlotSeasons(ids));
    }
    setMode(next);
  };

  const addPlayer = (id: string) => {
    const token = crossYear ? slotToken(id, searchSeason) : id;
    if (ids.includes(token) || ids.length >= maxPlayers) return;
    updateIds([...ids, token]);
  };
  const removeSlot = (index: number) => updateIds(ids.filter((_, i) => i !== index));
  const changeSlotSeason = (index: number, next: number) =>
    updateIds(setSlotSeason(ids, index, next));

  // Don't offer a player the comparison already holds — but in cross-year mode
  // that's per YEAR, since the same player in two seasons is the case the mode
  // exists for. Match on the resolved id, which is the season-scoped UUID the
  // search returns.
  const pickerExistingIds = useMemo(() => {
    if (!crossYear) return ids;
    return visibleSlots
      .filter((s) => s.season === searchSeason)
      .map((s) => (s.available ? s.player.id : s.requested_id));
  }, [crossYear, ids, visibleSlots, searchSeason]);

  /// Column captions: cross-year needs the year on every name, including in
  /// the chart legends where there is no header to read it off.
  const slotLabel = (p: ResolvedSlot) =>
    crossYear ? `${p.player.name} ${p.season}` : p.player.name;

  // ---------- table rows ----------
  const perGameRows: StatRow[] = players.length
    ? [
        { label: 'MPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.minutes_per_game), cells: players.map((p) => ({ value: fmt(p.season_stats?.minutes_per_game), pctile: p.percentiles?.mpg_pct, color: p.color })) },
        { label: 'PPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.ppg), cells: players.map((p) => ({ value: fmt(p.season_stats?.ppg), pctile: p.percentiles?.ppg_pct, color: p.color })) },
        { label: 'RPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.rpg), cells: players.map((p) => ({ value: fmt(p.season_stats?.rpg), pctile: p.percentiles?.rpg_pct, color: p.color })) },
        { label: 'APG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.apg), cells: players.map((p) => ({ value: fmt(p.season_stats?.apg), pctile: p.percentiles?.apg_pct, color: p.color })) },
        { label: 'SPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.spg), cells: players.map((p) => ({ value: fmt(p.season_stats?.spg), pctile: p.percentiles?.spg_pct, color: p.color })) },
        { label: 'BPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.bpg), cells: players.map((p) => ({ value: fmt(p.season_stats?.bpg), pctile: p.percentiles?.bpg_pct, color: p.color })) },
        { label: 'TOPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.topg), cells: players.map((p) => ({ value: fmt(p.season_stats?.topg), pctile: p.percentiles?.topg_pct, color: p.color })) },
      ]
    : [];

  const shootingRows: StatRow[] = players.length
    ? [
        { label: 'FG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.fg_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.fg_pct), pctile: p.percentiles?.fg_pct_pct, color: p.color })) },
        { label: '3P%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.tp_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.tp_pct), pctile: p.percentiles?.tp_pct_pct, color: p.color })) },
        { label: 'FT%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.ft_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.ft_pct), pctile: p.percentiles?.ft_pct_pct, color: p.color })) },
        { label: 'eFG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.effective_fg_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.effective_fg_pct), pctile: p.percentiles?.effective_fg_pct_pct, color: p.color })) },
        { label: 'TS%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.true_shooting_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.true_shooting_pct), pctile: p.percentiles?.true_shooting_pct_pct, color: p.color })) },
        { label: 'USG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.usage_rate), cells: players.map((p) => ({ value: pct(p.season_stats?.usage_rate), pctile: p.percentiles?.usage_rate_pct, color: p.color })) },
      ]
    : [];

  const rateRows: StatRow[] = players.length
    ? [
        { label: 'AST%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.ast_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.ast_pct), pctile: p.percentiles?.ast_pct_pct, color: p.color })) },
        { label: 'TOV%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.tov_pct), cells: players.map((p) => ({ value: pct(p.season_stats?.tov_pct), pctile: p.percentiles?.tov_pct_pct, color: p.color })) },
        { label: 'OR%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.orb_pct), cells: players.map((p) => ({ value: pctVal(p.season_stats?.orb_pct), pctile: p.percentiles?.orb_pct_pct, color: p.color })) },
        { label: 'DR%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.drb_pct), cells: players.map((p) => ({ value: pctVal(p.season_stats?.drb_pct), pctile: p.percentiles?.drb_pct_pct, color: p.color })) },
        { label: 'STL%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.stl_pct), cells: players.map((p) => ({ value: pctVal(p.season_stats?.stl_pct), pctile: p.percentiles?.stl_pct_pct, color: p.color })) },
        { label: 'BLK%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.blk_pct), cells: players.map((p) => ({ value: pctVal(p.season_stats?.blk_pct), pctile: p.percentiles?.blk_pct_pct, color: p.color })) },
        { label: 'FT Rate', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.ft_rate), cells: players.map((p) => ({ value: fmt(p.season_stats?.ft_rate, 2), pctile: p.percentiles?.ft_rate_pct, color: p.color })) },
      ]
    : [];

  const hasTorvik = players.some((p) => p.torvik_stats);
  // Unlike the three tables above, this one has a fallback branch, so it has
  // to check for an empty roster itself: cross-year can render columns with no
  // resolved player behind any of them, and rows carrying zero cells would
  // otherwise draw an Advanced Metrics table with labels and nothing in it.
  const advancedRows: StatRow[] = players.length === 0
    ? []
    : hasTorvik
    ? [
        { label: 'CAM', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.campom), cells: players.map((p) => ({ value: fmt(p.torvik_stats?.campom), pctile: p.torvik_stats?.campom_pct, color: p.color })) },
        // O/D halves of CAM (envelope-gated server-side). The compute
        // pipeline doesn't materialize a PERCENT_RANK for the halves, so the
        // bar is driven by a MODELED percentile (`camHalfPctile`, fit to the
        // documented O/D spread) — left-fill + advantage chips, consistent with
        // the other rows.
        { label: 'CAMO', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.campom_o), cells: players.map((p) => ({ value: fmt(p.torvik_stats?.campom_o), pctile: camHalfPctile(p.torvik_stats?.campom_o, 'o'), color: p.color, title: 'Offensive half of CAM (per 100). Bar = modeled D-I percentile, estimated from the O/D spread.' })) },
        { label: 'CAMD', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.campom_d), cells: players.map((p) => ({ value: fmt(p.torvik_stats?.campom_d), pctile: camHalfPctile(p.torvik_stats?.campom_d, 'd'), color: p.color, title: 'Defensive half of CAM (per 100, positive = value added). Bar = modeled D-I percentile, estimated from the O/D spread.' })) },
        { label: 'Adj ORTG', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.adj_oe ?? p.season_stats?.offensive_rating), cells: players.map((p) => ({ value: fmt(p.torvik_stats?.adj_oe ?? p.season_stats?.offensive_rating), pctile: p.torvik_stats?.adj_oe_pct ?? p.percentiles?.offensive_rating_pct, color: p.color })) },
        { label: 'Adj DRTG', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.adj_de ?? p.season_stats?.defensive_rating), cells: players.map((p) => ({ value: fmt(p.torvik_stats?.adj_de ?? p.season_stats?.defensive_rating), pctile: p.torvik_stats?.adj_de_pct ?? p.percentiles?.defensive_rating_pct, color: p.color })) },
        { label: 'SOS', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.player_sos), cells: players.map((p) => ({ value: fmt(p.season_stats?.player_sos, 2), pctile: p.percentiles?.player_sos_pct, color: p.color })) },
      ]
    : [
        { label: 'ORTG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.offensive_rating), cells: players.map((p) => ({ value: fmt(p.season_stats?.offensive_rating), pctile: p.percentiles?.offensive_rating_pct, color: p.color })) },
        { label: 'DRTG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.defensive_rating), cells: players.map((p) => ({ value: fmt(p.season_stats?.defensive_rating), pctile: p.percentiles?.defensive_rating_pct, color: p.color })) },
        { label: 'SOS', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.player_sos), cells: players.map((p) => ({ value: fmt(p.season_stats?.player_sos, 2), pctile: p.percentiles?.player_sos_pct, color: p.color })) },
      ];

  // ---------- radar overlay ----------
  // Resolve each player once; the radar reads `.value`, the tooltip reads
  // the matched axis entry for the active spoke.
  const resolvedPerPlayer = useMemo(
    () =>
      players.map((p) =>
        resolveAxes({
          season_stats: p.season_stats,
          percentiles: p.percentiles,
          torvik_stats: p.torvik_stats,
        }),
      ),
    [players],
  );
  const radarData = useMemo(() => {
    if (resolvedPerPlayer.length === 0) return [];
    const axisCount = resolvedPerPlayer[0]?.length ?? 0;
    return Array.from({ length: axisCount }, (_, axisIdx) => {
      const row: Record<string, number | string> = {
        stat: resolvedPerPlayer[0][axisIdx].stat,
      };
      resolvedPerPlayer.forEach((axes, i) => {
        row[`p${i}`] = axes[axisIdx]?.value ?? 0;
      });
      return row;
    });
  }, [resolvedPerPlayer]);

  // ---------- rolling form overlay ----------
  const rollingData = useMemo(() => {
    if (players.length === 0) return [];
    const maxGames = Math.max(...players.map((p) => p.game_log.length));
    if (maxGames === 0) return [];
    const rows: Record<string, number | null>[] = [];
    for (let idx = 0; idx < maxGames; idx++) {
      const row: Record<string, number | null> = { game: idx + 1 };
      players.forEach((p, i) => {
        const g = p.game_log[idx];
        row[`p${i}`] = g?.rolling_game_score ?? null;
      });
      rows.push(row);
    }
    return rows;
  }, [players]);

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold">Player Comparison</h1>
          <p className="text-sm text-gray-500 mt-1">
            Compare up to {maxPlayers} players side by side. Bars show D-I percentile.
          </p>
          {crossYear && (
            <p className="text-xs text-gray-500 mt-2 max-w-2xl">
              Each player carries their own year. Percentiles lead here: shooting
              is up and turnovers are down across the last decade, so two raw
              numbers a decade apart are measured against different baselines,
              while a percentile compares each player to the ones they actually
              played against. Same-year comparisons are the sturdiest; the
              further apart the years, the more of the gap is the era.
            </p>
          )}
        </div>
        <ModeToggle
          options={MODE_OPTIONS}
          value={mode}
          onChange={changeMode}
          ariaLabel="Comparison mode"
          className="self-start shrink-0"
        />
      </div>

      <div className="bg-gray-800 rounded-lg p-4 space-y-3">
        <div className="flex items-start gap-2">
          <div className="flex-1 min-w-0">
            <PlayerPicker
              onAdd={addPlayer}
              disabled={ids.length >= maxPlayers}
              max={maxPlayers}
              existingIds={pickerExistingIds}
              season={crossYear ? searchSeason : season}
            />
          </div>
          {crossYear && (
            <label className="flex shrink-0 items-center gap-1.5 py-2 text-xs text-gray-400">
              <span className="hidden sm:inline">Search</span>
              <select
                value={searchSeason}
                onChange={(e) => setSearchSeason(Number(e.target.value))}
                aria-label="Season to search in"
                className="bg-gray-800 border border-gray-600 rounded px-1.5 py-1 text-xs text-gray-200 focus:outline-none focus:border-blue-500"
              >
                {allSeasons.map((y) => (
                  <option key={y} value={y}>
                    {y}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
        {visibleSlots.length > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            {visibleSlots.map((p) => (
              <span
                key={p.slotId + p.slotIndex}
                className="inline-flex items-center gap-2 px-2 py-1 rounded text-sm bg-gray-900 border"
                style={{ borderColor: p.color }}
              >
                <span
                  className="inline-block w-2 h-2 rounded-full"
                  style={{ background: p.color }}
                />
                <span className={p.available ? '' : 'text-gray-400'}>
                  {p.available ? p.player.name : p.requested_name ?? 'Unknown player'}
                </span>
                {crossYear && (
                  <SlotSeasonSelect
                    season={p.season}
                    seasons={p.available_seasons}
                    color={p.color}
                    label={p.available ? p.player.name : p.requested_name ?? 'this player'}
                    onChange={(next) => changeSlotSeason(p.slotIndex, next)}
                  />
                )}
                <button
                  onClick={() => removeSlot(p.slotIndex)}
                  className="text-gray-500 hover:text-red-400"
                  aria-label="Remove"
                >
                  ×
                </button>
              </span>
            ))}
            {players.length >= 2 && (
              <label className="ml-auto inline-flex items-center gap-2 text-xs text-gray-400 cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={showChips}
                  onChange={(e) => setShowChips(e.target.checked)}
                  className="accent-blue-500"
                />
                Advantage chips
              </label>
            )}
          </div>
        )}
      </div>

      {error && <div className="text-red-400 text-sm">{error}</div>}
      {loading && <div className="text-gray-400 text-sm">Loading…</div>}

      {/* Single-season mode drops a slot with no row in the chosen year, which
          is the behaviour it has always had — but doing it in silence is how
          you end up looking at three columns when you picked four. Name them.
          Cross-year mode renders those slots, so it needs no note. */}
      {!loading && !crossYear && droppedSlots.length > 0 && (
        <div className="text-xs text-gray-400 bg-gray-800/50 border border-gray-700 rounded-lg px-4 py-3">
          Not shown:{' '}
          <span className="text-gray-300">
            {droppedSlots.map((s) => s.requested_name ?? 'one player').join(', ')}
          </span>{' '}
          — no {season} season. Switch to <span className="text-gray-300">Any year</span> to
          give each player their own.
        </div>
      )}

      {!loading && visibleSlots.length === 0 && (
        <div className="bg-gray-800/50 border border-dashed border-gray-700 rounded-lg p-8 text-center text-gray-500 text-sm">
          Search for players above to begin comparing.
        </div>
      )}

      {visibleSlots.length > 0 && (
        <>
          {/* Headers cover every slot, including the ones with no season row;
              the tables below cover only the resolved ones. */}
          <div
            className="grid gap-3"
            style={{ gridTemplateColumns: `repeat(${visibleSlots.length}, minmax(0, 1fr))` }}
          >
            {visibleSlots.map((p) =>
              p.available ? (
                <PlayerHeader
                  key={p.slotId + p.slotIndex}
                  p={p}
                  color={p.color}
                  onRemove={() => removeSlot(p.slotIndex)}
                  showSeason={crossYear}
                />
              ) : (
                <UnavailableHeader
                  key={p.slotId + p.slotIndex}
                  p={p}
                  color={p.color}
                  onRemove={() => removeSlot(p.slotIndex)}
                />
              ),
            )}
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <StatTable title="Per-Game" rows={perGameRows} showChips={showChips} leadWithPctile={spansSeasons} />
            <StatTable title="Shooting & Usage" rows={shootingRows} showChips={showChips} leadWithPctile={spansSeasons} />
            <StatTable title="Rate Stats" rows={rateRows} showChips={showChips} leadWithPctile={spansSeasons} />
            <StatTable title="Advanced Metrics" rows={advancedRows} showChips={showChips} leadWithPctile={spansSeasons} />
          </div>

          {radarData.length > 0 && (
            <div
              ref={radarRef as React.RefObject<HTMLDivElement>}
              className="bg-gray-800 rounded-lg p-5 relative"
            >
              <h2 className="text-lg font-bold mb-3">Percentile Profile</h2>
              <ResponsiveContainer width="100%" height={isMobile ? 280 : 360}>
                <RadarChart data={radarData}>
                  <PolarGrid stroke="#475569" />
                  <PolarAngleAxis
                    dataKey="stat"
                    tick={(props) => (
                      <RadarTick
                        {...props}
                        selected={selectedAxis === props.payload?.value}
                        onSelect={(s) =>
                          setSelectedAxis((prev) => (prev === s ? null : s))
                        }
                      />
                    )}
                  />
                  <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
                  {players.map((p, i) => (
                    <Radar
                      key={p.slotId + p.slotIndex}
                      name={slotLabel(p)}
                      dataKey={`p${i}`}
                      stroke={p.color}
                      fill={p.color}
                      fillOpacity={0.2}
                    />
                  ))}
                  <Legend wrapperStyle={{ fontSize: 12 }} />
                </RadarChart>
              </ResponsiveContainer>
              {selectedAxis &&
                (() => {
                  const axisIdx = resolvedPerPlayer[0]?.findIndex(
                    (a) => a.stat === selectedAxis,
                  );
                  if (axisIdx == null || axisIdx < 0) return null;
                  const resolutions = resolvedPerPlayer.map((axes, i) => ({
                    ...axes[axisIdx],
                    playerLabel: slotLabel(players[i]),
                    playerColor: players[i].color,
                  }));
                  return (
                    <RadarAxisTooltip
                      resolutions={resolutions}
                      onClose={() => setSelectedAxis(null)}
                    />
                  );
                })()}
            </div>
          )}

          {players.some((p) => p.torvik_stats) && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-3">Shot Diet</h2>
              <div
                className="grid gap-3"
                style={{ gridTemplateColumns: `repeat(${players.length}, minmax(0, 1fr))` }}
              >
                {players.map((p) => (
                  <div key={p.slotId + p.slotIndex} className="flex flex-col items-center">
                    <div
                      className="text-xs font-medium mb-2 truncate w-full text-center"
                      style={{ color: p.color }}
                    >
                      {slotLabel(p)}
                    </div>
                    {p.torvik_stats ? (
                      <>
                        <ShotDietCourt torvik={p.torvik_stats} />
                        <div className="w-full mt-3">
                          <ShotDistributionBar torvik={p.torvik_stats} />
                        </div>
                      </>
                    ) : (
                      <div className="text-xs text-gray-500 py-12">No shot data</div>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}

          {rollingData.length > 0 && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-1">Rolling Game Score (5-game avg)</h2>
              <p className="text-xs text-gray-500 mb-3">X-axis is game number into the season.</p>
              <ResponsiveContainer width="100%" height={isMobile ? 220 : 280}>
                <LineChart data={rollingData}>
                  <CartesianGrid stroke="#334155" />
                  <XAxis
                    dataKey="game"
                    tick={{ fill: '#94a3b8', fontSize: 11 }}
                    label={{ value: 'Game #', position: 'insideBottom', offset: -2, fill: '#64748b', fontSize: 11 }}
                  />
                  <YAxis tick={{ fill: '#94a3b8', fontSize: 11 }} />
                  <Tooltip
                    contentStyle={{ background: '#1e293b', border: '1px solid #475569', borderRadius: '0.5rem' }}
                  />
                  <Legend wrapperStyle={{ fontSize: 12 }} />
                  {players.map((p, i) => (
                    <Line
                      key={p.slotId + p.slotIndex}
                      type="monotone"
                      dataKey={`p${i}`}
                      name={slotLabel(p)}
                      stroke={p.color}
                      dot={false}
                      strokeWidth={2}
                      connectNulls
                    />
                  ))}
                </LineChart>
              </ResponsiveContainer>
            </div>
          )}
        </>
      )}
    </div>
  );
}
