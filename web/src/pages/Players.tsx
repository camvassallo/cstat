import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import {
  AllCommunityModule,
  ModuleRegistry,
  type ColDef,
  type IDatasource,
  type GridApi,
} from 'ag-grid-community';
import { fetchPlayers, type PlayerRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor } from '../components/campom';
import { classColor, classTagline } from '../components/archetypeColors';
import { pctileTextColor } from '../components/pctile';
import { TableToolbar, TableSearchInput } from '../components/TableToolbar';

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');
// pss stores rate stats with mixed scale conventions, matching how the
// roster table handles them:
//   ast_pct / tov_pct / ft_rate: fractions (0–1), need ×100 for display
//   orb_pct / drb_pct / stl_pct / blk_pct: already percent-points (0–100)
const fracPct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');
const pointPct = (v: number | null) => (v != null ? v.toFixed(1) : '—');

const PAGE_SIZE = 100;

// Map AG Grid sort column id → backend sort field. Returning null for any
// column we don't sort on the server falls back to the default Campom sort.
function sortFieldFor(colId: string | undefined): string | null {
  if (!colId) return null;
  const map: Record<string, string> = {
    campom: 'campom',
    games_played: 'games_played',
    minutes_per_game: 'minutes_per_game',
    ppg: 'ppg',
    rpg: 'rpg',
    apg: 'apg',
    spg: 'spg',
    bpg: 'bpg',
    topg: 'topg',
    effective_fg_pct: 'effective_fg_pct',
    true_shooting_pct: 'true_shooting_pct',
    usage_rate: 'usage_rate',
    offensive_rating: 'offensive_rating',
    defensive_rating: 'defensive_rating',
    net_rating: 'net_rating',
    ast_pct: 'ast_pct',
    tov_pct: 'tov_pct',
    orb_pct: 'orb_pct',
    drb_pct: 'drb_pct',
    stl_pct: 'stl_pct',
    blk_pct: 'blk_pct',
    ft_rate: 'ft_rate',
  };
  return map[colId] ?? null;
}

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

type ColumnView = 'raw' | 'rate';

// Subtle vertical divider matching the roster table's `border-l border-gray-800`.
// Applied via inline style so it survives AG Grid's themed cell borders.
const CATEGORY_DIVIDER_STYLE = { borderLeft: '1px solid rgb(31 41 55)' } as const;

// Builds an AG Grid cellStyle that paints the value with the red→green
// percentile gradient and (optionally) prepends the category divider. Mirrors
// the roster table's `<ValueWithPctile>` + `border-l` pattern.
function gradientCellStyle(
  pctField: keyof PlayerRow,
  divider = false,
): ColDef<PlayerRow>['cellStyle'] {
  return (p) => {
    const raw = p.data?.[pctField];
    const pctile = typeof raw === 'number' ? raw : null;
    return {
      color: pctileTextColor(pctile),
      ...(divider ? CATEGORY_DIVIDER_STYLE : {}),
    };
  };
}

