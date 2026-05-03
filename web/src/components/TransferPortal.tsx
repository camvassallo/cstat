import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchTransfers, type TransferRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor } from './campom';
import { classColor } from './archetypeColors';
import { SeasonLink } from './SeasonLink';
import { seasonHref, useSeason } from './season';

// Players ranked by 247Sports who carry one of our derived ranks (we have a
// matching cstat player with a CamPom value). `rank_delta` is `rank_247 −
// rank_cstat`: positive means CamPom values the player higher than 247 does
// (best value), negative the opposite. Null when we couldn't rank the player.
type RankedTransfer = TransferRow & {
  rank_cstat: number | null;
  rank_delta: number | null;
};

const fmtCampom = (v: number | null) => (v != null ? v.toFixed(1) : '—');

const campomRenderer = (p: { value: number | null; data?: RankedTransfer }) => {
  if (p.value == null) return <span className="text-slate-500">—</span>;
  const tier = campomTier(p.value);
  const pct = p.data?.campom_pct;
  const pctStr = pct != null ? Math.round(pct * 100) : null;
  return (
    <span className="inline-flex items-baseline gap-2">
      <span
        className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
        title={tier ?? ''}
      >
        {p.value.toFixed(1)}
      </span>
      {pctStr != null && (
        <span className="text-slate-400 text-xs">{pctStr}</span>
      )}
    </span>
  );
};

// Renders a team cell as a link to /teams/:id when we resolved the 247 short
// name to a cstat team_id, or as plain text when we didn't (rare; small
// schools we don't carry, or "TBD" for an uncommitted next destination).
function teamCellRenderer(opts: {
  name: string | null;
  id: string | null;
  fallback?: string;
  fallbackClass?: string;
}) {
  const { name, id, fallback = '—', fallbackClass = 'text-gray-500' } = opts;
  if (!name) return <span className={fallbackClass}>{fallback}</span>;
  if (!id) return <span className="text-gray-200">{name}</span>;
  return (
    <SeasonLink
      to={`/teams/${id}`}
      onClick={(e) => e.stopPropagation()}
      className="text-blue-400 hover:underline"
    >
      {name}
    </SeasonLink>
  );
}

