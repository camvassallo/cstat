import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchRecruits, type RecruitRow } from '../api/client';
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

function buildColumns(isMobile: boolean, year: number): ColDef<RecruitRow>[] {
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  return [
    {
      headerName: 'Rank',
      field: 'composite_rank',
      width: 70,
      pinned: 'left',
      sort: 'asc',
      headerTooltip: "247Sports composite national rank within the recruiting class",
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
      headerName: 'Hometown',
      ...flexCol(2, 140),
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
    {
      headerName: 'Committed To',
      field: 'committed_school',
      ...flexCol(2, 160),
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
      headerName: 'Status',
      field: 'commit_status',
      ...flexCol(1, 110),
      cellRenderer: (p: { value: string | null }) => (
        <span
          className={`inline-block px-2 py-0.5 rounded border text-xs ${statusChipClass(p.value)}`}
        >
          {p.value ?? '—'}
        </span>
      ),
    },
    {
      headerName: 'Rating',
      field: 'composite_rating',
      ...flexCol(1, 90),
      headerTooltip: '247Sports composite rating (0.0000–1.0000)',
      valueFormatter: (p: { value: number | null }) =>
        p.value != null ? p.value.toFixed(4) : '—',
      cellRenderer: (p: { value: number | null }) =>
        p.value != null ? (
          <span className="text-gray-300 text-xs tabular-nums">
            {p.value.toFixed(4)}
          </span>
        ) : (
          <span className="text-gray-600 text-xs">—</span>
        ),
    },
  ];
}

interface Props {
  year: number;
}

export default function RecruitClass({ year }: Props) {
  const [rows, setRows] = useState<RecruitRow[] | null>(null);
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
        setRows(r.recruits);
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
        <AgGridReact<RecruitRow>
          theme={gridTheme}
          columnDefs={columns}
          rowData={filtered ?? []}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
        />
      </div>
    </div>
  );
}
