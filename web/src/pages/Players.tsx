import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import {
  AllCommunityModule,
  ModuleRegistry,
  type ColDef,
} from 'ag-grid-community';
import { fetchPlayers, type PlayerRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor, campomTitle, campomHalfColor } from '../components/campom';
import { agNullsBottom } from '../components/tableSort';
import { classColor, CLASS_ORDER } from '../components/archetypeColors';
import ArchetypeFilter, { type MatchMode } from '../components/ArchetypeFilter';
import { pctileTextColor } from '../components/pctile';
import { fracPct, pointPct } from '../components/format';
import { TableToolbar, TableSearchInput } from '../components/TableToolbar';
import TransferPortal from '../components/TransferPortal';
import RecruitClass from '../components/RecruitClass';
import { SeasonLink } from '../components/SeasonLink';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

ModuleRegistry.registerModules([AllCommunityModule]);

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');

// Match the API cap (`PLAYER_LIST_MAX_LIMIT` in `crates/cstat-api/src/routes/
// players.rs`). The qualified pool (5+ GP, 10+ MPG) is ~2-3k for a typical
// season, so a single fetch covers it with headroom.
const PAGE_FETCH_LIMIT = 5000;

const campomCellRenderer = (p: { value: number | null; data?: PlayerRow }) => {
  if (p.value == null) return <span className="text-slate-500">—</span>;
  const tier = campomTier(p.value);
  const pctVal = p.data?.campom_pct;
  const pctStr = pctVal != null ? Math.round(pctVal * 100) : null;
  return (
    <span className="inline-flex items-baseline gap-2">
      <span
        className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
        title={campomTitle(p.value, p.data?.campom_o, p.data?.campom_d)}
      >
        {p.value.toFixed(1)}
      </span>
      {pctStr != null && <span className="text-slate-400 text-xs">{pctStr}</span>}
    </span>
  );
};

// O/D CamPom halves — signed values on the shared diverging red→green
// gradient (per-half saturation, see campomHalfColor), gated server-side
// (±30 sanity envelope; unstable rows arrive null and render "—").
const campomHalfRenderer = (side: 'o' | 'd') =>
  function CampomHalfCell(p: { value: number | null }) {
    return p.value != null ? (
      <span className="tabular-nums text-xs" style={{ color: campomHalfColor(p.value, side) }}>
        {`${p.value > 0 ? '+' : ''}${p.value.toFixed(1)}`}
      </span>
    ) : (
      <span className="text-slate-500">—</span>
    );
  };

type ColumnView = 'raw' | 'rate';
type PageMode = 'all' | 'transfers' | 'recruits';

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

