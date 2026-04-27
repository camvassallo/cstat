import { useEffect, useMemo, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import {
  fetchTeamDetail,
  type TeamProfile,
  type ScheduleEntry,
  type RosterEntry,
  type ArchetypeCount,
} from '../api/client';
import { classColor } from '../components/archetypeColors';
import { ClassTooltip } from '../components/Archetype';
import { campomTier, campomTierColor } from '../components/campom';
import { compareValues, type SortDir } from '../components/tableSort';
import { SortHeader, StickyHeader } from '../components/TableHeaders';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');
const rkStr = (v: number | null | undefined) => (v != null ? `#${v}` : undefined);

function StatCard({ label, value, rank }: { label: string; value: string; rank?: string }) {
  return (
    <div className="bg-gray-800 rounded-lg p-4 text-center">
      <div className="text-xs text-gray-400 uppercase tracking-wide mb-1">{label}</div>
      <div className="text-2xl font-bold">{value}</div>
      {rank && <div className="text-xs text-gray-500 mt-1">{rank}</div>}
    </div>
  );
}

function FourFactors({ team, label }: { team: TeamProfile; label: string }) {
  const isOffense = label === 'Offense';
  const items = isOffense
    ? [
        { label: 'eFG%', value: pct(team.effective_fg_pct), rank: rkStr(team.effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.turnover_pct), rank: rkStr(team.turnover_pct_rank) },
        { label: 'ORB%', value: pct(team.off_rebound_pct), rank: rkStr(team.off_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.ft_rate, 2), rank: rkStr(team.ft_rate_rank) },
      ]
    : [
        { label: 'eFG%', value: pct(team.opp_effective_fg_pct), rank: rkStr(team.opp_effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.opp_turnover_pct), rank: rkStr(team.opp_turnover_pct_rank) },
        { label: 'DRB%', value: pct(team.def_rebound_pct), rank: rkStr(team.def_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.opp_ft_rate, 2), rank: rkStr(team.opp_ft_rate_rank) },
      ];

  return (
    <div className="bg-gray-800 rounded-lg p-4">
      <h3 className="text-sm font-semibold text-gray-400 uppercase mb-3">{label} Four Factors</h3>
      <div className="grid grid-cols-4 gap-3 text-center">
        {items.map((item) => (
          <div key={item.label}>
            <div className="text-xs text-gray-500">{item.label}</div>
            <div className="font-semibold">{item.value}</div>
            {item.rank && <div className="text-[10px] text-gray-500">{item.rank}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}

export default function TeamDetail() {
  const { id } = useParams<{ id: string }>();
  const [team, setTeam] = useState<TeamProfile | null>(null);
  const [schedule, setSchedule] = useState<ScheduleEntry[]>([]);
  const [roster, setRoster] = useState<RosterEntry[]>([]);
  const [archetypeDist, setArchetypeDist] = useState<ArchetypeCount[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    fetchTeamDetail(id)
      .then((r) => {
        setTeam(r.team);
        setSchedule(r.schedule);
        setRoster(r.roster);
        setArchetypeDist(r.archetype_distribution);
      })
      .finally(() => setLoading(false));
  }, [id]);

  // Minute-weighted distribution. Each class's "weight" is its share of total
  // team minutes — a 5-min/game deep-bench Druid contributes far less than
  // a 32-min/game Druid starter. Falls back to player count if minutes are
  // missing (shouldn't happen in practice).
  const distWeights = useMemo(() => {
    const items = archetypeDist.map((a) => ({
      primary_class: a.primary_class,
      count: a.count,
      weight: a.total_minutes != null && a.total_minutes > 0
        ? a.total_minutes
        : a.count,
    }));
    const total = items.reduce((s, x) => s + x.weight, 0);
    return { items, total };
  }, [archetypeDist]);

  // Balance score: normalized Shannon entropy of the minute-weighted
  // distribution. 1.0 = maximally diverse rotation, 0 = single-style roster.
  const balance = useMemo(() => {
    const { items, total } = distWeights;
    if (total <= 0 || items.length < 2) return null;
    const entropy = items.reduce((s, a) => {
      const p = a.weight / total;
      return s + (p > 0 ? -p * Math.log(p) : 0);
    }, 0);
    return entropy / Math.log(items.length);
  }, [distWeights]);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!team) return <div className="text-red-400">Team not found</div>;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-3xl font-bold">{team.name}</h1>
        <div className="text-gray-400">
          {team.conference ?? 'Independent'} &middot; {team.wins ?? 0}-{team.losses ?? 0}
        </div>
      </div>

      {/* Stat Cards */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
        <StatCard label="AdjEM" value={fmt(team.adj_efficiency_margin)} rank={rkStr(team.adj_efficiency_margin_rank)} />
        <StatCard label="AdjO" value={fmt(team.adj_offense)} rank={rkStr(team.adj_offense_rank)} />
        <StatCard label="AdjD" value={fmt(team.adj_defense)} rank={rkStr(team.adj_defense_rank)} />
        <StatCard label="Tempo" value={fmt(team.adj_tempo)} rank={rkStr(team.adj_tempo_rank)} />
        <StatCard label="SOS" value={fmt(team.sos, 2)} rank={team.sos_rank ? `#${team.sos_rank}` : undefined} />
        <StatCard label="ELO" value={fmt(team.elo_rating, 0)} rank={team.elo_rank ? `#${team.elo_rank}` : undefined} />
      </div>

      {/* Four Factors */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FourFactors team={team} label="Offense" />
        <FourFactors team={team} label="Defense" />
      </div>

      {/* Archetype Distribution (minute-weighted) */}
      {archetypeDist.length > 0 && distWeights.total > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <div className="flex items-center justify-between mb-1 flex-wrap gap-2">
            <h2 className="text-lg font-bold">Roster Archetypes</h2>
            {balance != null && (
              <span className="text-xs text-gray-400">
                Balance:{' '}
                <span className="font-bold text-gray-200">
                  {(balance * 100).toFixed(0)}%
                </span>
                <span className="text-gray-500 ml-1">
                  ({balance >= 0.85 ? 'very diverse'
                    : balance >= 0.7 ? 'diverse'
                    : balance >= 0.5 ? 'balanced'
                    : balance >= 0.3 ? 'narrow'
                    : 'single-style'})
                </span>
              </span>
            )}
          </div>
          <p className="text-xs text-gray-500 mb-3">
            Weighted by total minutes played, so the rotation drives the breakdown.
          </p>
          <div className="space-y-2">
            <div className="flex h-3 rounded overflow-hidden bg-gray-900">
              {distWeights.items.map((a) => {
                const share = a.weight / distWeights.total;
                return (
                  <div
                    key={a.primary_class}
                    style={{ flexBasis: `${share * 100}%` }}
                  >
                    <ClassTooltip
                      cls={a.primary_class}
                      asBlock
                      extra={`${(share * 100).toFixed(1)}% of minutes · ${a.count} ${a.count === 1 ? 'player' : 'players'}`}
                    >
                      <div
                        className="h-3 w-full"
                        style={{ background: classColor(a.primary_class) }}
                      />
                    </ClassTooltip>
                  </div>
                );
              })}
            </div>
            <div className="flex flex-wrap gap-2 mt-2">
              {distWeights.items.map((a) => {
                const share = a.weight / distWeights.total;
                return (
                  <ClassTooltip
                    key={a.primary_class}
                    cls={a.primary_class}
                    extra={`${(share * 100).toFixed(1)}% of minutes · ${a.count} ${a.count === 1 ? 'player' : 'players'}`}
                  >
                    <span className="inline-flex items-center gap-1.5 text-xs px-2 py-1 rounded bg-gray-900">
                      <span
                        className="inline-block w-2 h-2 rounded-full"
                        style={{ background: classColor(a.primary_class) }}
                      />
                      <span style={{ color: classColor(a.primary_class) }} className="font-semibold">
                        {a.primary_class}
                      </span>
                      <span className="text-gray-400">{(share * 100).toFixed(0)}%</span>
                    </span>
                  </ClassTooltip>
                );
              })}
            </div>
          </div>
        </div>
      )}

      {/* Roster */}
      <RosterTable roster={roster} />

      {/* Schedule */}
      <ScheduleTable schedule={schedule} />
    </div>
  );
}

type RosterSortKey =
  | 'name'
  | 'campom'
  | 'games_played'
  | 'minutes_per_game'
  | 'usage_rate'
  | 'ppg'
  | 'rpg'
  | 'apg'
  | 'spg'
  | 'bpg'
  | 'topg'
  | 'true_shooting_pct'
  | 'ast_pct'
  | 'tov_pct'
  | 'orb_pct'
  | 'drb_pct'
  | 'stl_pct'
  | 'blk_pct';

type RosterView = 'raw' | 'rate';

// Continuous red → neutral → green gradient on percentile (0–1).
// Anchors: red-400 (#f87171) → gray-200 (#e5e7eb, the table's default text) → green-400 (#4ade80).
// Returns an rgb() string suitable for a `style.color` value.
function pctileTextColor(p: number | null | undefined): string {
  if (p == null) return '#6b7280'; // gray-500 (matches the existing "—" muting)
  const red = [248, 113, 113];
  const mid = [229, 231, 235];
  const green = [74, 222, 128];
  const lerp = (a: number[], b: number[], t: number) =>
    a.map((av, i) => Math.round(av + (b[i] - av) * t));
  const c = p <= 0.5 ? lerp(red, mid, p / 0.5) : lerp(mid, green, (p - 0.5) / 0.5);
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}

function ValueWithPctile({ value, pctile }: { value: string; pctile: number | null | undefined }) {
  return <span style={{ color: pctileTextColor(pctile) }}>{value}</span>;
}

function RosterTable({ roster }: { roster: RosterEntry[] }) {
  const [view, setView] = useState<RosterView>('raw');
  const [sort, setSort] = useState<{ key: RosterSortKey; dir: SortDir }>({
    key: 'campom',
    dir: 'desc',
  });
  const onSort = (key: RosterSortKey) => {
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'name' ? 'asc' : 'desc' },
    );
  };

  // If the current sort column isn't visible in the new view, fall back to CamPom desc.
  const onViewChange = (next: RosterView) => {
    setView(next);
    const rawOnly: RosterSortKey[] = ['ppg', 'rpg', 'apg', 'spg', 'bpg', 'topg'];
    const rateOnly: RosterSortKey[] = ['ast_pct', 'tov_pct', 'orb_pct', 'drb_pct', 'stl_pct', 'blk_pct'];
    if (next === 'rate' && rawOnly.includes(sort.key)) setSort({ key: 'campom', dir: 'desc' });
    if (next === 'raw' && rateOnly.includes(sort.key)) setSort({ key: 'campom', dir: 'desc' });
  };

  const sorted = useMemo(() => {
    return [...roster].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir));
  }, [roster, sort]);

  // pss stores rate stats with mixed conventions:
  //   ast_pct / tov_pct: fractions (0–1), need ×100 for display
  //   orb_pct / drb_pct / stl_pct / blk_pct: already percent-points (0–100)
  const fracPct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) : '—');
  const pointPct = (v: number | null | undefined) => (v != null ? v.toFixed(1) : '—');

  return (
    <div>
      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <h2 className="text-xl font-bold">Roster</h2>
        <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs">
          <button
            onClick={() => onViewChange('raw')}
            className={`px-3 py-1 ${view === 'raw' ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'}`}
          >
            Raw
          </button>
          <button
            onClick={() => onViewChange('rate')}
            className={`px-3 py-1 ${view === 'rate' ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'}`}
          >
            Rate
          </button>
        </div>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <SortHeader label="Player" sortKey="name" current={sort} onSort={onSort} />
              <StickyHeader>Class</StickyHeader>
              <SortHeader
                label="CamPom"
                sortKey="campom"
                current={sort}
                onSort={onSort}
                align="right"
                title="Composite player valuation."
                className="border-l border-gray-800"
              />
              <SortHeader label="GP" sortKey="games_played" current={sort} onSort={onSort} align="right" />
              <SortHeader label="MPG" sortKey="minutes_per_game" current={sort} onSort={onSort} align="right" />
              <SortHeader label="USG%" sortKey="usage_rate" current={sort} onSort={onSort} align="right" />
              <SortHeader label="TS%" sortKey="true_shooting_pct" current={sort} onSort={onSort} align="right" />
              {view === 'raw' ? (
                <>
                  <SortHeader
                    label="PPG"
                    sortKey="ppg"
                    current={sort}
                    onSort={onSort}
                    align="right"
                    className="border-l border-gray-800"
                  />
                  <SortHeader label="RPG" sortKey="rpg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="APG" sortKey="apg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="SPG" sortKey="spg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="BPG" sortKey="bpg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="TOPG" sortKey="topg" current={sort} onSort={onSort} align="right" />
                </>
              ) : (
                <>
                  <SortHeader
                    label="AST%"
                    sortKey="ast_pct"
                    current={sort}
                    onSort={onSort}
                    align="right"
                    className="border-l border-gray-800"
                  />
                  <SortHeader label="TOV%" sortKey="tov_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="ORB%" sortKey="orb_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="DRB%" sortKey="drb_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="STL%" sortKey="stl_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="BLK%" sortKey="blk_pct" current={sort} onSort={onSort} align="right" />
                </>
              )}
            </tr>
          </thead>
          <tbody>
            {sorted.map((p) => (
              <tr key={p.player_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                <td className="py-2 px-2">
                  <Link to={`/players/${p.player_id}`} className="text-blue-400 hover:underline">
                    {p.name}
                  </Link>
                </td>
                <td className="py-2 px-2">
                  {p.primary_class ? (
                    <ClassTooltip cls={p.primary_class}>
                      <span
                        className="text-[10px] font-bold uppercase tracking-wide px-1.5 py-0.5 rounded"
                        style={{
                          color: classColor(p.primary_class),
                          background: classColor(p.primary_class) + '22',
                        }}
                      >
                        {p.primary_class}
                      </span>
                    </ClassTooltip>
                  ) : (
                    <span className="text-gray-600 text-xs">—</span>
                  )}
                </td>
                <td className="py-2 px-2 text-right border-l border-gray-800">
                  {p.campom != null ? (
                    <span
                      className={`px-1.5 rounded border text-xs ${campomTierColor(campomTier(p.campom))}`}
                      title={campomTier(p.campom) ?? ''}
                    >
                      {p.campom.toFixed(1)}
                    </span>
                  ) : (
                    <span className="text-gray-600">—</span>
                  )}
                </td>
                <td className="py-2 px-2 text-right">{p.games_played}</td>
                <td className="py-2 px-2 text-right">{fmt(p.minutes_per_game)}</td>
                <td className="py-2 px-2 text-right">
                  <ValueWithPctile value={fracPct(p.usage_rate)} pctile={p.usage_rate_pct} />
                </td>
                <td className="py-2 px-2 text-right">
                  <ValueWithPctile value={fracPct(p.true_shooting_pct)} pctile={p.true_shooting_pct_pct} />
                </td>
                {view === 'raw' ? (
                  <>
                    <td className="py-2 px-2 text-right border-l border-gray-800">
                      <ValueWithPctile value={fmt(p.ppg)} pctile={p.ppg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.rpg)} pctile={p.rpg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.apg)} pctile={p.apg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.spg)} pctile={p.spg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.bpg)} pctile={p.bpg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.topg)} pctile={p.topg_pct} />
                    </td>
                  </>
                ) : (
                  <>
                    <td className="py-2 px-2 text-right border-l border-gray-800">
                      <ValueWithPctile value={fracPct(p.ast_pct)} pctile={p.ast_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fracPct(p.tov_pct)} pctile={p.tov_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.orb_pct)} pctile={p.orb_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.drb_pct)} pctile={p.drb_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.stl_pct)} pctile={p.stl_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.blk_pct)} pctile={p.blk_pct_pct} />
                    </td>
                  </>
                )}
              </tr>
            ))}
            {sorted.length === 0 && (
              <tr>
                <td colSpan={13} className="py-6 text-center text-gray-500 text-sm">
                  No roster data.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ScheduleTable({ schedule }: { schedule: ScheduleEntry[] }) {
  return (
    <div>
      <h2 className="text-xl font-bold mb-3">Schedule</h2>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <StickyHeader>Date</StickyHeader>
              <StickyHeader>Opponent</StickyHeader>
              <StickyHeader align="center">Result</StickyHeader>
              <StickyHeader align="center">Score</StickyHeader>
            </tr>
          </thead>
          <tbody>
            {schedule.map((g) => {
              const won = g.team_score != null && g.opponent_score != null && g.team_score > g.opponent_score;
              const lost = g.team_score != null && g.opponent_score != null && g.team_score < g.opponent_score;
              return (
                <tr key={g.game_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                  <td className="py-2 px-2 text-gray-400">{g.game_date}</td>
                  <td className="py-2 px-2">
                    {g.is_home === false && '@ '}
                    {g.opponent_id ? (
                      <Link to={`/teams/${g.opponent_id}`} className="text-blue-400 hover:underline">
                        {g.opponent_name ?? 'Unknown'}
                      </Link>
                    ) : (
                      g.opponent_name ?? 'Unknown'
                    )}
                    {g.is_neutral && ' (N)'}
                    {g.is_conference && <span className="text-gray-500 ml-1">*</span>}
                  </td>
                  <td className={`py-2 px-2 text-center font-semibold ${won ? 'text-green-400' : lost ? 'text-red-400' : ''}`}>
                    {g.team_score != null ? (won ? 'W' : 'L') : '—'}
                  </td>
                  <td className="py-2 px-2 text-center">
                    {g.team_score != null ? `${g.team_score}-${g.opponent_score}` : '—'}
                  </td>
                </tr>
              );
            })}
            {schedule.length === 0 && (
              <tr>
                <td colSpan={4} className="py-6 text-center text-gray-500 text-sm">
                  No games scheduled.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
