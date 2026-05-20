import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchDraft, type DraftProspect } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor } from '../components/campom';
import { SeasonLink } from '../components/SeasonLink';
import { useIsMobile } from '../components/useIsMobile';
import { useSeason } from '../components/season';

// A prospect decorated with the CamPom-derived rank and the headline Δ.
// `rank_cstat` is the row's position among draft-ranked prospects (those with
// a Tankathon number) sorted by CamPom desc. `rank_delta = draft_rank −
// rank_cstat`: positive means CamPom rates the player higher than the draft
// board does — a sleeper. Both are null when the prospect has no CamPom value
// or no draft rank (the unranked tail), so Δ can't be computed.
type RankedProspect = DraftProspect & {
  rank_cstat: number | null;
  rank_delta: number | null;
};

// Stable identity for the CamPom-rank lookup. Name alone can collide across
// schools; pairing it with the board team keeps each of the ~116 rows unique.
const rowKey = (p: DraftProspect) => `${p.name}|${p.current_team}`;

// Tier chip — Tankathon's bucket maps onto NBA draft-round structure. Ordered
// best (lottery) to worst (unranked); reuses the CamPom chip palette so the
// page reads consistently with the rest of the site.
function tierChipClass(tier: string): string {
  switch (tier) {
    case 'lottery':
      return 'bg-emerald-500/20 text-emerald-300 border-emerald-500/40';
    case '1st-round':
      return 'bg-sky-500/20 text-sky-300 border-sky-500/40';
    case '2nd-round':
      return 'bg-blue-500/20 text-blue-300 border-blue-500/40';
    case 'fringe':
      return 'bg-amber-500/20 text-amber-300 border-amber-500/40';
    default: // unranked
      return 'bg-slate-700/40 text-slate-400 border-slate-600/40';
  }
}

function tierLabel(tier: string): string {
  switch (tier) {
    case 'lottery':
      return 'Lottery';
    case '1st-round':
      return '1st Rd';
    case '2nd-round':
      return '2nd Rd';
    case 'fringe':
      return 'Fringe';
    default:
      return 'Unranked';
  }
}

// Tier quality order for sorting. The tier is a categorical string, so a plain
// sort would land alphabetically (1st-round, 2nd-round, fringe, lottery, …)
// instead of best-to-worst — this maps each tier to its rank so the Tier
// column sorts lottery → unranked. Unknown values sort last.
const TIER_ORDER: Record<string, number> = {
  lottery: 0,
  '1st-round': 1,
  '2nd-round': 2,
  fringe: 3,
  unranked: 4,
};
const tierComparator = (a: string, b: string) =>
  (TIER_ORDER[a] ?? 99) - (TIER_ORDER[b] ?? 99);

// Status chip — derived eligibility state from the API (early-entrant
// cross-reference + class year).
function statusChip(status: string): { label: string; cls: string; title: string } {
  switch (status) {
    case 'declared':
      return {
        label: 'Declared',
        cls: 'bg-amber-500/20 text-amber-300 border-amber-500/40',
        title:
          'Underclassman who has formally declared for the draft — the withdrawal deadline is still ahead, so this is not yet final.',
      };
    case 'senior':
      return {
        label: 'Senior',
        cls: 'bg-slate-600/30 text-slate-300 border-slate-500/40',
        title: 'Senior — automatically draft-eligible, no early-entry declaration needed.',
      };
    case 'international':
      return {
        label: 'Intl',
        cls: 'bg-violet-500/20 text-violet-300 border-violet-500/40',
        title: 'International prospect — not in the college dataset, so no CamPom value.',
      };
    case 'g-league':
      return {
        label: 'G League',
        cls: 'bg-violet-500/20 text-violet-300 border-violet-500/40',
        title: 'G League prospect — not in the college dataset, so no CamPom value.',
      };
    default: // prospect
      return {
        label: 'Watch',
        cls: 'bg-blue-500/20 text-blue-300 border-blue-500/40',
        title:
          'On the board but has not declared — an underclassman with remaining eligibility that scouts are tracking.',
      };
  }
}

// Nulls-last numeric comparator. AG Grid inverts a comparator's result for
// descending sort, so we read `isDescending` and pre-invert the null verdict —
// that keeps unranked / unmatched rows (no draft rank, no CamPom) pinned to
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

