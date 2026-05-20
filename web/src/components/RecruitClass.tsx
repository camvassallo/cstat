import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchRecruits, type RecruitRow } from '../api/client';
import { campomTier, campomTierColor } from './campom';
import { gridTheme } from '../theme';
import { SeasonLink } from './SeasonLink';
import { useIsMobile } from './useIsMobile';

// Star rating glyph row. Filled stars are amber-300 (matches the CamPom tier
// chip palette); empties are slate-700. Rendered with `★` rather than SVGs to
// stay cheap on AG Grid's per-row render budget.
function StarRating({ value }: { value: number | null }) {
  if (value == null) return <span className="text-gray-600">—</span>;
  const clamped = Math.max(0, Math.min(5, value));
  return (
    <span className="text-xs tracking-tight">
      <span className="text-amber-300">{'★'.repeat(clamped)}</span>
      <span className="text-gray-700">{'★'.repeat(5 - clamped)}</span>
    </span>
  );
}

// One status pill per commit_status. Vocab pinned by migration 020 comment:
// Signed / Committed / Uncommitted. Defensive default so an unexpected value
// still renders rather than blanking the cell.
function statusChipClass(status: string | null): string {
  switch (status) {
    case 'Signed':
      return 'bg-emerald-900/30 border-emerald-700 text-emerald-300';
    case 'Committed':
      return 'bg-amber-900/30 border-amber-700 text-amber-300';
    case 'Uncommitted':
      return 'bg-gray-800 border-gray-700 text-gray-400';
    default:
      return 'bg-gray-800 border-gray-700 text-gray-500';
  }
}

function teamCellRenderer(opts: {
  name: string | null;
  id: string | null;
  season: number;
  fallback?: string;
  fallbackClass?: string;
}) {
  const { name, id, season, fallback = 'Uncommitted', fallbackClass = 'text-gray-500 italic' } = opts;
  if (!name) return <span className={fallbackClass}>{fallback}</span>;
  if (!id) return <span className="text-gray-200">{name}</span>;
  return (
    <SeasonLink
      to={`/teams/${id}?season=${season}`}
      onClick={(e) => e.stopPropagation()}
      className="text-blue-400 hover:underline"
    >
      {name}
    </SeasonLink>
  );
}

// Row enriched with the model-vs-247 disagreement signal. `campom_rank` is
// the recruit's position when the class is sorted by projected freshman
// CamPom desc; `rank_delta = composite_rank − campom_rank` is the value
// index. Mirrors TransferPortal's sign convention — positive = CamPom
// rates the recruit higher than 247 does. Both fields are NULL when either
// rank input is missing (unranked recruit OR predict failure).
interface RankedRecruit extends RecruitRow {
  campom_rank: number | null;
  rank_delta: number | null;
}

