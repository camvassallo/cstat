import { useEffect, useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchTransfers, type TransferRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor } from './campom';
import { pctileTextColor } from './pctile';

// Players ranked by 247Sports who carry one of our derived ranks (we have a
// matching cstat player with a CamPom value).
type RankedTransfer = TransferRow & { rank_cstat: number | null };

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

const teamMoveRenderer = (p: { data?: RankedTransfer }) => {
  const prev = p.data?.previous_team;
  const next = p.data?.next_team;
  return (
    <span className="inline-flex items-center gap-1.5 text-sm">
      <span className={prev ? 'text-gray-200' : 'text-gray-500'}>
        {prev ?? '—'}
      </span>
      <span className="text-gray-500">→</span>
      <span className={next ? 'text-blue-300' : 'text-gray-500 italic'}>
        {next ?? 'TBD'}
      </span>
    </span>
  );
};

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
      headerName: '247',
      field: 'rank_247',
      width: 90,
      headerTooltip:
        '247Sports rank. Subscript = (247 rank − our rank): + green when we rank the player higher than 247 does, − red when lower.',
      cellRenderer: (p: { value: number; data?: RankedTransfer }) => {
        const ours = p.data?.rank_cstat;
        // Delta only makes sense once we have an our-rank to compare against.
        // Positive = CamPom rates the player better (lower rank number) than
        // 247 does, so it gets the green "+N".
        const delta = ours != null ? p.value - ours : null;
        const deltaColor =
          delta == null
            ? ''
            : delta > 0
              ? 'text-emerald-400'
              : delta < 0
                ? 'text-rose-400'
                : 'text-gray-500';
        const deltaText =
          delta == null
            ? null
            : delta > 0
              ? `+${delta}`
              : delta < 0
                ? `${delta}`
                : '0';
        return (
          <span className="inline-flex items-baseline gap-0.5">
            <span className="text-gray-400 text-xs">{p.value}</span>
            {deltaText && (
              <sub className={`text-[9px] font-semibold ${deltaColor}`}>
                {deltaText}
              </sub>
            )}
          </span>
        );
      },
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
      headerName: 'Pos',
      field: 'position',
      width: 70,
      sortable: false,
    },
    {
      headerName: 'Ht/Wt',
      width: 90,
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
      headerName: 'Previous → Next',
      colId: 'team_move',
      width: 320,
      sortable: false,
      cellRenderer: teamMoveRenderer,
    },
    {
      headerName: 'Status',
      field: 'status',
      width: 120,
      sortable: false,
      cellRenderer: (p: { value: string }) => {
        const v = p.value || 'Open';
        const color =
          v === 'Committed'
            ? 'text-emerald-300'
            : v === 'Withdrew'
              ? 'text-rose-300'
              : 'text-gray-400';
        return <span className={`text-xs ${color}`}>{v}</span>;
      },
    },
    {
      headerName: 'CamPom',
      field: 'campom',
      width: 110,
      sort: 'desc',
      headerTooltip: 'Our composite player valuation from prior season',
      cellRenderer: campomRenderer,
      valueFormatter: (p) => fmtCampom(p.value),
    },
    {
      headerName: 'MPG',
      field: 'minutes_per_game',
      width: 70,
      headerTooltip: 'Minutes per game in prior season',
      valueFormatter: (p) => (p.value != null ? p.value.toFixed(1) : '—'),
      cellStyle: (p) => ({
        color: pctileTextColor(null),
        opacity: p.value == null ? 0.4 : 1,
      }),
    },
  ];
}

interface Props {
  year: number;
}

export default function TransferPortal({ year }: Props) {
  const navigate = useNavigate();
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
        const withRank: RankedTransfer[] = ranked.map((t) => ({
          ...t,
          rank_cstat: t.campom != null ? ++i : null,
        }));
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
    const q = search.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      (t) =>
        t.name.toLowerCase().includes(q) ||
        (t.previous_team ?? '').toLowerCase().includes(q) ||
        (t.next_team ?? '').toLowerCase().includes(q),
    );
  }, [rows, search]);

  if (error) {
    return (
      <div className="p-4 text-rose-300">Failed to load transfers: {error}</div>
    );
  }

  const matched = rows?.filter((r) => r.player_id != null).length ?? 0;
  const total = rows?.length ?? 0;

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
          {total} transfers · {matched} matched to cstat ·{' '}
          <a
            href="https://247sports.com/season/2026-basketball/transferportaltop/"
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
            if (id) navigate(`/players/${id}`);
          }}
        />
      </div>
    </div>
  );
}