function buildColumns(isMobile: boolean): ColDef<RankedProspect>[] {
  // Mobile: fixed natural widths so AG Grid horizontal-scrolls instead of
  // compressing content. Desktop: flex distribution. Mirrors TransferPortal.
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  return [
    {
      headerName: '#',
      field: 'draft_rank',
      width: 64,
      pinned: 'left',
      headerTooltip: "Tankathon's draft-board rank. — for the unranked tail.",
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
      width: isMobile ? 170 : 200,
      pinned: 'left',
      cellRenderer: (p: { value: string; data?: RankedProspect }) => {
        const id = p.data?.player_id;
        if (!id) {
          return (
            <span className="text-gray-300" title="No cstat match (international, G League, or unmatched)">
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
      headerName: 'Pos',
      field: 'position',
      ...flexCol(1, 70),
      sortable: false,
      cellRenderer: (p: { value: string | null }) => (
        <span className="text-gray-400 text-xs">{p.value || '—'}</span>
      ),
    },
    {
      headerName: 'Class',
      field: 'class_year',
      ...flexCol(1, 100),
      sortable: false,
      cellRenderer: (p: { value: string | null }) => (
        <span className="text-gray-400 text-xs">{p.value || '—'}</span>
      ),
    },
    {
      headerName: 'Tier',
      field: 'tier',
      ...flexCol(1, 100),
      headerTooltip:
        'Tankathon tier mapped to NBA draft-round structure: Lottery (1–14), 1st Rd (15–30), 2nd Rd (31–60), Fringe (61+), Unranked. Sorts best-to-worst.',
      comparator: tierComparator,
      cellRenderer: (p: { value: string }) => (
        <span
          className={`px-1.5 py-0.5 rounded border text-[11px] font-semibold ${tierChipClass(p.value)}`}
        >
          {tierLabel(p.value)}
        </span>
      ),
    },
    {
      headerName: 'CamPom',
      field: 'campom',
      ...flexCol(1, 100),
      sort: 'desc',
      headerTooltip:
        "cstat's CamPom v3 player value for this prospect's just-completed college season. — for prospects with no college row (internationals, G-Leaguers).",
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-gray-600 text-xs">—</span>;
        const tier = campomTier(p.value);
        return (
          <span
            className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
            title={tier ?? undefined}
          >
            {p.value.toFixed(1)}
          </span>
        );
      },
    },
    {
      headerName: 'Δ',
      field: 'rank_delta',
      ...flexCol(1, 80),
      headerTooltip:
        "Value vs. the draft board: draft rank − CamPom rank (CamPom rank = position among draft-ranked prospects sorted by CamPom). Positive (green) means CamPom rates the player higher than scouts do — a sleeper. Negative (red) means scouts are higher on them than CamPom. — when the prospect has no CamPom value or no draft rank.",
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
            title={rc != null ? `CamPom rank ${rc} vs draft rank ${p.data?.draft_rank}` : undefined}
          >
            {text}
          </span>
        );
      },
    },
    {
      headerName: 'Status',
      field: 'status',
      ...flexCol(1, 100),
      cellRenderer: (p: { value: string }) => {
        const chip = statusChip(p.value);
        return (
          <span
            className={`px-1.5 py-0.5 rounded border text-[11px] font-semibold ${chip.cls}`}
            title={chip.title}
          >
            {chip.label}
          </span>
        );
      },
    },
  ];
}

export default function Draft() {
  const { season } = useSeason();
  const [rows, setRows] = useState<DraftProspect[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const isMobile = useIsMobile();

  // No synchronous state reset here — the codebase forbids setState in an
  // effect body. On a season change the previous board lingers until the new
  // fetch resolves (mild stale-flicker, matches the Rankings/Players pattern).
  useEffect(() => {
    let canceled = false;
    fetchDraft(season)
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
  }, [season]);

  // Decorate each prospect with its CamPom rank and the headline Δ. The
  // CamPom rank only ranks prospects that have BOTH a draft rank and a CamPom
  // value — the same cohort the draft rank covers — so Δ stays a like-for-like
  // comparison. Display order is left to AG Grid (default sort = CamPom desc).
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
      <div>
        <h1 className="text-2xl font-bold text-gray-100 mb-1">NBA Draft Big Board</h1>
        <div className="mt-4 p-4 rounded bg-gray-800 text-gray-300">
          No draft board available for {season}.
          <div className="text-xs text-gray-500 mt-1">{error}</div>
        </div>
      </div>
    );
  }

  const matched = rows?.filter((p) => p.campom != null).length ?? 0;
  const total = rows?.length ?? 0;

  return (
    <div>
      <h1 className="text-2xl font-bold text-gray-100 mb-1">NBA Draft Big Board</h1>
      <p className="text-sm text-gray-400 mb-3">
        {season} draft class — Tankathon's ranked prospects joined to CamPom. The{' '}
        <span className="text-emerald-400 font-semibold">Δ</span> column flags CamPom's
        sleepers: positive means CamPom rates the player higher than the draft board does.
      </p>
      <div className="flex items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search prospects / teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-64"
        />
        <span className="text-xs text-gray-500">
          {total} prospects · {matched} with CamPom ·{' '}
          <a
            href="https://www.tankathon.com/big_board"
            target="_blank"
            rel="noopener noreferrer"
            className="text-blue-400 hover:underline"
          >
            Tankathon source
          </a>
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
