import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import { AllCommunityModule, ModuleRegistry, type ColDef } from 'ag-grid-community';
import { fetchPlayers, type PlayerRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor } from '../components/campom';

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');

const campomCellRenderer = (p: { value: number | null; data?: PlayerRow }) => {
  if (p.value == null) return <span className="text-slate-500">—</span>;
  const tier = campomTier(p.value);
  const pctVal = p.data?.campom_pct;
  const pctStr = pctVal != null ? Math.round(pctVal * 100) : null;
  return (
    <span className="inline-flex items-baseline gap-2">
      <span className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`} title={tier ?? ''}>
        {p.value.toFixed(1)}
      </span>
      {pctStr != null && <span className="text-slate-400 text-xs">{pctStr}</span>}
    </span>
  );
};

function buildColumns(navigate: (to: string) => void): ColDef<PlayerRow>[] {
  return [
    {
      field: 'name',
      headerName: 'Player',
      width: 180,
      pinned: 'left',
      filter: 'agTextColumnFilter',
      floatingFilter: true,
      cellRenderer: (p: { value: string }) => (
        <span className="text-blue-400 hover:underline cursor-pointer">{p.value}</span>
      ),
    },
    {
      field: 'team_name',
      headerName: 'Team',
      width: 170,
      filter: 'agTextColumnFilter',
      floatingFilter: true,
      cellRenderer: (p: { value: string | null; data?: PlayerRow }) => {
        if (!p.value) return <span className="text-gray-500">—</span>;
        const teamId = p.data?.team_id;
        if (!teamId) return <span>{p.value}</span>;
        return (
          <span
            className="text-blue-400 hover:underline cursor-pointer"
            onClick={(e) => {
              e.stopPropagation();
              navigate(`/teams/${teamId}`);
            }}
          >
            {p.value}
          </span>
        );
      },
    },
    {
      field: 'conference',
      headerName: 'Conf',
      width: 110,
      filter: 'agTextColumnFilter',
      floatingFilter: true,
    },
    {
      field: 'campom',
      headerName: 'CamPom',
      headerTooltip: 'Composite player valuation. Hover the chip for tier.',
      width: 120,
      sort: 'desc',
      cellRenderer: campomCellRenderer,
    },
    { field: 'games_played', headerName: 'GP', width: 60 },
    { field: 'minutes_per_game', headerName: 'MPG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'ppg', headerName: 'PPG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'rpg', headerName: 'RPG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'apg', headerName: 'APG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'spg', headerName: 'SPG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'bpg', headerName: 'BPG', width: 70, valueFormatter: (p) => fmt(p.value) },
    { field: 'effective_fg_pct', headerName: 'eFG%', width: 80, valueFormatter: (p) => pct(p.value) },
    { field: 'true_shooting_pct', headerName: 'TS%', width: 75, valueFormatter: (p) => pct(p.value) },
    { field: 'usage_rate', headerName: 'USG%', width: 80, valueFormatter: (p) => pct(p.value) },
    { field: 'offensive_rating', headerName: 'ORTG', width: 75, valueFormatter: (p) => fmt(p.value) },
    { field: 'defensive_rating', headerName: 'DRTG', width: 75, valueFormatter: (p) => fmt(p.value) },
    {
      field: 'net_rating',
      headerName: 'NET',
      width: 70,
      valueFormatter: (p) => fmt(p.value),
      cellClassRules: {
        'text-green-400': (p) => (p.value ?? 0) > 0,
        'text-red-400': (p) => (p.value ?? 0) < 0,
      },
    },
  ];
}

export default function Players() {
  const [players, setPlayers] = useState<PlayerRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState('');
  const navigate = useNavigate();
  const columns = buildColumns(navigate);

  const load = (q?: string) => {
    setLoading(true);
    fetchPlayers({ search: q || undefined, limit: 200 })
      .then((r) => setPlayers(r.players))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchPlayers({ limit: 200 })
      .then((r) => setPlayers(r.players))
      .finally(() => setLoading(false));
  }, []);

  const handleSearch = (e: React.FormEvent) => {
    e.preventDefault();
    load(search);
  };

  return (
    <div>
      <div className="flex items-center gap-4 mb-4">
        <h1 className="text-2xl font-bold">Player Stats</h1>
        <form onSubmit={handleSearch} className="flex gap-2">
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search players..."
            className="bg-gray-800 border border-gray-600 rounded px-3 py-1.5 text-sm text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
          />
          <button type="submit" className="bg-blue-600 hover:bg-blue-700 text-white text-sm px-3 py-1.5 rounded">
            Search
          </button>
        </form>
      </div>
      <div style={{ height: 'calc(100vh - 160px)', width: '100%' }}>
        <AgGridReact<PlayerRow>
          theme={gridTheme}
          rowData={players}
          columnDefs={columns}
          loading={loading}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
          onRowClicked={(e) => {
            if (e.data) navigate(`/players/${e.data.player_id}`);
          }}
          getRowId={(p) => p.data.player_id}
        />
      </div>
    </div>
  );
}
