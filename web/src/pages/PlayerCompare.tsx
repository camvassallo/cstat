import { useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams, Link } from 'react-router-dom';
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
  fetchPlayers,
  fetchPlayerCompare,
  type ComparePlayer,
  type PlayerRow,
} from '../api/client';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';

const PLAYER_COLORS = ['#3b82f6', '#f97316', '#22c55e', '#a855f7'];
const MAX_PLAYERS = 4;

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
      className={`inline-flex items-center gap-1 text-[10px] font-bold px-1.5 py-0.5 rounded leading-none ${cfg.classes}`}
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
}

function StatCell({ value, pctile, color, chip }: StatCellProps) {
  const p = pctile != null ? Math.max(0, Math.min(1, pctile)) : null;
  return (
    <div>
      <div className="flex items-center justify-end gap-1.5">
        {chip && <Chip {...chip} />}
        <span className="font-medium text-sm">{value}</span>
      </div>
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
}: {
  title: string;
  rows: StatRow[];
  showChips: boolean;
}) {
  if (rows.length === 0) return null;
  const cols = rows[0].cells.length;
  return (
    <div className="bg-gray-800 rounded-lg p-5">
      <h2 className="text-lg font-bold mb-3">{title}</h2>
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
                    <StatCell {...cell} chip={chips[j]} />
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

function PlayerHeader({ p, color, onRemove }: { p: ComparePlayer; color: string; onRemove: () => void }) {
  const { player } = p;
  return (
    <div
      className="bg-gray-800 rounded-lg p-4 flex items-start justify-between gap-3 border-l-4"
      style={{ borderLeftColor: color }}
    >
      <div className="min-w-0">
        <Link
          to={`/players/${player.id}`}
          className="text-base font-bold hover:underline block truncate"
        >
          {player.name}
        </Link>
        <div className="text-xs text-gray-400 truncate">
          {player.team_id ? (
            <Link to={`/teams/${player.team_id}`} className="hover:underline">
              {player.team_name}
            </Link>
          ) : (
            player.team_name ?? 'Unknown'
          )}
          {player.conference && <span className="text-gray-500"> · {player.conference}</span>}
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

function PlayerPicker({
  onAdd,
  disabled,
  existingIds,
}: {
  onAdd: (id: string) => void;
  disabled: boolean;
  existingIds: string[];
}) {
  const [search, setSearch] = useState('');
  const [results, setResults] = useState<PlayerRow[]>([]);
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const reqRef = useRef(0);

  useEffect(() => {
    const term = search.trim();
    const reqId = ++reqRef.current;
    if (term.length < 2) return;
    const handle = setTimeout(() => {
      setLoading(true);
      fetchPlayers({ search: term, limit: 12 })
        .then((r) => {
          if (reqRef.current === reqId) setResults(r.players);
        })
        .finally(() => {
          if (reqRef.current === reqId) setLoading(false);
        });
    }, 200);
    return () => clearTimeout(handle);
  }, [search]);

  const filtered =
    search.trim().length >= 2 ? results.filter((r) => !existingIds.includes(r.player_id)) : [];

  return (
    <div className="relative">
      <input
        type="text"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        placeholder={disabled ? `Up to ${MAX_PLAYERS} players` : 'Add player by name…'}
        disabled={disabled}
        className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:border-blue-500 disabled:opacity-50"
      />
      {open && search.trim().length >= 2 && (
        <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-700 rounded shadow-lg max-h-72 overflow-y-auto">
          {loading && <div className="px-3 py-2 text-xs text-gray-500">Searching…</div>}
          {!loading && filtered.length === 0 && (
            <div className="px-3 py-2 text-xs text-gray-500">No players found</div>
          )}
          {filtered.map((p) => (
            <button
              key={p.player_id}
              type="button"
              onMouseDown={(e) => {
                e.preventDefault();
                onAdd(p.player_id);
                setSearch('');
                setResults([]);
              }}
              className="w-full text-left px-3 py-2 hover:bg-gray-800 text-sm flex items-center justify-between gap-3"
            >
              <span className="truncate">{p.name}</span>
              <span className="text-xs text-gray-500 truncate">
                {p.team_name ?? '—'} · {fmt(p.ppg)} PPG
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function PlayerCompare() {
  const [searchParams, setSearchParams] = useSearchParams();
  const idsCsv = searchParams.get('ids') ?? '';
  const ids = useMemo(
    () => (idsCsv ? idsCsv.split(',').map((s) => s.trim()).filter(Boolean) : []),
    [idsCsv],
  );

  const [players, setPlayers] = useState<ComparePlayer[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showChips, setShowChips] = useState(true);

  useEffect(() => {
    if (ids.length === 0) {
      setPlayers([]);
      return;
    }
    setLoading(true);
    setError(null);
    fetchPlayerCompare(ids)
      .then((r) => {
        // Preserve URL order in case the API returns differently
        const byId = new Map(r.players.map((p) => [p.player.id, p]));
        setPlayers(ids.map((id) => byId.get(id)).filter((p): p is ComparePlayer => !!p));
      })
      .catch((e) => setError(e.message ?? 'Failed to load comparison'))
      .finally(() => setLoading(false));
  }, [ids]);

  const updateIds = (next: string[]) => {
    if (next.length === 0) setSearchParams({});
    else setSearchParams({ ids: next.join(',') });
  };

  const addPlayer = (id: string) => {
    if (ids.includes(id) || ids.length >= MAX_PLAYERS) return;
    updateIds([...ids, id]);
  };
  const removePlayer = (id: string) => updateIds(ids.filter((x) => x !== id));

  // ---------- table rows ----------
  const perGameRows: StatRow[] = players.length
    ? [
        { label: 'MPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.minutes_per_game), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.minutes_per_game), pctile: p.percentiles?.mpg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'PPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.ppg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.ppg), pctile: p.percentiles?.ppg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'RPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.rpg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.rpg), pctile: p.percentiles?.rpg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'APG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.apg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.apg), pctile: p.percentiles?.apg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'SPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.spg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.spg), pctile: p.percentiles?.spg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'BPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.bpg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.bpg), pctile: p.percentiles?.bpg_pct, color: PLAYER_COLORS[i] })) },
        { label: 'TOPG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.topg), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.topg), pctile: p.percentiles?.topg_pct, color: PLAYER_COLORS[i] })) },
      ]
    : [];

  const shootingRows: StatRow[] = players.length
    ? [
        { label: 'FG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.fg_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.fg_pct), pctile: p.percentiles?.fg_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: '3P%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.tp_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.tp_pct), pctile: p.percentiles?.tp_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'FT%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.ft_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.ft_pct), pctile: p.percentiles?.ft_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'eFG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.effective_fg_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.effective_fg_pct), pctile: p.percentiles?.effective_fg_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'TS%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.true_shooting_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.true_shooting_pct), pctile: p.percentiles?.true_shooting_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'USG%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.usage_rate), cells: players.map((p, i) => ({ value: pct(p.season_stats?.usage_rate), pctile: p.percentiles?.usage_rate_pct, color: PLAYER_COLORS[i] })) },
      ]
    : [];

  const rateRows: StatRow[] = players.length
    ? [
        { label: 'AST%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.ast_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.ast_pct), pctile: p.percentiles?.ast_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'TOV%', deltaFmt: dFmtPpFrac, raws: players.map((p) => p.season_stats?.tov_pct), cells: players.map((p, i) => ({ value: pct(p.season_stats?.tov_pct), pctile: p.percentiles?.tov_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'OR%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.orb_pct), cells: players.map((p, i) => ({ value: pctVal(p.season_stats?.orb_pct), pctile: p.percentiles?.orb_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'DR%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.drb_pct), cells: players.map((p, i) => ({ value: pctVal(p.season_stats?.drb_pct), pctile: p.percentiles?.drb_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'STL%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.stl_pct), cells: players.map((p, i) => ({ value: pctVal(p.season_stats?.stl_pct), pctile: p.percentiles?.stl_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'BLK%', deltaFmt: dFmtPpDirect, raws: players.map((p) => p.season_stats?.blk_pct), cells: players.map((p, i) => ({ value: pctVal(p.season_stats?.blk_pct), pctile: p.percentiles?.blk_pct_pct, color: PLAYER_COLORS[i] })) },
        { label: 'FT Rate', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.ft_rate), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.ft_rate, 2), pctile: p.percentiles?.ft_rate_pct, color: PLAYER_COLORS[i] })) },
      ]
    : [];

  const hasTorvik = players.some((p) => p.torvik_stats);
  const advancedRows: StatRow[] = hasTorvik
    ? [
        { label: 'GBPM', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.gbpm), cells: players.map((p, i) => ({ value: fmt(p.torvik_stats?.gbpm), pctile: p.torvik_stats?.gbpm_pct, color: PLAYER_COLORS[i] })) },
        { label: 'OGBPM', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.ogbpm), cells: players.map((p, i) => ({ value: fmt(p.torvik_stats?.ogbpm), pctile: p.torvik_stats?.ogbpm_pct, color: PLAYER_COLORS[i] })) },
        { label: 'DGBPM', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.dgbpm), cells: players.map((p, i) => ({ value: fmt(p.torvik_stats?.dgbpm), pctile: p.torvik_stats?.dgbpm_pct, color: PLAYER_COLORS[i] })) },
        { label: 'Adj ORTG', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.adj_oe ?? p.season_stats?.offensive_rating), cells: players.map((p, i) => ({ value: fmt(p.torvik_stats?.adj_oe ?? p.season_stats?.offensive_rating), pctile: p.torvik_stats?.adj_oe_pct ?? p.percentiles?.offensive_rating_pct, color: PLAYER_COLORS[i] })) },
        { label: 'Adj DRTG', deltaFmt: dFmt1, raws: players.map((p) => p.torvik_stats?.adj_de ?? p.season_stats?.defensive_rating), cells: players.map((p, i) => ({ value: fmt(p.torvik_stats?.adj_de ?? p.season_stats?.defensive_rating), pctile: p.torvik_stats?.adj_de_pct ?? p.percentiles?.defensive_rating_pct, color: PLAYER_COLORS[i] })) },
        { label: 'SOS', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.player_sos), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.player_sos, 2), pctile: p.percentiles?.player_sos_pct, color: PLAYER_COLORS[i] })) },
      ]
    : [
        { label: 'ORTG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.offensive_rating), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.offensive_rating), pctile: p.percentiles?.offensive_rating_pct, color: PLAYER_COLORS[i] })) },
        { label: 'DRTG', deltaFmt: dFmt1, raws: players.map((p) => p.season_stats?.defensive_rating), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.defensive_rating), pctile: p.percentiles?.defensive_rating_pct, color: PLAYER_COLORS[i] })) },
        { label: 'SOS', deltaFmt: dFmt2, raws: players.map((p) => p.season_stats?.player_sos), cells: players.map((p, i) => ({ value: fmt(p.season_stats?.player_sos, 2), pctile: p.percentiles?.player_sos_pct, color: PLAYER_COLORS[i] })) },
      ];

  // ---------- radar overlay ----------
  const radarData = useMemo(() => {
    if (players.length === 0) return [];
    const axes = [
      { stat: 'Scoring', get: (p: ComparePlayer) => p.percentiles?.ppg_pct ?? 0 },
      { stat: 'Efficiency', get: (p: ComparePlayer) => p.percentiles?.true_shooting_pct_pct ?? 0 },
      { stat: '3PT', get: (p: ComparePlayer) => p.percentiles?.tp_pct_pct ?? 0 },
      { stat: 'Playmaking', get: (p: ComparePlayer) => p.percentiles?.ast_pct_pct ?? p.percentiles?.apg_pct ?? 0 },
      { stat: 'Usage', get: (p: ComparePlayer) => p.percentiles?.usage_rate_pct ?? 0 },
      { stat: 'Steals', get: (p: ComparePlayer) => p.percentiles?.stl_pct_pct ?? p.torvik_stats?.stl_pct_pct ?? p.percentiles?.spg_pct ?? 0 },
      { stat: 'Blocks', get: (p: ComparePlayer) => p.percentiles?.blk_pct_pct ?? p.torvik_stats?.blk_pct_pct ?? p.percentiles?.bpg_pct ?? 0 },
      { stat: 'Rebounding', get: (p: ComparePlayer) => p.percentiles?.drb_pct_pct ?? p.torvik_stats?.drb_pct_pct ?? p.percentiles?.rpg_pct ?? 0 },
      { stat: 'Def Rating', get: (p: ComparePlayer) => p.torvik_stats?.adj_de_pct ?? p.percentiles?.defensive_rating_pct ?? 0 },
    ];
    return axes.map((axis) => {
      const row: Record<string, number | string> = { stat: axis.stat };
      players.forEach((p, i) => {
        row[`p${i}`] = (axis.get(p) ?? 0) * 100;
      });
      return row;
    });
  }, [players]);

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
      <div>
        <h1 className="text-2xl font-bold">Player Comparison</h1>
        <p className="text-sm text-gray-500 mt-1">
          Compare up to {MAX_PLAYERS} players side by side. Bars show D-I percentile.
        </p>
      </div>

      <div className="bg-gray-800 rounded-lg p-4 space-y-3">
        <PlayerPicker
          onAdd={addPlayer}
          disabled={ids.length >= MAX_PLAYERS}
          existingIds={ids}
        />
        {players.length > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            {players.map((p, i) => (
              <span
                key={p.player.id}
                className="inline-flex items-center gap-2 px-2 py-1 rounded text-sm bg-gray-900 border"
                style={{ borderColor: PLAYER_COLORS[i] }}
              >
                <span
                  className="inline-block w-2 h-2 rounded-full"
                  style={{ background: PLAYER_COLORS[i] }}
                />
                {p.player.name}
                <button
                  onClick={() => removePlayer(p.player.id)}
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

      {!loading && players.length === 0 && (
        <div className="bg-gray-800/50 border border-dashed border-gray-700 rounded-lg p-8 text-center text-gray-500 text-sm">
          Search for players above to begin comparing.
        </div>
      )}

      {players.length > 0 && (
        <>
          <div
            className="grid gap-3"
            style={{ gridTemplateColumns: `repeat(${players.length}, minmax(0, 1fr))` }}
          >
            {players.map((p, i) => (
              <PlayerHeader
                key={p.player.id}
                p={p}
                color={PLAYER_COLORS[i]}
                onRemove={() => removePlayer(p.player.id)}
              />
            ))}
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <StatTable title="Per-Game" rows={perGameRows} showChips={showChips} />
            <StatTable title="Shooting & Usage" rows={shootingRows} showChips={showChips} />
            <StatTable title="Rate Stats" rows={rateRows} showChips={showChips} />
            <StatTable title="Advanced Metrics" rows={advancedRows} showChips={showChips} />
          </div>

          {radarData.length > 0 && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-3">Percentile Profile</h2>
              <ResponsiveContainer width="100%" height={360}>
                <RadarChart data={radarData}>
                  <PolarGrid stroke="#475569" />
                  <PolarAngleAxis dataKey="stat" tick={{ fill: '#94a3b8', fontSize: 12 }} />
                  <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
                  {players.map((p, i) => (
                    <Radar
                      key={p.player.id}
                      name={p.player.name}
                      dataKey={`p${i}`}
                      stroke={PLAYER_COLORS[i]}
                      fill={PLAYER_COLORS[i]}
                      fillOpacity={0.2}
                    />
                  ))}
                  <Legend wrapperStyle={{ fontSize: 12 }} />
                </RadarChart>
              </ResponsiveContainer>
            </div>
          )}

          {players.some((p) => p.torvik_stats) && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-3">Shot Diet</h2>
              <div
                className="grid gap-3"
                style={{ gridTemplateColumns: `repeat(${players.length}, minmax(0, 1fr))` }}
              >
                {players.map((p, i) => (
                  <div key={p.player.id} className="flex flex-col items-center">
                    <div
                      className="text-xs font-medium mb-2 truncate w-full text-center"
                      style={{ color: PLAYER_COLORS[i] }}
                    >
                      {p.player.name}
                    </div>
                    {p.torvik_stats ? (
                      <>
                        <ShotDietCourt torvik={p.torvik_stats} />
                        <div className="w-full mt-3">
                          <ShotDistributionBar torvik={p.torvik_stats} />
                        </div>
                      </>
                    ) : (
                      <div className="text-xs text-gray-500 py-12">No Torvik data</div>
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
              <ResponsiveContainer width="100%" height={280}>
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
                      key={p.player.id}
                      type="monotone"
                      dataKey={`p${i}`}
                      name={p.player.name}
                      stroke={PLAYER_COLORS[i]}
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
