import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchDraft, type DraftProspect } from '../api/client';
import { gridTheme } from '../theme';
import { camTier, camTierColor, camHalfColor } from './cam';
import { classColor, provisionalMeta } from './archetypeColors';
import { SeasonLink } from './SeasonLink';
import { useIsMobile } from './useIsMobile';

// A prospect decorated with the CAM-derived rank and the headline Δ.
// `rank_cstat` is the row's position among draft-ranked prospects (those with
// a draft pick number) sorted by CAM desc. `rank_delta = draft_rank −
// rank_cstat`: positive means CAM rates the player higher than the draft
// order does — a sleeper. Both are null when the prospect has no CAM value
// or no draft rank (the unranked tail), so Δ can't be computed.
type RankedProspect = DraftProspect & {
  rank_cstat: number | null;
  rank_delta: number | null;
};

// Stable identity for the CAM-rank lookup. Name alone can collide across
// schools; pairing it with the board team keeps each row unique.
const rowKey = (p: DraftProspect) => `${p.name}|${p.current_team}`;

// Nulls-last numeric comparator. AG Grid inverts a comparator's result for
// descending sort, so we read `isDescending` and pre-invert the null verdict —
// that keeps unranked / unmatched rows (no draft rank, no CAM) pinned to
// the visual bottom in BOTH sort directions.
const nullsLast = (
  a: number | null,
  b: number | null,
  _nodeA: unknown,
  _nodeB: unknown,
  isDescending: boolean,
) => {
  if (a == null && b == null) return 0;
  if (a == null) return isDescending ? -1 : 1;
  if (b == null) return isDescending ? 1 : -1;
  return a - b;
};

// O/D CAM halves — signed values on the shared diverging red→green
// gradient (per-half saturation, see camHalfColor), gated server-side
// (±30 sanity envelope; unstable rows arrive null and render "—").
const campomHalfRenderer = (side: 'o' | 'd') =>
  function CampomHalfCell(p: { value: number | null }) {
    return p.value != null ? (
      <span className="tabular-nums text-xs" style={{ color: camHalfColor(p.value, side) }}>
        {`${p.value > 0 ? '+' : ''}${p.value.toFixed(1)}`}
      </span>
    ) : (
      <span className="text-gray-600 text-xs">—</span>
    );
  };

function buildColumns(isMobile: boolean): ColDef<RankedProspect>[] {
  // Mobile: fixed natural widths so AG Grid horizontal-scrolls instead of
  // compressing content. Desktop: flex distribution. Mirrors TransferPortal.
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  return [
    {
      headerName: '#',
      field: 'draft_rank',
      // Wide enough for a 2-digit pick plus the always-visible sort arrow so
      // "60" doesn't truncate.
      width: 72,
      pinned: 'left',
      // Default sort: draft order (ascending pick number). — for the unranked
      // tail sorts last in both directions via nullsLast.
      sort: 'asc',
      headerTooltip: 'Draft pick number (draft order). — for the unranked tail.',
      comparator: nullsLast,
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
      width: isMobile ? 126 : 148,
      pinned: 'left',
      cellRenderer: (p: { value: string; data?: RankedProspect }) => {
        const id = p.data?.player_id;
        if (!id) {
          return (
            <span className="text-gray-300" title="No matching college season (international, G League, or unmatched)">
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
      headerName: 'Team',
      field: 'current_team',
      ...flexCol(2, 140),
      cellRenderer: (p: { data?: RankedProspect }) => {
        // Prefer the resolved cstat short name; fall back to the board's
        // school string verbatim when we couldn't match a cstat team.
        const name = p.data?.team_name ?? p.data?.current_team ?? null;
        const id = p.data?.team_id ?? null;
        if (!name) return <span className="text-gray-500">—</span>;
        if (!id) return <span className="text-gray-300">{name}</span>;
        return (
          <SeasonLink
            to={`/teams/${id}`}
            onClick={(e) => e.stopPropagation()}
            className="text-blue-400 hover:underline"
          >
            {name}
          </SeasonLink>
        );
      },
    },
    {
      headerName: 'Archetype',
      colId: 'archetype',
      // Primary / secondary combo in one column, mirroring the Players and
      // Transfer Portal pages. — for picks with no cstat college row
      // (internationals, G-Leaguers, or an unmatched name).
      ...flexCol(2, 150),
      sortable: false,
      headerTooltip:
        "The matched player's D&D-class archetype (primary / secondary) for their just-completed college season. — for picks with no college season on file.",
      cellRenderer: (p: { data?: RankedProspect }) => {
        const cls = p.data?.primary_archetype;
        if (!cls) return <span className="text-gray-600 text-xs">—</span>;
        const sec = p.data?.secondary_archetype;
        const prov = provisionalMeta(p.data);
        return (
          <span
            className={`text-xs font-bold uppercase tracking-wide whitespace-nowrap ${
              prov.provisional ? 'opacity-70' : ''
            }`}
            style={{ color: classColor(cls) }}
            title={(sec ? `${cls} / ${sec}` : cls) + (prov.note ? ` · ${prov.note}` : '')}
          >
            {cls}
            {sec && (
              <span className="ml-1 opacity-70" style={{ color: classColor(sec) }}>
                / {sec}
              </span>
            )}
            {prov.shortYear && (
              <span className="ml-1 text-gray-500 lowercase font-normal tracking-normal">
                {prov.shortYear}
              </span>
            )}
          </span>
        );
      },
    },
    {
      headerName: 'CAM',
      field: 'campom',
      ...flexCol(1, 100),
      headerTooltip:
        "CAM player value for this prospect's just-completed college season. — for prospects with no college row (internationals, G-Leaguers).",
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-gray-600 text-xs">—</span>;
        const tier = camTier(p.value);
        return (
          <span
            className={`px-1.5 rounded border text-xs ${camTierColor(tier)}`}
            title={tier ?? undefined}
          >
            {p.value.toFixed(1)}
          </span>
        );
      },
    },
    {
      headerName: 'CAMO',
      field: 'campom_o',
      ...flexCol(1, 80),
      headerTooltip:
        "CAM's offensive half (O + D = CAM, same per-100 scale). — for unmatched prospects or where the decomposition is numerically unstable (±30 sanity envelope).",
      comparator: nullsLast,
      cellRenderer: campomHalfRenderer('o'),
    },
    {
      headerName: 'CAMD',
      field: 'campom_d',
      ...flexCol(1, 80),
      headerTooltip:
        "CAM's defensive half — positive is GOOD (defensive value added; O + D = CAM). — for unmatched prospects or where the decomposition is numerically unstable.",
      comparator: nullsLast,
      cellRenderer: campomHalfRenderer('d'),
    },
    {
      headerName: 'Δ',
      field: 'rank_delta',
      ...flexCol(1, 80),
      headerTooltip:
        "Value vs. the draft order: draft rank − CAM rank (CAM rank = position among draft-ranked prospects sorted by CAM). Positive (green) means CAM rates the player higher than scouts do — a sleeper. Negative (red) means scouts are higher on them than CAM. — when the prospect has no CAM value or no draft rank.",
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null; data?: RankedProspect }) => {
        if (p.value == null) return <span className="text-gray-600">—</span>;
        const v = p.value;
        const color =
          v > 0 ? 'text-emerald-400' : v < 0 ? 'text-rose-400' : 'text-gray-500';
        const text = v > 0 ? `+${v}` : `${v}`;
        const rc = p.data?.rank_cstat;
        return (
          <span
            className={`text-xs font-semibold ${color}`}
            title={rc != null ? `CAM rank ${rc} vs draft rank ${p.data?.draft_rank}` : undefined}
          >
            {text}
          </span>
        );
      },
    },
  ];
}

