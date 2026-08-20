import { useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import { AllCommunityModule, ModuleRegistry, type ColDef } from 'ag-grid-community';
import {
  fetchProjectedPlayers,
  type ProjectedPlayer,
  type ProjectionSource,
} from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor, campomTitle } from './campom';
import { classColor } from './archetypeColors';
import { agNullsBottom } from './tableSort';
import { TableToolbar, TableSearchInput } from './TableToolbar';
import { seasonHref } from './season';

ModuleRegistry.registerModules([AllCommunityModule]);

// Cohort chip. Returners/transfers carry a real base-season player row (and a
// link); freshmen are synthesized from a recruit commit (no player page).
const SOURCE_META: Record<ProjectionSource, { label: string; cls: string; title: string }> = {
  returning: {
    label: 'Ret',
    cls: 'border-emerald-500/60 text-emerald-300',
    title: 'Returning player — projected from last season',
  },
  transfer: {
    label: 'Tfr',
    cls: 'border-amber-500/60 text-amber-300',
    title: 'Incoming transfer — projected from their source-team season',
  },
  freshman: {
    label: 'Fr',
    cls: 'border-sky-500/60 text-sky-300',
    title: 'Incoming freshman recruit — freshman-model projection',
  },
  uncertain: {
    label: '?',
    cls: 'border-slate-400/60 text-slate-300',
    title:
      'Eligibility or draft status unresolved — counted in this team\u2019s ceiling but not its floor',
  },
};

const fmtBand = (v: number | null) => (v != null ? v.toFixed(1) : '—');

const campomCellRenderer = (p: { value: number | null }) => {
  if (p.value == null) return <span className="text-slate-500">—</span>;
  const tier = campomTier(p.value);
  return (
    <span
      className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
      title={campomTitle(p.value)}
    >
      {p.value.toFixed(1)}
    </span>
  );
};