function buildColumns(isMobile: boolean, year: number): ColDef<RankedRecruit>[] {
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  return [
    {
      headerName: 'Rank',
      field: 'campom_rank',
      width: 70,
      pinned: 'left',
      headerTooltip:
        "Our rank among 247-ranked recruits, sorted by projected freshman CamPom. Forward-looking — favors recruits the freshman-impact model expects to be more productive in year one, not just who 247 has ranked highest. '—' for unranked-by-247 recruits or rare projection failures.",
      cellRenderer: (p: { value: number | null }) =>
        p.value != null ? (
          <span className="font-bold">{p.value}</span>
        ) : (
          <span className="text-gray-600">—</span>
        ),
    },
    {
      headerName: 'Player',
      field: 'name',
      width: 200,
      pinned: 'left',
      cellRenderer: (p: { value: string; data?: RecruitRow }) => {
        const id = p.data?.player_id;
        // Most class-of-2026 rows won't have a cstat player_id until their
        // freshman cstat-season (2027) ingests; until then render plain.
        if (!id) return <span className="text-gray-300">{p.value}</span>;
        return (
          <SeasonLink
            to={`/players/${id}?season=${year + 1}`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {p.value}
          </SeasonLink>
        );
      },
    },
    {
      headerName: 'Stars',
      field: 'star_rating',
      ...flexCol(1, 90),
      headerTooltip: '247Sports star rating (1–5)',
      cellRenderer: (p: { value: number | null }) => <StarRating value={p.value} />,
    },
    {
      headerName: 'Pos',
      field: 'position',
      ...flexCol(1, 70),
      cellRenderer: (p: { value: string | null }) =>
        p.value ? (
          <span className="text-gray-300 text-xs">{p.value}</span>
        ) : (
          <span className="text-gray-600 text-xs">—</span>
        ),
    },
    {
      headerName: 'Ht/Wt',
      ...flexCol(1, 90),
      sortable: false,
      valueGetter: (p) => {
        const h = p.data?.height;
        const w = p.data?.weight;
        if (!h && !w) return '';
        return `${h ?? '—'}${w ? ` / ${w}` : ''}`;
      },
      cellRenderer: (p: { value: string }) => (
        <span className="text-gray-400 text-xs">{p.value || '—'}</span>
      ),
    },
    {
      headerName: 'Committed To',
      field: 'committed_school',
      ...flexCol(2, 180),
      cellRenderer: (p: { data?: RecruitRow }) =>
        teamCellRenderer({
          // Prefer the cstat short name (resolved via team_match_score during
          // ingest) when we matched the school; fall back to 247's display
          // name verbatim if no match.
          name: p.data?.committed_school_short ?? p.data?.committed_school ?? null,
          id: p.data?.committed_team_id ?? null,
          // Recruits join their committed school in the *next* cstat-
          // season (class-of-2024 first plays in 2025, class-of-2026 in
          // 2027). The 2027 case lands on the projected team page.
          season: year + 1,
        }),
    },
    {
      headerName: 'Projection',
      field: 'projected_campom_mean',
      ...flexCol(1, 110),
      headerTooltip:
        "cstat's freshman-impact projection (CamPom v3 for the recruit's first college season). Hover a cell to see the q10–q90 band. Wider band = thinner training-set support; tighter band = denser cohort. Selection-bias caveat: elite top-30 projections are calibrated on returners since the highest-rated freshmen leave for the draft.",
      sort: 'desc',
      cellRenderer: (p: { value: number | null; data?: RankedRecruit }) => {
        if (p.value == null) return <span className="text-gray-600 text-xs">—</span>;
        const tier = campomTier(p.value);
        const lo = p.data?.projected_campom_lower;
        const hi = p.data?.projected_campom_upper;
        const bandStr =
          lo != null && hi != null
            ? `Projected freshman CamPom: ${p.value.toFixed(1)} (${lo.toFixed(1)}–${hi.toFixed(1)})${tier ? ` · ${tier}` : ''}`
            : `Projected freshman CamPom: ${p.value.toFixed(1)}${tier ? ` · ${tier}` : ''}`;
        return (
          <span
            className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
            title={bandStr}
          >
            {p.value.toFixed(1)}
          </span>
        );
      },
    },
    {
      headerName: '247',
      field: 'composite_rank',
      ...flexCol(1, 70),
      headerTooltip: '247Sports composite national rank within the recruiting class. — for unranked recruits.',
      comparator: (a: number | null, b: number | null) => {
        if (a == null && b == null) return 0;
        if (a == null) return 1;
        if (b == null) return -1;
        return a - b;
      },
      cellRenderer: (p: { value: number | null }) =>
        p.value != null ? (
          <span className="text-gray-400 text-xs">{p.value}</span>
        ) : (
          <span className="text-gray-600 text-xs">—</span>
        ),
    },
    {
      headerName: 'Δ247',
      field: 'rank_delta',
      ...flexCol(1, 70),
      headerTooltip:
        'Value vs. 247: composite_rank − cstat projected rank. Positive (green) = CamPom rates the recruit higher than 247 does; negative (red) = CamPom is lower. Useful for spotting model-vs-scouts disagreement; also a sanity-check surface — sort desc to find sleepers, asc to find scout-favorites the model is bearish on. NULL when either rank is missing (unranked-by-247 recruit, or projection unavailable).',
      comparator: (a: number | null, b: number | null) => {
        if (a == null && b == null) return 0;
        if (a == null) return 1;
        if (b == null) return -1;
        return a - b;
      },
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-gray-600 text-xs">—</span>;
        const v = p.value;
        const color =
          v > 0 ? 'text-emerald-400' : v < 0 ? 'text-rose-400' : 'text-gray-500';
        const text = v > 0 ? `+${v}` : `${v}`;
        return <span className={`text-xs font-semibold ${color}`}>{text}</span>;
      },
    },
    {
      headerName: 'Hometown',
      ...flexCol(2, 160),
      sortable: false,
      valueGetter: (p) => {
        const c = p.data?.city;
        const s = p.data?.state;
        if (!c && !s) return '';
        return [c, s].filter(Boolean).join(', ');
      },
      cellRenderer: (p: { value: string }) => (
        <span className="text-gray-400 text-xs">{p.value || '—'}</span>
      ),
    },
  ];
}

interface Props {
  year: number;
}