function buildColumns(
  view: ColumnView,
  campomRank: Map<string, number>,
  isDesktop: boolean,
): ColDef<PlayerRow>[] {
  // Pinned identity / context columns. Mirrors the roster table's first block
  // (Player | Class) plus team / conf which the roster doesn't need (already
  // scoped to one team).
  const pinned: ColDef<PlayerRow>[] = [
    {
      headerName: 'Rk',
      colId: 'campom_rank',
      headerTooltip: 'CamPom rank within the loaded pool (best CamPom = 1).',
      width: 56,
      pinned: 'left',
      sortable: false,
      // The synthetic rank is not a searchable attribute — keep it out of the
      // quick-filter corpus so typing "5" doesn't match rank-5 rows.
      getQuickFilterText: () => '',
      // The player's CamPom rank over the loaded qualified pool, NOT their
      // position in the currently displayed rows. Reading a filtered row index
      // here would renumber 1..N over whatever the search matched (issue #121);
      // `campomRank` is bound to the row's data, so it stays fixed under
      // search/sort and only changes when the season or archetype filter changes.
      valueGetter: (p) => (p.data ? (campomRank.get(p.data.player_id) ?? null) : null),
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
      // Desktop: a roomy single-line column so full names ("Olusegun-Kupono
      // Aderoju") render without wrapping or clipping. Mobile: deliberately
      // tight so the pinned identity block doesn't crowd out the stat columns —
      // long names wrap to a second line (wrapText) at the 44px row height.
      width: isDesktop ? 240 : 132,
      pinned: 'left',
      wrapText: !isDesktop,
      cellRenderer: (p: { value: string; data?: PlayerRow }) => {
        const id = p.data?.player_id;
        if (!id) return <span>{p.value}</span>;
        return (
          <SeasonLink
            to={`/players/${id}`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </SeasonLink>
        );
      },
    },
    {
      field: 'team_name',
      headerName: 'Team',
      width: 138,
      wrapText: true,
      cellRenderer: (p: { value: string | null; data?: PlayerRow }) => {
        if (!p.value) return <span className="text-gray-500">—</span>;
        const teamId = p.data?.team_id;
        if (!teamId) return <span>{p.value}</span>;
        return (
          <SeasonLink
            to={`/teams/${teamId}`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </SeasonLink>
        );
      },
    },
    {
      field: 'conference',
      headerName: 'Conf',
      width: 80,
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
            className="text-xs font-bold uppercase tracking-wide whitespace-nowrap"
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
      headerTooltip: 'Composite player valuation. Hover the chip for tier and the O/D split.',
      width: 120,
      sort: 'desc',
      // First click sorts best-first, matching CPO/CPD.
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomCellRenderer,
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
    },
    {
      field: 'campom_o',
      headerName: 'CPO',
      headerTooltip:
        "CamPom's offensive half (O + D = CamPom, same per-100 scale). Hidden where the decomposition is numerically unstable (±30 sanity envelope).",
      width: 70,
      // First click sorts best-first (higher = better), matching CamPom.
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomHalfRenderer('o'),
    },
    {
      field: 'campom_d',
      headerName: 'CPD',
      headerTooltip:
        "CamPom's defensive half — positive is GOOD (defensive value added; O + D = CamPom). Hidden where the decomposition is numerically unstable.",
      width: 70,
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomHalfRenderer('d'),
    },
    { field: 'games_played', headerName: 'GP', width: 60 },
    {
      field: 'minutes_per_game', headerName: 'MPG', width: 70,
      valueFormatter: (p) => fmt(p.value),
      cellStyle: gradientCellStyle('mpg_pct'),
    },
    {
      field: 'usage_rate', headerName: 'USG%', width: 80,
      valueFormatter: (p) => fracPct(p.value),
      cellStyle: gradientCellStyle('usage_rate_pct'),
    },
    {
      field: 'true_shooting_pct', headerName: 'TS%', width: 75,
      valueFormatter: (p) => fracPct(p.value),
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
  const { season } = useSeason();
  usePageTitle('Players');
  const [searchParams, setSearchParams] = useSearchParams();
  // Multi-archetype filter: `?archetypes=Paladin,Monk` (union/OR). The legacy
  // single-class `?archetype=Wizard` (still emitted by the /archetypes cards and
  // old bookmarks) is folded into the same set so those links keep working.
  const selectedClasses = useMemo(() => {
    const set = new Set<string>();
    const multi = searchParams.get('archetypes');
    if (multi) multi.split(',').map((s) => s.trim()).filter(Boolean).forEach((c) => set.add(c));
    const legacy = searchParams.get('archetype');
    if (legacy) set.add(legacy);
    return set;
  }, [searchParams]);
  const includeSecondary = searchParams.get('include_secondary') === 'true';
  const matchMode: MatchMode = searchParams.get('match') === 'all' ? 'all' : 'any';
  const modeParam = searchParams.get('mode');
  const mode: PageMode =
    modeParam === 'transfers'
      ? 'transfers'
      : modeParam === 'recruits'
        ? 'recruits'
        : 'all';

  const setMode = useCallback(
    (next: PageMode) => {
      setSearchParams((prev) => {
        const p = new URLSearchParams(prev);
        if (next === 'all') p.delete('mode');
        else p.set('mode', next);
        return p;
      });
    },
    [setSearchParams],
  );

  const [view, setView] = useState<ColumnView>('raw');
  const [searchInput, setSearchInput] = useState('');
  const [rows, setRows] = useState<PlayerRow[]>([]);
  const [loading, setLoading] = useState(true);

  // Track the `md` breakpoint (Tailwind's 768px) so the Player column can run
  // full-width on desktop and stay tight (wrapping) on mobile — AG Grid column
  // defs can't carry CSS media queries, so the breakpoint is read in JS.
  const [isDesktop, setIsDesktop] = useState(
    () => window.matchMedia('(min-width: 768px)').matches,
  );
  useEffect(() => {
    const mq = window.matchMedia('(min-width: 768px)');
    const onChange = (e: MediaQueryListEvent) => setIsDesktop(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  // Apply the archetype filter client-side over the already-loaded pool — the
  // full qualified set is in memory, so re-filtering is instant with no
  // round-trip. Empty selection = no filter. Two modes:
  //   'any' (union): primary is selected, or — with include-secondary on —
  //          secondary is. The default.
  //   'all' (intersection): the player must hold EVERY selected class across
  //          {primary, secondary}. Players have at most two classes, so this
  //          always considers the secondary and returns nothing for 3+ selected.
  const displayedRows = useMemo(() => {
    if (selectedClasses.size === 0) return rows;
    if (matchMode === 'all') {
      return rows.filter((p) => {
        for (const c of selectedClasses) {
          if (p.primary_class !== c && p.secondary_class !== c) return false;
        }
        return true;
      });
    }
    return rows.filter(
      (p) =>
        (p.primary_class != null && selectedClasses.has(p.primary_class)) ||
        (includeSecondary &&
          p.secondary_class != null &&
          selectedClasses.has(p.secondary_class)),
    );
  }, [rows, selectedClasses, includeSecondary, matchMode]);

  // CamPom rank over the displayed (post-filter) pool (best = 1), keyed by
  // player_id. A function of the loaded pool + archetype filter only —
  // deliberately NOT of search/sort, so the rank stays fixed to each player
  // (issue #121). With an archetype filter active the rank is within that
  // class union, matching the prior server-filtered behavior.
  const campomRank = useMemo(() => {
    const m = new Map<string, number>();
    [...displayedRows]
      .filter((p) => p.campom != null)
      .sort((a, b) => (b.campom as number) - (a.campom as number))
      .forEach((p, i) => m.set(p.player_id, i + 1));
    return m;
  }, [displayedRows]);

  const columns = useMemo(
    () => buildColumns(view, campomRank, isDesktop),
    [view, campomRank, isDesktop],
  );

  // Single fetch loads the entire qualified pool; sort + search run client-
  // side via AG Grid's built-in sorting and `quickFilterText`, and the
  // archetype filter (above) is also client-side. Pagination (set on the
  // grid below) keeps DOM small. Re-fetches only when the season changes.
  //
  // No `setLoading(true)` here — `react-hooks/set-state-in-effect` forbids
  // it. The initial `useState(true)` covers first load; on a season change
  // the previous data stays visible until the new fetch resolves. Mild
  // stale-flicker, matches the Rankings page pattern.
  useEffect(() => {
    let cancelled = false;
    fetchPlayers({
      season,
      limit: PAGE_FETCH_LIMIT,
    })
      .then((r) => {
        if (cancelled) return;
        setRows(r.players);
      })
      .catch((err) => {
        console.error('fetchPlayers failed', err);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [season]);

  // Toggle one class in the URL-backed selection. Normalizes any legacy
  // `?archetype=` into the canonical `?archetypes=` list and keeps the list in
  // CLASS_ORDER for stable, readable URLs.
  const toggleClass = useCallback(
    (cls: string) => {
      setSearchParams((prev) => {
        const next = new URLSearchParams(prev);
        const set = new Set<string>();
        const multi = next.get('archetypes');
        if (multi) multi.split(',').map((s) => s.trim()).filter(Boolean).forEach((c) => set.add(c));
        const legacy = next.get('archetype');
        if (legacy) {
          set.add(legacy);
          next.delete('archetype');
        }
        if (set.has(cls)) set.delete(cls);
        else set.add(cls);
        if (set.size === 0) {
          next.delete('archetypes');
          next.delete('include_secondary');
          next.delete('match');
        } else {
          next.set(
            'archetypes',
            CLASS_ORDER.filter((c) => set.has(c)).join(','),
          );
        }
        return next;
      });
    },
    [setSearchParams],
  );

  const setMatchMode = useCallback(
    (m: MatchMode) => {
      setSearchParams((prev) => {
        const next = new URLSearchParams(prev);
        if (m === 'all') next.set('match', 'all');
        else next.delete('match');
        return next;
      });
    },
    [setSearchParams],
  );

  const clearArchetypes = useCallback(() => {
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev);
      next.delete('archetypes');
      next.delete('archetype');
      next.delete('include_secondary');
      next.delete('match');
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

  // Top-level page-mode tabs (Players ↔ Transfer Portal). Sits above the
  // mode-specific toolbar so the two grids share a single chrome header.
  const modeTabs = (
    <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs mb-3">
      <button
        onClick={() => setMode('all')}
        className={`px-3 py-1 ${
          mode === 'all'
            ? 'bg-blue-600 text-white'
            : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
        }`}
      >
        Players
      </button>
      <button
        onClick={() => setMode('transfers')}
        className={`px-3 py-1 ${
          mode === 'transfers'
            ? 'bg-blue-600 text-white'
            : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
        }`}
      >
        Transfer Portal
      </button>
      <button
        onClick={() => setMode('recruits')}
        className={`px-3 py-1 ${
          mode === 'recruits'
            ? 'bg-blue-600 text-white'
            : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
        }`}
      >
        Recruits
      </button>
    </div>
  );

  if (mode === 'transfers') {
    // The transfer portal is per-season — surface the current site-selected
    // year so switching seasons in the nav repoints the list at the right
    // portal class.
    return (
      <div>
        {modeTabs}
        <TransferPortal year={season} />
      </div>
    );
  }

  if (mode === 'recruits') {
    // Recruiting class year = spring of HS graduation. We anchor it to the
    // current site-selected season so navigating "2026" in the dropdown
    // shows the class-of-2026 entering cstat-season 2027.
    return (
      <div>
        {modeTabs}
        <RecruitClass year={season} />
      </div>
    );
  }

  return (
    <div>
      {modeTabs}
      <TableToolbar
        title="Player Stats"
        count={displayedRows.length}
        countLabel={selectedClasses.size > 0 ? 'matching' : 'qualified'}
        search={
          <TableSearchInput
            value={searchInput}
            onChange={setSearchInput}
            placeholder="Search players…"
          />
        }
        controls={
          <>
            <ArchetypeFilter
              selected={selectedClasses}
              onToggle={toggleClass}
              matchMode={matchMode}
              onSetMatchMode={setMatchMode}
              includeSecondary={includeSecondary}
              onToggleIncludeSecondary={toggleIncludeSecondary}
              onClear={clearArchetypes}
            />
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

      {selectedClasses.size > 0 && (
        <div className="flex flex-wrap items-center gap-2 mb-3">
          <span className="text-xs text-gray-500 uppercase tracking-wide">Filtering</span>
          {CLASS_ORDER.filter((c) => selectedClasses.has(c)).map((cls, i) => (
            <Fragment key={cls}>
              {i > 0 && (
                <span className="text-xs text-gray-500">
                  {matchMode === 'all' ? 'and' : 'or'}
                </span>
              )}
              <button
                onClick={() => toggleClass(cls)}
                className="inline-flex items-center gap-1.5 pl-2 pr-1.5 py-0.5 rounded-full border text-xs"
                style={{ borderColor: classColor(cls), color: classColor(cls) }}
                title={`Remove ${cls}`}
              >
                <span
                  className="inline-block w-2 h-2 rounded-full"
                  style={{ backgroundColor: classColor(cls) }}
                />
                {cls}
                <span className="text-gray-400 hover:text-gray-200">✕</span>
              </button>
            </Fragment>
          ))}
          {matchMode === 'any' && includeSecondary && (
            <span className="text-xs text-gray-400">+ secondary class</span>
          )}
          <button
            onClick={clearArchetypes}
            className="text-xs px-2 py-0.5 rounded bg-gray-700 hover:bg-gray-600 text-gray-200"
          >
            Clear
          </button>
        </div>
      )}

      {/* Same `domLayout="autoHeight"` strategy as Rankings — the grid grows
          to fit one page of rows, and the page itself is the only vertical
          scroll surface. With ~2-3k qualified players a single page in DOM
          would be too heavy, so AG Grid's built-in pagination caps the
          rendered page to 100 rows; the user clicks next/prev or jumps to
          a page via the pagination footer. Sort + search are client-side
          across the full dataset so the qualified pool stays one fetch. */}
      <div style={{ width: '100%' }}>
        <AgGridReact<PlayerRow>
          theme={gridTheme}
          rowData={displayedRows}
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
            // Wrap header text rather than clipping. Header labels like
            // "USG%" / "TOPG" / rate-stat columns otherwise lose characters
            // when the column compresses on mobile.
            wrapHeaderText: true,
            autoHeaderHeight: true,
          }}
          getRowId={(p) => p.data.player_id}
        />
      </div>
    </div>
  );
}
