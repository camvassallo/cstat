import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import { AllCommunityModule, ModuleRegistry, type ColDef } from 'ag-grid-community';
import { fetchTeamRankings, type TeamRanking } from '../api/client';
import { gridTheme } from '../theme';
import { TableToolbar, TableSearchInput } from '../components/TableToolbar';
import { pctileTextColor } from '../components/pctile';

// AdjEM presentation tiers — same chip styling pattern as CamPom on the
// Players tab. Thresholds use the conventional KenPom absolute scale where
// +20 is roughly Final Four caliber and 0 is the league-average D-I team.
type AdjEmTier =
  | 'Elite'
  | 'Strong'
  | 'Above average'
  | 'Average'
  | 'Below average'
  | 'Weak';

function adjEmTier(em: number | null | undefined): AdjEmTier | null {
  if (em == null) return null;
  if (em >= 25) return 'Elite';
  if (em >= 15) return 'Strong';
  if (em >= 5) return 'Above average';
  if (em >= -5) return 'Average';
  if (em >= -15) return 'Below average';
  return 'Weak';
}

function adjEmTierColor(tier: AdjEmTier | null): string {
  switch (tier) {
    case 'Elite':         return 'bg-emerald-500/20 text-emerald-300 border-emerald-500/40';
    case 'Strong':        return 'bg-sky-500/20 text-sky-300 border-sky-500/40';
    case 'Above average': return 'bg-blue-500/20 text-blue-300 border-blue-500/40';
    case 'Average':       return 'bg-slate-500/20 text-slate-300 border-slate-500/40';
    case 'Below average': return 'bg-amber-500/20 text-amber-300 border-amber-500/40';
    case 'Weak':          return 'bg-rose-500/20 text-rose-300 border-rose-500/40';
    default:              return 'bg-slate-700/40 text-slate-400 border-slate-600/40';
  }
}

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');

/** Cell renderer that shows a formatted value with a subtle rank underneath.
 *  The rank text is tinted by percentile when `totalTeams` is provided —
 *  small enough to stay subtle, but gives an at-a-glance cue alongside the
 *  precise rank number. AdjEM gets its own chip treatment (see
 *  `adjEmTierColor`); this helper is for the supporting columns. */
function RankedCell({
  value,
  rank,
  format,
  totalTeams,
}: {
  value: number | null;
  rank: number | null;
  format: (v: number | null) => string;
  totalTeams?: number;
}) {
  const pctile =
    rank != null && totalTeams && totalTeams > 1
      ? 1 - (rank - 1) / (totalTeams - 1)
      : null;
  const rankColor = pctile != null ? pctileTextColor(pctile) : '#6b7280';
  return (
    <div className="leading-tight py-0.5">
      <div>{format(value)}</div>
      {rank != null && (
        <div className="text-[10px]" style={{ color: rankColor }}>
          #{rank}
        </div>
      )}
    </div>
  );
}

type RankingsView = 'standard' | 'offense' | 'defense';

// Visual divider before the four-factors block — matches the roster table's
// `border-l border-gray-800` category separator. Applied via inline style so
// it survives AG Grid's themed cell borders.
const CATEGORY_DIVIDER_STYLE = { borderLeft: '1px solid rgb(31 41 55)' } as const;

