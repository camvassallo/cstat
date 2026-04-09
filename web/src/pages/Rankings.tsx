import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import { AllCommunityModule, ModuleRegistry, type ColDef } from 'ag-grid-community';
import { fetchTeamRankings, type TeamRanking } from '../api/client';
import { gridTheme } from '../theme';

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');

const columns: ColDef<TeamRanking>[] = [
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
  { field: 'conference', headerName: 'Conf', width: 100 },
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
  { field: 'adj_offense', headerName: 'AdjO', width: 80, valueFormatter: (p) => fmt(p.value) },
  { field: 'adj_defense', headerName: 'AdjD', width: 80, valueFormatter: (p) => fmt(p.value) },
  { field: 'adj_tempo', headerName: 'Tempo', width: 80, valueFormatter: (p) => fmt(p.value) },
  { field: 'sos', headerName: 'SOS', width: 75, valueFormatter: (p) => fmt(p.value, 2) },
  { field: 'elo_rating', headerName: 'ELO', width: 80, valueFormatter: (p) => fmt(p.value, 0) },
  { field: 'elo_rank', headerName: 'ELO Rk', width: 80, valueFormatter: (p) => fmt(p.value, 0) },
  { field: 'effective_fg_pct', headerName: 'eFG%', width: 80, valueFormatter: (p) => pct(p.value) },
  { field: 'turnover_pct', headerName: 'TOV%', width: 80, valueFormatter: (p) => pct(p.value) },
  { field: 'off_rebound_pct', headerName: 'ORB%', width: 80, valueFormatter: (p) => pct(p.value) },
  { field: 'ft_rate', headerName: 'FTR', width: 75, valueFormatter: (p) => fmt(p.value, 2) },
  { field: 'opp_effective_fg_pct', headerName: 'OppeFG%', width: 95, valueFormatter: (p) => pct(p.value) },
  { field: 'def_rebound_pct', headerName: 'DRB%', width: 80, valueFormatter: (p) => pct(p.value) },
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