function buildColumns(): ColDef<RankedTransfer>[] {
  return [
    {
      headerName: 'Rank',
      field: 'rank_cstat',
      width: 70,
      pinned: 'left',
      headerTooltip: 'Our rank by CamPom; players with no CamPom value are unranked',
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
      cellRenderer: (p: { value: string; data?: RankedTransfer }) => {
        const id = p.data?.player_id;
        if (!id) {
          return (
            <span className="text-gray-300" title="No cstat match">
              {p.value}
            </span>
          );
        }
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
      headerName: 'Class',
      colId: 'archetype',
      // Mirrors the Players page column so users see the same primary /
      // secondary archetype combo for each transfer.
      flex: 2,
      minWidth: 150,
      sortable: false,
      cellRenderer: (p: { data?: RankedTransfer }) => {
        const cls = p.data?.primary_class;
        if (!cls) return <span className="text-gray-600 text-xs">—</span>;
        const sec = p.data?.secondary_class;
        return (
          <span
            className="text-[10px] font-bold uppercase tracking-wide whitespace-nowrap"
            style={{ color: classColor(cls) }}
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
    {
      headerName: 'Ht/Wt',
      flex: 1,
      minWidth: 80,
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
      headerName: 'Previous',
      field: 'previous_team',
      flex: 2,
      minWidth: 150,
      cellRenderer: (p: { data?: RankedTransfer }) =>
        teamCellRenderer({
          // Prefer the cstat Torvik short name ("Kansas") when we matched it;
          // fall back to the 247 short name verbatim if no match.
          name: p.data?.previous_team_full ?? p.data?.previous_team ?? null,
          id: p.data?.previous_team_id ?? null,
        }),
    },
    {
      headerName: 'Next',
      field: 'next_team',
      flex: 2,
      minWidth: 150,
      cellRenderer: (p: { data?: RankedTransfer }) =>
        teamCellRenderer({
          name: p.data?.next_team ?? null,
          id: p.data?.next_team_id ?? null,
          fallback: 'TBD',
          fallbackClass: 'text-gray-500 italic',
        }),
    },
    {
      headerName: 'CamPom',
      field: 'campom',
      flex: 1,
      minWidth: 100,
      sort: 'desc',
      headerTooltip: 'Our composite player valuation from prior season',
      cellRenderer: campomRenderer,
      valueFormatter: (p) => fmtCampom(p.value),
    },
    {
      headerName: '247',
      field: 'rank_247',
      flex: 1,
      minWidth: 60,
      headerTooltip: '247Sports rank',
      cellRenderer: (p: { value: number }) => (
        <span className="text-gray-400 text-xs">{p.value}</span>
      ),
    },
    {
      headerName: 'Δ',
      field: 'rank_delta',
      flex: 1,
      minWidth: 70,
      headerTooltip:
        'Value vs. 247: 247 rank − our rank. Positive (green) means CamPom rates the player higher than 247 does — sort desc to find best values. Negative (red) means CamPom is lower on the player.',
      comparator: (a: number | null, b: number | null) => {
        // Push unranked rows to the bottom regardless of sort direction.
        if (a == null && b == null) return 0;
        if (a == null) return 1;
        if (b == null) return -1;
        return a - b;
      },
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-gray-600">—</span>;
        const v = p.value;
        const color =
          v > 0
            ? 'text-emerald-400'
            : v < 0
              ? 'text-rose-400'
              : 'text-gray-500';
        const text = v > 0 ? `+${v}` : `${v}`;
        return (
          <span className={`text-xs font-semibold ${color}`}>{text}</span>
        );
      },
    },
  ];
}

interface Props {
  year: number;
}

export default function TransferPortal({ year }: Props) {
  const navigate = useNavigate();
  const { season } = useSeason();
  const [rows, setRows] = useState<RankedTransfer[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');

  useEffect(() => {
    let canceled = false;
    fetchTransfers(year)
      .then((r) => {
        if (canceled) return;
        // Assign our rank: sort by CamPom desc; null CamPom values stay
        // unranked (rank_cstat = null) and fall to the bottom by default.
        const ranked = [...r.transfers].sort((a, b) => {
          if (a.campom == null && b.campom == null) return 0;
          if (a.campom == null) return 1;
          if (b.campom == null) return -1;
          return b.campom - a.campom;
        });
        let i = 0;
        const withRank: RankedTransfer[] = ranked.map((t) => {
          const rank_cstat = t.campom != null ? ++i : null;
          return {
            ...t,
            rank_cstat,
            rank_delta: rank_cstat != null ? t.rank_247 - rank_cstat : null,
          };
        });
        setRows(withRank);
      })
      .catch((e) => {
        if (!canceled) setError(String(e));
      });
    return () => {
      canceled = true;
    };
  }, [year]);

  const columns = useMemo(() => buildColumns(), []);

  const filtered = useMemo(() => {
    if (!rows) return null;
    // Hide unranked rows (no CamPom). Players without prior-season cstat
    // data don't carry a comparable rank, so they'd just clutter the bottom.
    const ranked = rows.filter((t) => t.rank_cstat != null);
    const q = search.trim().toLowerCase();
    if (!q) return ranked;
    // Also match the resolved full team name (e.g. searching "Jayhawks"
    // should find a player whose previous_team is "Kansas") since that's
    // what we render in the cell.
    return ranked.filter(
      (t) =>
        t.name.toLowerCase().includes(q) ||
        (t.previous_team ?? '').toLowerCase().includes(q) ||
        (t.previous_team_full ?? '').toLowerCase().includes(q) ||
        (t.next_team ?? '').toLowerCase().includes(q),
    );
  }, [rows, search]);

  if (error) {
    return (
      <div className="p-4 text-rose-300">Failed to load transfers: {error}</div>
    );
  }

  const ranked = rows?.filter((r) => r.rank_cstat != null).length ?? 0;
  const total = rows?.length ?? 0;
  const hidden = total - ranked;

  return (
    <div>
      <div className="flex items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search transfers / teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-64"
        />
        <span className="text-xs text-gray-500">
          {ranked} ranked transfers
          {hidden > 0 && ` · ${hidden} hidden (no CamPom)`} ·{' '}
          <a
            href={`https://247sports.com/season/${year}-basketball/transferportaltop/`}
            target="_blank"
            rel="noopener noreferrer"
            className="text-blue-400 hover:underline"
          >
            247Sports source
          </a>
        </span>
      </div>
      <div style={{ height: 'calc(100vh - 220px)', width: '100%' }}>
        <AgGridReact<RankedTransfer>
          theme={gridTheme}
          columnDefs={columns}
          rowData={filtered ?? []}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
          onRowClicked={(e) => {
            const target = e.event?.target as HTMLElement | undefined;
            if (target?.closest('a')) return;
            const id = e.data?.player_id;
            if (id) navigate(seasonHref(`/players/${id}`, season));
          }}
        />
      </div>
    </div>
  );
}