export default function RecruitClass({ year }: Props) {
  const [rows, setRows] = useState<RankedRecruit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [statusFilter, setStatusFilter] = useState<string | null>(null);
  const isMobile = useIsMobile();

  useEffect(() => {
    let canceled = false;
    // No setError/setRows reset at the top — `react-hooks/set-state-in-effect`
    // forbids synchronous state-sets in the effect body. Async resets in the
    // `.then` / `.catch` callbacks are fine; clearing `error` there is what
    // lets a successful year-change recover from a prior error.
    fetchRecruits(year)
      .then((r) => {
        if (canceled) return;
        setError(null);
        // Rank by projected freshman CamPom desc. Mirrors the TransferPortal
        // pattern: `campom_rank` is only assigned when BOTH inputs are
        // present (projected CamPom + 247 composite_rank), so the Δ column
        // is NULL for unranked-by-247 recruits and for the rare predict
        // failure — frontend column already renders that as an em-dash.
        // Predict-rank counter `i` is incremented only on eligible rows
        // so on-screen position matches the displayed rank.
        const sorted = [...r.recruits].sort((a, b) => {
          if (a.projected_campom_mean == null && b.projected_campom_mean == null) return 0;
          if (a.projected_campom_mean == null) return 1;
          if (b.projected_campom_mean == null) return -1;
          return b.projected_campom_mean - a.projected_campom_mean;
        });
        let i = 0;
        const ranked: RankedRecruit[] = sorted.map((rec) => {
          const campom_rank =
            rec.projected_campom_mean != null && rec.composite_rank != null
              ? ++i
              : null;
          return {
            ...rec,
            campom_rank,
            rank_delta:
              campom_rank != null ? rec.composite_rank! - campom_rank : null,
          };
        });
        setRows(ranked);
      })
      .catch((e) => {
        if (!canceled) setError(String(e));
      });
    return () => {
      canceled = true;
    };
  }, [year]);

  const columns = useMemo(() => buildColumns(isMobile, year), [isMobile, year]);

  const filtered = useMemo(() => {
    if (!rows) return null;
    const q = search.trim().toLowerCase();
    return rows.filter((r) => {
      if (statusFilter && r.commit_status !== statusFilter) return false;
      if (!q) return true;
      return (
        r.name.toLowerCase().includes(q) ||
        (r.committed_school ?? '').toLowerCase().includes(q) ||
        (r.committed_school_short ?? '').toLowerCase().includes(q) ||
        (r.high_school ?? '').toLowerCase().includes(q) ||
        (r.city ?? '').toLowerCase().includes(q) ||
        (r.state ?? '').toLowerCase().includes(q)
      );
    });
  }, [rows, search, statusFilter]);

  const counts = useMemo(() => {
    if (!rows) return null;
    const by: Record<string, number> = {};
    for (const r of rows) {
      const k = r.commit_status ?? 'Unknown';
      by[k] = (by[k] ?? 0) + 1;
    }
    return by;
  }, [rows]);

  if (error) {
    // The route's 404 path returns `{ "error": "no recruits data for year N" }`,
    // which `fetchJson` surfaces verbatim — no HTTP status in the message — so
    // we anchor the empty-state check on the route's own wording.
    if (error.includes('no recruits')) {
      return (
        <div className="p-4 text-gray-500 text-sm">
          No recruits ingested for class of {year}. Run{' '}
          <code className="px-1 bg-gray-800 rounded">
            cstat-ingest recruits --year {year}
          </code>{' '}
          to populate.
        </div>
      );
    }
    return (
      <div className="p-4 text-rose-300">Failed to load recruits: {error}</div>
    );
  }

  const total = rows?.length ?? 0;
  const shown = filtered?.length ?? 0;

  // Status filter chip set. Clicking the active chip clears the filter.
  const statusChip = (label: string) => {
    const active = statusFilter === label;
    const n = counts?.[label] ?? 0;
    return (
      <button
        key={label}
        onClick={() => setStatusFilter(active ? null : label)}
        aria-pressed={active}
        className={`px-2 py-0.5 rounded border text-xs transition-colors ${
          active
            ? statusChipClass(label) + ' ring-1 ring-current'
            : 'bg-gray-900 border-gray-700 text-gray-400 hover:border-gray-500'
        }`}
        title={active ? 'Clear filter' : `Filter to ${label}`}
      >
        {label} <span className="opacity-70">{n}</span>
      </button>
    );
  };

  return (
    <div>
      <div className="flex items-center gap-3 mb-3 flex-wrap">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search recruits / schools / states…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-64"
        />
        <div className="flex items-center gap-1">
          {statusChip('Signed')}
          {statusChip('Committed')}
          {statusChip('Uncommitted')}
        </div>
        <span className="text-xs text-gray-500">
          {shown === total ? `${total} recruits` : `${shown} of ${total} recruits`}
          {' · '}
          <a
            href={`https://247sports.com/Season/${year}-Basketball/CompositeRecruitRankings/?InstitutionGroup=HighSchool`}
            target="_blank"
            rel="noopener noreferrer"
            className="text-blue-400 hover:underline"
          >
            247Sports source
          </a>
        </span>
      </div>
      <div style={{ height: 'calc(100dvh - 220px)', minHeight: '400px', width: '100%' }}>
        <AgGridReact<RankedRecruit>
          theme={gridTheme}
          columnDefs={columns}
          rowData={filtered ?? []}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
          // Size each column to its widest cell + header so nothing
          // truncates; AG Grid surfaces a horizontal scrollbar
          // automatically when the total width exceeds the container.
          autoSizeStrategy={{ type: 'fitCellContents' }}
        />
      </div>
    </div>
  );
}