function buildColumns(
  rank: Map<string, number>,
  baseSeason: number,
  year: number,
  isDesktop: boolean,
): ColDef<ProjectedPlayer>[] {
  return [
    {
      headerName: 'Rk',
      colId: 'proj_rank',
      headerTooltip: 'Projected-CamPom rank within the loaded pool (best = 1).',
      width: 56,
      pinned: 'left',
      sortable: false,
      getQuickFilterText: () => '',
      valueGetter: (p) => (p.data ? (rank.get(p.data.player_id) ?? null) : null),
      cellRenderer: (p: { value: number | null }) =>
        p.value == null ? (
          <span className="text-slate-600">—</span>
        ) : (
          <span className="text-gray-400 tabular-nums">{p.value}</span>
        ),
    },
    {
      field: 'name',
      headerName: 'Player',
      width: isDesktop ? 220 : 132,
      pinned: 'left',
      wrapText: !isDesktop,
      cellRenderer: (p: { value: string; data?: ProjectedPlayer }) => {
        const d = p.data;
        if (!d) return <span>{p.value}</span>;
        // Freshmen have no player detail page (they're recruit commits).
        if (d.source === 'freshman') return <span>{p.value}</span>;
        return (
          <Link
            to={seasonHref(`/players/${d.player_id}`, baseSeason)}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </Link>
        );
      },
    },
    {
      field: 'team_name',
      headerName: 'Team',
      width: 140,
      wrapText: true,
      cellRenderer: (p: { value: string | null; data?: ProjectedPlayer }) => {
        if (!p.value) return <span className="text-gray-500">—</span>;
        const teamId = p.data?.team_id;
        if (!teamId) return <span>{p.value}</span>;
        // Link to the team's FUTURE page for the projected year (same deep-link
        // the /projected grid uses: `?season={year}&view=projected`).
        return (
          <Link
            to={`/teams/${teamId}?season=${year}&view=projected`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </Link>
        );
      },
    },
    {
      field: 'source',
      headerName: 'Type',
      width: 74,
      headerTooltip:
        'Ret = returning · Tfr = incoming transfer · Fr = incoming freshman · ? = status unresolved (ceiling only)',
      cellRenderer: (p: { value: ProjectionSource }) => {
        const m = SOURCE_META[p.value];
        if (!m) return <span className="text-gray-500">—</span>;
        return (
          <span
            className={`px-1.5 py-0.5 rounded border text-xs font-semibold ${m.cls}`}
            title={m.title}
          >
            {m.label}
          </span>
        );
      },
    },
    {
      field: 'class_year',
      headerName: 'Cl',
      width: 64,
      valueFormatter: (p) => p.value ?? '—',
    },
    {
      field: 'primary_archetype',
      headerName: 'Archetype',
      colId: 'archetype',
      width: 150,
      sortable: false,
      cellRenderer: (p: { value: string | null }) => {
        if (!p.value) return <span className="text-gray-600 text-xs">—</span>;
        return (
          <span
            className="text-xs font-bold uppercase tracking-wide whitespace-nowrap"
            style={{ color: classColor(p.value) }}
          >
            {p.value}
          </span>
        );
      },
    },
    {
      field: 'campom',
      headerName: 'Proj CamPom',
      headerTooltip: 'Projected CamPom (model mean) for the upcoming season.',
      width: 130,
      sort: 'desc',
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomCellRenderer,
    },
    {
      field: 'campom_lower',
      headerName: 'Floor',
      headerTooltip: 'q10 of the projection band (low-end outcome).',
      width: 78,
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      valueFormatter: (p) => fmtBand(p.value),
      cellStyle: { color: 'rgb(148 163 184)' },
    },
    {
      field: 'campom_upper',
      headerName: 'Ceil',
      headerTooltip: 'q90 of the projection band (high-end outcome).',
      width: 78,
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      valueFormatter: (p) => fmtBand(p.value),
      cellStyle: { color: 'rgb(148 163 184)' },
    },
    {
      field: 'composite_rank',
      headerName: '247 Rk',
      headerTooltip: '247Sports composite national rank (recruits only).',
      width: 84,
      sortingOrder: ['asc', 'desc', null],
      comparator: agNullsBottom,
      valueFormatter: (p) => (p.value != null ? `#${p.value}` : '—'),
      cellStyle: { color: 'rgb(148 163 184)' },
    },
  ];
}

/** Projected-player ranking for an upcoming (not-yet-played) season, read from
 *  the materialized `player_season_projection` table. Rendered by the Players
 *  page when the season picker is set to the upcoming projection year. */
export default function ProjectedPlayers({ year }: { year: number }) {
  const [rows, setRows] = useState<ProjectedPlayer[]>([]);
  const [baseSeason, setBaseSeason] = useState(year - 1);
  const [loading, setLoading] = useState(true);
  const [searchInput, setSearchInput] = useState('');

  const [isDesktop, setIsDesktop] = useState(
    () => window.matchMedia('(min-width: 768px)').matches,
  );
  useEffect(() => {
    const mq = window.matchMedia('(min-width: 768px)');
    const onChange = (e: MediaQueryListEvent) => setIsDesktop(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  // No `setLoading(true)` on re-fetch — `react-hooks/set-state-in-effect`
  // forbids a synchronous setState in the effect body. Initial `useState(true)`
  // covers first load; on a `year` change the prior rows stay until the new
  // fetch resolves (mild stale-flicker, matching the Players grid pattern).
  useEffect(() => {
    let cancelled = false;
    fetchProjectedPlayers(year)
      .then((r) => {
        if (cancelled) return;
        setRows(r.players);
        setBaseSeason(r.base_season);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('fetchProjectedPlayers failed', err);
        setRows([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [year]);

  // Projected-CamPom rank over the loaded pool (best = 1), keyed by player_id.
  // Fixed to each player regardless of search/sort (mirrors the Players grid).
  const rank = useMemo(() => {
    const m = new Map<string, number>();
    [...rows]
      .filter((p) => p.campom != null)
      .sort((a, b) => b.campom - a.campom)
      .forEach((p, i) => m.set(p.player_id, i + 1));
    return m;
  }, [rows]);

  const columns = useMemo(
    () => buildColumns(rank, baseSeason, year, isDesktop),
    [rank, baseSeason, year, isDesktop],
  );

  return (
    <div>
      <TableToolbar
        title={`${year} Projected Players`}
        count={rows.length}
        countLabel="projected"
        search={
          <TableSearchInput
            value={searchInput}
            onChange={setSearchInput}
            placeholder="Search players…"
          />
        }
      />
      <p className="text-xs text-gray-500 mb-3">
        Projected CamPom for the upcoming {year} season — returners, incoming
        transfers, and committed freshmen, composed from the {baseSeason} roster
        via cstat's trajectory and freshman-impact models. Ranked by the model
        mean; Floor / Ceil are the q10 / q90 band.
      </p>

      {!loading && rows.length === 0 ? (
        <div className="rounded border border-gray-700 bg-gray-900/40 p-6 text-sm text-gray-400">
          No projections have been computed for {year} yet.
        </div>
      ) : (
        <div style={{ width: '100%' }}>
          <AgGridReact<ProjectedPlayer>
            theme={gridTheme}
            rowData={rows}
            columnDefs={columns}
            loading={loading}
            domLayout="autoHeight"
            pagination
            paginationPageSize={100}
            paginationPageSizeSelector={[50, 100, 200]}
            rowHeight={44}
            quickFilterText={searchInput}
            defaultColDef={{
              sortable: true,
              resizable: true,
              suppressMovable: true,
              wrapHeaderText: true,
              autoHeaderHeight: true,
            }}
            getRowId={(p) => p.data.player_id}
          />
        </div>
      )}
    </div>
  );
}
