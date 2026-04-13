import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import { AllCommunityModule, ModuleRegistry, type ColDef } from 'ag-grid-community';
import { fetchTeamRankings, type TeamRanking } from '../api/client';
import { gridTheme } from '../theme';

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');

/** Cell renderer that shows a formatted value with a subtle rank underneath. */
function RankedCell({ value, rank, format }: { value: number | null; rank: number | null; format: (v: number | null) => string }) {
  return (
    <div className="leading-tight py-0.5">
      <div>{format(value)}</div>
      {rank != null && <div className="text-[10px] text-gray-500">#{rank}</div>}
    </div>
  );
}

const columns: ColDef<TeamRanking>[] = [
  { field: 'rank', headerName: 'Rk', width: 60, pinned: 'left' },
  {
    field: 'name',
    headerName: 'Team',
    width: 200,
    pinned: 'left',
    filter: 'agTextColumnFilter',
    floatingFilter: true,
    cellRenderer: (p: { value: string }) => (
      <span className="text-blue-400 hover:underline cursor-pointer">{p.value}</span>
    ),
  },
  { field: 'conference', headerName: 'Conf', width: 120, filter: 'agTextColumnFilter', floatingFilter: true },
  {
    headerName: 'Record',
    width: 80,
    valueGetter: (p) => (p.data ? `${p.data.wins}-${p.data.losses}` : ''),
    sortable: false,
  },
  {
    field: 'adj_efficiency_margin',
    headerName: 'AdjEM',
    width: 85,
    valueFormatter: (p) => fmt(p.value),
    cellClassRules: {
      'text-green-400': (p) => (p.value ?? 0) > 0,
      'text-red-400': (p) => (p.value ?? 0) < 0,
    },
  },
  {
    field: 'adj_offense',
    headerName: 'AdjO',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.adj_offense} rank={p.data.adj_offense_rank} format={(v) => fmt(v)} />,
  },
  {
    field: 'adj_defense',
    headerName: 'AdjD',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.adj_defense} rank={p.data.adj_defense_rank} format={(v) => fmt(v)} />,
  },
  {
    field: 'adj_tempo',
    headerName: 'Tempo',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.adj_tempo} rank={p.data.adj_tempo_rank} format={(v) => fmt(v)} />,
  },
  {
    field: 'sos',
    headerName: 'SOS',
    width: 75,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.sos} rank={p.data.sos_rank} format={(v) => fmt(v, 2)} />,
  },
  {
    field: 'elo_rating',
    headerName: 'ELO',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.elo_rating} rank={p.data.elo_rank} format={(v) => fmt(v, 0)} />,
  },
  {
    field: 'effective_fg_pct',
    headerName: 'eFG%',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.effective_fg_pct} rank={p.data.effective_fg_pct_rank} format={pct} />,
  },
  {
    field: 'turnover_pct',
    headerName: 'TOV%',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.turnover_pct} rank={p.data.turnover_pct_rank} format={pct} />,
  },
  {
    field: 'off_rebound_pct',
    headerName: 'ORB%',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.off_rebound_pct} rank={p.data.off_rebound_pct_rank} format={pct} />,
  },
  {
    field: 'ft_rate',
    headerName: 'FTR',
    width: 75,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.ft_rate} rank={p.data.ft_rate_rank} format={(v) => fmt(v, 2)} />,
  },
  {
    field: 'opp_effective_fg_pct',
    headerName: 'OppeFG%',
    width: 95,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.opp_effective_fg_pct} rank={p.data.opp_effective_fg_pct_rank} format={pct} />,
  },
  {
    field: 'def_rebound_pct',
    headerName: 'DRB%',
    width: 80,
    cellRenderer: (p: { data: TeamRanking }) =>
      p.data && <RankedCell value={p.data.def_rebound_pct} rank={p.data.def_rebound_pct_rank} format={pct} />,
  },
];

export default function Rankings() {
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  const [loading, setLoading] = useState(true);
  const navigate = useNavigate();

  useEffect(() => {
    fetchTeamRankings()
      .then((r) => setTeams(r.teams))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div>
      <h1 className="text-2xl font-bold mb-4">Team Rankings</h1>
      <div style={{ height: 'calc(100vh - 160px)', width: '100%' }}>
        <AgGridReact<TeamRanking>
          theme={gridTheme}
          rowData={teams}
          columnDefs={columns}
          loading={loading}
          rowHeight={48}
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