function buildColumns(totalTeams: number, view: RankingsView): ColDef<TeamRanking>[] {
  // Helper for flex-distributed columns. AG Grid normalizes `flex` weights
  // so we can pass natural width values directly as the weight: a column
  // with flex=200 gets 2.5× the share of one with flex=80, preserving
  // proportional sizing.
  //
  // `minWidth` is set ~20px below natural with a 65px absolute floor — this
  // gives the 14-column Offense/Defense view room to compress without
  // forcing horizontal scroll, while still keeping every header readable.
  // Use the second argument for columns whose header text needs more room
  // (e.g. "OpTOV%").
  const flexCol = (w: number, min?: number) => ({
    flex: w,
    minWidth: min ?? Math.max(65, w - 20),
  });

  const base: ColDef<TeamRanking>[] = [
    // Pinned identity columns stay at fixed width (don't flex with the
    // content area; AG Grid recommends fixed widths for pinned cols).
    { field: 'rank', headerName: 'Rk', width: 60, pinned: 'left' },
    {
      field: 'name',
      headerName: 'Team',
      width: 200,
      pinned: 'left',
      cellRenderer: (p: { value: string }) => (
        <span className="text-blue-400 hover:underline cursor-pointer">{p.value}</span>
      ),
    },
    { field: 'conference', headerName: 'Conf', ...flexCol(120) },
    {
      headerName: 'Record',
      ...flexCol(80),
      valueGetter: (p) => (p.data ? `${p.data.wins}-${p.data.losses}` : ''),
      sortable: false,
    },
    {
      field: 'adj_efficiency_margin',
      headerName: 'AdjEM',
      ...flexCol(85),
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-slate-500">—</span>;
        const tier = adjEmTier(p.value);
        return (
          <span
            className={`px-1.5 rounded border text-xs ${adjEmTierColor(tier)}`}
            title={tier ?? ''}
          >
            {p.value.toFixed(1)}
          </span>
        );
      },
    },
    {
      field: 'adj_offense',
      headerName: 'AdjO',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_offense} rank={p.data.adj_offense_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'adj_defense',
      headerName: 'AdjD',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_defense} rank={p.data.adj_defense_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'adj_tempo',
      headerName: 'Tempo',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_tempo} rank={p.data.adj_tempo_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'sos',
      headerName: 'SOS',
      ...flexCol(75),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.sos} rank={p.data.sos_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
    {
      field: 'elo_rating',
      headerName: 'ELO',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.elo_rating} rank={p.data.elo_rank} format={(v) => fmt(v, 0)} totalTeams={totalTeams} />,
    },
  ];

  // Offensive four factors — what this team's offense does. First column
  // gets the category divider so it visually breaks from the efficiency
  // block; same pattern the roster table uses.
  const offense: ColDef<TeamRanking>[] = [
    {
      field: 'effective_fg_pct',
      headerName: 'eFG%',
      ...flexCol(90),
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.effective_fg_pct} rank={p.data.effective_fg_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'turnover_pct',
      headerName: 'TOV%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.turnover_pct} rank={p.data.turnover_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'off_rebound_pct',
      headerName: 'ORB%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.off_rebound_pct} rank={p.data.off_rebound_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'ft_rate',
      headerName: 'FTR',
      ...flexCol(85),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.ft_rate} rank={p.data.ft_rate_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
  ];

  // Defensive four factors — what this team's defense forces opponents into.
  const defense: ColDef<TeamRanking>[] = [
    {
      field: 'opp_effective_fg_pct',
      headerName: 'OpFG%',
      ...flexCol(95, 75),
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
      headerTooltip: 'Opponent eFG% — defense holds opponents to lower number = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_effective_fg_pct} rank={p.data.opp_effective_fg_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'opp_turnover_pct',
      headerName: 'OpTOV%',
      ...flexCol(100, 80),
      headerTooltip: 'Opponent TOV% — defense forces turnovers; higher = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_turnover_pct} rank={p.data.opp_turnover_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'def_rebound_pct',
      headerName: 'DRB%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.def_rebound_pct} rank={p.data.def_rebound_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'opp_ft_rate',
      headerName: 'OpFTR',
      ...flexCol(90),
      headerTooltip: 'Opponent FT Rate — defense avoids fouling; lower = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_ft_rate} rank={p.data.opp_ft_rate_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
  ];

  if (view === 'offense') return [...base, ...offense];
  if (view === 'defense') return [...base, ...defense];
  return base;
}

export default function Rankings() {
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState('');
  const [view, setView] = useState<RankingsView>('standard');
  const navigate = useNavigate();
  const columns = useMemo(
    () => buildColumns(teams.length, view),
    [teams.length, view],
  );

  useEffect(() => {
    fetchTeamRankings()
      .then((r) => setTeams(r.teams))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div>
      <TableToolbar
        title="Team Rankings"
        count={teams.length || null}
        countLabel="teams"
        search={
          <TableSearchInput
            value={search}
            onChange={setSearch}
            placeholder="Search team or conference…"
          />
        }
        controls={
          <>
            <span className="text-xs text-gray-500">View</span>
            <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs">
              {(
                [
                  { v: 'standard', label: 'Standard' },
                  { v: 'offense', label: 'Offense' },
                  { v: 'defense', label: 'Defense' },
                ] as const
              ).map(({ v, label }) => (
                <button
                  key={v}
                  onClick={() => setView(v)}
                  className={`px-2.5 py-1 ${
                    view === v
                      ? 'bg-blue-600 text-white'
                      : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          </>
        }
      />
      <div style={{ height: 'calc(100vh - 160px)', width: '100%' }}>
        <AgGridReact<TeamRanking>
          theme={gridTheme}
          rowData={teams}
          columnDefs={columns}
          loading={loading}
          rowHeight={48}
          quickFilterText={search}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
          onRowClicked={(e) => {
            if (e.data) navigate(`/teams/${e.data.team_id}`);
          }}
          getRowId={(p) => p.data.team_id}
        />
      </div>
    </div>
  );
}