function buildColumns(view: ColumnView): ColDef<PlayerRow>[] {
  // Pinned identity / context columns. Mirrors the roster table's first block
  // (Player | Class) plus team / conf which the roster doesn't need (already
  // scoped to one team).
  const pinned: ColDef<PlayerRow>[] = [
    {
      field: 'name',
      headerName: 'Player',
      width: 180,
      pinned: 'left',
      cellRenderer: (p: { value: string; data?: PlayerRow }) => {
        const id = p.data?.player_id;
        if (!id) return <span>{p.value}</span>;
        return (
          <Link
            to={`/players/${id}`}
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
      width: 170,
      cellRenderer: (p: { value: string | null; data?: PlayerRow }) => {
        if (!p.value) return <span className="text-gray-500">—</span>;
        const teamId = p.data?.team_id;
        if (!teamId) return <span>{p.value}</span>;
        return (
          <Link
            to={`/teams/${teamId}`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </Link>
        );
      },
    },
    {
      field: 'conference',
      headerName: 'Conf',
      width: 100,
      sortable: false,
    },
    {
      headerName: 'Class',
      colId: 'archetype',
      // Wider than the previous 110 so "Barbarian / Sorcerer"–length combos
      // render in full without truncation.
      width: 170,
      sortable: false,
      cellRenderer: (p: { data?: PlayerRow }) => {
        const cls = p.data?.primary_class;
        if (!cls) return <span className="text-gray-600 text-xs">—</span>;
        const c = classColor(cls);
        const sec = p.data?.secondary_class;
        return (
          <span
            className="text-[10px] font-bold uppercase tracking-wide whitespace-nowrap"
            style={{ color: c }}
            title={sec ? `${cls} / ${sec}` : cls}
          >
            {cls}
            {sec && (
              <span
                className="ml-1 opacity-70"
                style={{ color: classColor(sec) }}
              >
                / {sec}
              </span>
            )}
          </span>
        );
      },
    },
  ];

  // Identity / volume block — always visible. CamPom kicks off a new visual
  // block so it gets the category divider; its renderer already applies its
  // own tier-based color, so no gradient cellStyle.
  const identity: ColDef<PlayerRow>[] = [
    {
      field: 'campom',
      headerName: 'CamPom',
      headerTooltip: 'Composite player valuation. Hover the chip for tier.',
      width: 120,
      sort: 'desc',
      cellRenderer: campomCellRenderer,
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
    },
    { field: 'games_played', headerName: 'GP', width: 60 },
    {
      field: 'minutes_per_game', headerName: 'MPG', width: 70,
      valueFormatter: (p) => fmt(p.value),
      cellStyle: gradientCellStyle('mpg_pct'),
    },
    {
      field: 'usage_rate', headerName: 'USG%', width: 80,
      valueFormatter: (p) => pct(p.value),
      cellStyle: gradientCellStyle('usage_rate_pct'),
    },
    {
      field: 'true_shooting_pct', headerName: 'TS%', width: 75,
      valueFormatter: (p) => pct(p.value),
      cellStyle: gradientCellStyle('true_shooting_pct_pct'),
    },
  ];

  // View-specific stat block. First column gets the divider — same pattern
  // the roster uses to break up volume from per-possession rate stats.
  const raw: ColDef<PlayerRow>[] = [
    {
      field: 'ppg', headerName: 'PPG', width: 70, valueFormatter: (p) => fmt(p.value),
      headerStyle: CATEGORY_DIVIDER_STYLE, cellStyle: gradientCellStyle('ppg_pct', true),
    },
    { field: 'rpg', headerName: 'RPG', width: 70, valueFormatter: (p) => fmt(p.value), cellStyle: gradientCellStyle('rpg_pct') },
    { field: 'apg', headerName: 'APG', width: 70, valueFormatter: (p) => fmt(p.value), cellStyle: gradientCellStyle('apg_pct') },
    { field: 'spg', headerName: 'SPG', width: 70, valueFormatter: (p) => fmt(p.value), cellStyle: gradientCellStyle('spg_pct') },
    { field: 'bpg', headerName: 'BPG', width: 70, valueFormatter: (p) => fmt(p.value), cellStyle: gradientCellStyle('bpg_pct') },
    { field: 'topg', headerName: 'TOPG', width: 75, valueFormatter: (p) => fmt(p.value), cellStyle: gradientCellStyle('topg_pct') },
  ];

  // Rate stats — display as bare percent numbers (unit lives in the header).
  // AST/TOV come in as fractions; ORB/DRB/STL/BLK come in as percent-points.
  const rate: ColDef<PlayerRow>[] = [
    {
      field: 'ast_pct', headerName: 'AST%', width: 80, valueFormatter: (p) => fracPct(p.value),
      headerTooltip: 'Assist rate',
      headerStyle: CATEGORY_DIVIDER_STYLE, cellStyle: gradientCellStyle('ast_pct_pct', true),
    },
    { field: 'tov_pct', headerName: 'TOV%', width: 80, valueFormatter: (p) => fracPct(p.value), headerTooltip: 'Turnover rate', cellStyle: gradientCellStyle('tov_pct_pct') },
    { field: 'orb_pct', headerName: 'ORB%', width: 80, valueFormatter: (p) => pointPct(p.value), headerTooltip: 'Offensive rebound rate', cellStyle: gradientCellStyle('orb_pct_pct') },
    { field: 'drb_pct', headerName: 'DRB%', width: 80, valueFormatter: (p) => pointPct(p.value), headerTooltip: 'Defensive rebound rate', cellStyle: gradientCellStyle('drb_pct_pct') },
    { field: 'stl_pct', headerName: 'STL%', width: 80, valueFormatter: (p) => pointPct(p.value), headerTooltip: 'Steal rate', cellStyle: gradientCellStyle('stl_pct_pct') },
    { field: 'blk_pct', headerName: 'BLK%', width: 80, valueFormatter: (p) => pointPct(p.value), headerTooltip: 'Block rate', cellStyle: gradientCellStyle('blk_pct_pct') },
  ];

  return [...pinned, ...identity, ...(view === 'raw' ? raw : rate)];
}

export default function Players() {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const archetype = searchParams.get('archetype');
  const includeSecondary = searchParams.get('include_secondary') === 'true';

  const [view, setView] = useState<ColumnView>('raw');
  const [searchInput, setSearchInput] = useState('');
  const [search, setSearch] = useState('');
  const [total, setTotal] = useState<number | null>(null);
  const gridApiRef = useRef<GridApi<PlayerRow> | null>(null);

  const columns = useMemo(() => buildColumns(view), [view]);

  // Debounce keystroke → backend re-fetch so the API isn't hit on every
  // character. 250ms feels live without flooding the server.
  useEffect(() => {
    const handle = setTimeout(() => {
      setSearch(searchInput.trim());
    }, 250);
    return () => clearTimeout(handle);
  }, [searchInput]);

  // Build a fresh datasource whenever the filters change. AG Grid's infinite
  // row model caches blocks keyed by start row; swapping the datasource (or
  // calling `purgeInfiniteCache()`) is what forces a refetch.
  const datasource = useMemo<IDatasource>(
    () => ({
      getRows: (params) => {
        const sortModel = params.sortModel?.[0];
        const sortField = sortFieldFor(sortModel?.colId);
        fetchPlayers({
          search: search || undefined,
          archetype: archetype || undefined,
          includeSecondaryArchetype: archetype != null && includeSecondary,
          sort: sortField || undefined,
          order: sortModel?.sort?.toLowerCase(),
          limit: params.endRow - params.startRow,
          offset: params.startRow,
        })
          .then((r) => {
            setTotal(r.total);
            // When this is the last block, pass the absolute total so AG Grid
            // can stop asking for more rows.
            const lastRow =
              params.startRow + r.players.length >= r.total ? r.total : undefined;
            params.successCallback(r.players, lastRow);
          })
          .catch((err) => {
            console.error('fetchPlayers failed', err);
            params.failCallback();
          });
      },
    }),
    [search, archetype, includeSecondary],
  );

  // Repoint the grid at the new datasource whenever it changes.
  useEffect(() => {
    gridApiRef.current?.setGridOption('datasource', datasource);
  }, [datasource]);

  const clearArchetype = useCallback(() => {
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev);
      next.delete('archetype');
      next.delete('include_secondary');
      return next;
    });
  }, [setSearchParams]);

  const toggleIncludeSecondary = useCallback(() => {
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev);
      if (includeSecondary) {
        next.delete('include_secondary');
      } else {
        next.set('include_secondary', 'true');
      }
      return next;
    });
  }, [includeSecondary, setSearchParams]);

  const archetypeColor = archetype ? classColor(archetype) : null;
  const archetypeBlurb = archetype ? classTagline(archetype) : null;

  return (
    <div>
      <TableToolbar
        title="Player Stats"
        count={total}
        countLabel="qualified"
        search={
          <TableSearchInput
            value={searchInput}
            onChange={setSearchInput}
            placeholder="Search players…"
          />
        }
        controls={
          <>
            <span className="text-xs text-gray-500">View</span>
            <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs">
              <button
                onClick={() => setView('raw')}
                className={`px-2.5 py-1 ${
                  view === 'raw'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                }`}
              >
                Raw
              </button>
              <button
                onClick={() => setView('rate')}
                className={`px-2.5 py-1 ${
                  view === 'rate'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                }`}
              >
                Rate
              </button>
            </div>
          </>
        }
      />

      {archetype && (
        <div
          className="flex flex-wrap items-center gap-3 mb-3 px-3 py-2 rounded border-l-4 bg-gray-800/60"
          style={{ borderLeftColor: archetypeColor ?? undefined }}
        >
          <div className="flex items-baseline gap-2">
            <span className="text-xs text-gray-500 uppercase tracking-wide">Class</span>
            <span
              className="text-sm font-bold"
              style={{ color: archetypeColor ?? undefined }}
            >
              {archetype}
            </span>
            {archetypeBlurb && (
              <span className="text-xs text-gray-400">— {archetypeBlurb}</span>
            )}
          </div>
          <label className="flex items-center gap-2 text-xs text-gray-300 cursor-pointer">
            <input
              type="checkbox"
              checked={includeSecondary}
              onChange={toggleIncludeSecondary}
              className="rounded"
            />
            Include secondary class
          </label>
          <button
            onClick={clearArchetype}
            className="text-xs px-2 py-1 rounded bg-gray-700 hover:bg-gray-600 text-gray-200 ml-auto"
          >
            Clear filter
          </button>
        </div>
      )}

      <div style={{ height: 'calc(100vh - 180px)', width: '100%' }}>
        <AgGridReact<PlayerRow>
          theme={gridTheme}
          columnDefs={columns}
          rowModelType="infinite"
          datasource={datasource}
          cacheBlockSize={PAGE_SIZE}
          maxBlocksInCache={10}
          infiniteInitialRowCount={50}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
          onGridReady={(e) => {
            gridApiRef.current = e.api;
          }}
          onRowClicked={(e) => {
            // If the click landed on an explicit link (player or team name),
            // let the Link handle navigation — don't double-navigate to the
            // player detail page.
            const target = e.event?.target as HTMLElement | undefined;
            if (target?.closest('a')) return;
            if (e.data) navigate(`/players/${e.data.player_id}`);
          }}
          getRowId={(p) => p.data.player_id}
        />
      </div>
    </div>
  );
}