// The NBA Draft big board for a single draft-cycle year — a mode tab on the
// Players page (mirrors TransferPortal / RecruitClass). `year` is the
// site-selected season, plumbed from the Players page.
export default function DraftBoard({ year }: { year: number }) {
  const [rows, setRows] = useState<DraftProspect[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const isMobile = useIsMobile();

  // No synchronous state reset here — the codebase forbids setState in an
  // effect body. On a season change the previous board lingers until the new
  // fetch resolves (mild stale-flicker, matches the Rankings/Players pattern).
  useEffect(() => {
    let canceled = false;
    fetchDraft(year)
      .then((r) => {
        if (canceled) return;
        setRows(r.prospects);
        setError(null);
      })
      .catch((e) => {
        if (!canceled) setError(String(e));
      });
    return () => {
      canceled = true;
    };
  }, [year]);

  // Decorate each prospect with its CAM rank and the headline Δ. The
  // CAM rank only ranks prospects that have BOTH a draft rank and a CAM
  // value — the same cohort the draft rank covers — so Δ stays a like-for-like
  // comparison. Display order is left to AG Grid (default sort = draft order).
  const ranked = useMemo<RankedProspect[]>(() => {
    if (!rows) return [];
    const rankable = rows
      .filter((p) => p.campom != null && p.draft_rank != null)
      .sort((a, b) => b.campom! - a.campom!);
    const cstatRank = new Map<string, number>();
    rankable.forEach((p, i) => cstatRank.set(rowKey(p), i + 1));
    return rows.map((p) => {
      const rc = cstatRank.get(rowKey(p)) ?? null;
      return {
        ...p,
        rank_cstat: rc,
        rank_delta: rc != null && p.draft_rank != null ? p.draft_rank - rc : null,
      };
    });
  }, [rows]);

  const columns = useMemo(() => buildColumns(isMobile), [isMobile]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return ranked;
    return ranked.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.current_team.toLowerCase().includes(q) ||
        (p.team_name ?? '').toLowerCase().includes(q),
    );
  }, [ranked, search]);

  if (error) {
    return (
      <div className="mt-1 p-4 rounded bg-gray-800 text-gray-300">
        No draft board available for {year}.
        <div className="text-xs text-gray-500 mt-1">{error}</div>
      </div>
    );
  }

  const matched = rows?.filter((p) => p.campom != null).length ?? 0;
  const total = rows?.length ?? 0;

  return (
    <div>
      <p className="text-sm text-gray-400 mb-3">
        {year} draft, in pick order — joined to each player's CAM value and archetype.
      </p>
      <div className="flex flex-wrap items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search prospects / teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-full sm:w-64"
        />
        <span className="text-xs text-gray-500">
          {total} picks · {matched} with CAM
        </span>
      </div>
      <div style={{ height: 'calc(100dvh - 250px)', minHeight: '400px', width: '100%' }}>
        <AgGridReact<RankedProspect>
          theme={gridTheme}
          columnDefs={columns}
          rowData={filtered}
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
