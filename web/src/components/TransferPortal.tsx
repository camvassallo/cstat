import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchTransfers, type TransferRow } from '../api/client';
import { gridTheme } from '../theme';
import { campomTier, campomTierColor, campomHalfColor } from './campom';
import { onOffColor, signedRtg, adjOnOff, adjOnOffTitle } from './onoff';
import { agNullsBottom } from './tableSort';
import { classColor } from './archetypeColors';
import { SeasonLink } from './SeasonLink';
import { useIsMobile } from './useIsMobile';

// Players ranked by 247Sports who carry one of our derived ranks (we have a
// matching cstat player with a projected CamPom value). `rank_cstat` is the
// row's rank among ranked-by-247 transfers when the cohort is sorted by
// projected next-season CamPom desc — the forward-looking "who should I be
// excited about" view. `rank_delta` is `rank_247 − rank_cstat`: positive
// means cstat rates the player higher than 247 does (best value), negative
// the opposite. `campom_delta` is the trajectory projection delta
// (projected − current); negative is the regression-to-the-mean case for
// elite transfers, positive is "model expects growth". All three derived
// fields are null when we couldn't compute them.
type RankedTransfer = TransferRow & {
  rank_cstat: number | null;
  rank_delta: number | null;
  campom_delta: number | null;
};


// Renders a team cell as a link to /teams/:id when we resolved the 247 short
// name to a cstat team_id, or as plain text when we didn't (rare; small
// schools we don't carry, or "TBD" for an uncommitted next destination).
//
// `season` pins the destination season on the link: previous-team links
// land on the played base season (year of the portal cycle), next-team
// links land on the upcoming season (year + 1) — which may be a
// projection page if that season hasn't been played yet.
function teamCellRenderer(opts: {
  name: string | null;
  id: string | null;
  season: number;
  fallback?: string;
  fallbackClass?: string;
}) {
  const { name, id, season, fallback = '—', fallbackClass = 'text-gray-500' } = opts;
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

// O/D CamPom halves — shared diverging gradient, "—" where envelope-gated.
const campomHalfCell = (side: 'o' | 'd') =>
  function CampomHalfCell(p: { value: number | null }) {
    return p.value != null ? (
      <span className="tabular-nums text-xs" style={{ color: campomHalfColor(p.value, side) }}>
        {`${p.value > 0 ? '+' : ''}${p.value.toFixed(1)}`}
      </span>
    ) : (
      <span className="text-gray-600 text-xs">—</span>
    );
  };

function buildColumns(isMobile: boolean, year: number, hasOnOff: boolean): ColDef<RankedTransfer>[] {
  // Mobile: fixed natural width (= the existing minWidth) so AG Grid
  // horizontal-scrolls instead of compressing content. Desktop: keep the
  // original flex distribution.
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  return [
    {
      headerName: 'Rank',
      field: 'rank_cstat',
      width: 70,
      pinned: 'left',
      headerTooltip:
        "Our rank among 247-ranked transfers, sorted by projected next-season CamPom. Forward-looking — favors players the trajectory model expects to be more impactful next year, not just who's good right now.",
      comparator: agNullsBottom,
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
      ...flexCol(2, 150),
      sortable: false,
      cellRenderer: (p: { data?: RankedTransfer }) => {
        const cls = p.data?.primary_class;
        if (!cls) return <span className="text-gray-600 text-xs">—</span>;
        const sec = p.data?.secondary_class;
        return (
          <span
            className="text-xs font-bold uppercase tracking-wide whitespace-nowrap"
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
      headerName: 'Previous',
      field: 'previous_team',
      ...flexCol(2, 150),
      cellRenderer: (p: { data?: RankedTransfer }) =>
        teamCellRenderer({
          // Prefer the cstat Torvik short name ("Kansas") when we matched it;
          // fall back to the 247 short name verbatim if no match.
          name: p.data?.previous_team_full ?? p.data?.previous_team ?? null,
          id: p.data?.previous_team_id ?? null,
          // Source team played the portal-cycle year (e.g. a 2025 portal
          // entry was at their previous school during cstat-season 2025).
          season: year,
        }),
    },
    {
      headerName: 'Next',
      field: 'next_team',
      ...flexCol(2, 150),
      cellRenderer: (p: { data?: RankedTransfer }) =>
        teamCellRenderer({
          name: p.data?.next_team ?? null,
          id: p.data?.next_team_id ?? null,
          // Destination team takes them into the *next* cstat-season
          // (year + 1) — that's the page that reflects who they'll
          // actually play for. For year=2026 this routes to the
          // projected 2027 team page; for past portal cycles it goes
          // to the played team page for that season.
          season: year + 1,
          fallback: 'TBD',
          fallbackClass: 'text-gray-500 italic',
        }),
    },
    {
      headerName: 'Projection',
      field: 'projected_campom_mean',
      ...flexCol(1, 130),
      sort: 'desc',
      headerTooltip:
        "Projected next-season CamPom (Phase 5c trajectory model) — the headline number for this page. Source-season CamPom shows as a small grey lead-in on the left when available, so you can read the delta directly (12.5 → 11.2). Trajectory model is destination-agnostic (no team feature), so the projection assumes a role similar to source team. Sort default is by projection desc. Tied/null projections fall back to source-season CamPom for tiebreak.",
      comparator: (a, b, nodeA, nodeB, isDescending) => {
        // Primary: projected_campom_mean, with nulls pinned to the BOTTOM in
        // both directions (sub-qual / unmatched rows). AG Grid negates the
        // comparator on descending sort, so the pin must pre-invert via
        // isDescending — the old plain "+1 for null" floated blanks to the
        // top exactly on the default desc sort (same bug as agNullsBottom
        // fixes for the plain numeric columns).
        // Secondary: source-season campom, used as a tiebreak inside the
        // null cohort so those rows don't clump randomly; its own nulls pin
        // to the very bottom the same way.
        const pin = (r: number) => (isDescending ? -r : r);
        if (a == null && b == null) {
          const ca = nodeA.data?.campom ?? null;
          const cb = nodeB.data?.campom ?? null;
          if (ca == null && cb == null) return 0;
          if (ca == null) return pin(1);
          if (cb == null) return pin(-1);
          return ca - cb;
        }
        if (a == null) return pin(1);
        if (b == null) return pin(-1);
        return a - b;
      },
      cellRenderer: (p: { value: number | null; data?: RankedTransfer }) => {
        const cur = p.data?.campom ?? null;
        const lo = p.data?.projected_campom_lower;
        const hi = p.data?.projected_campom_upper;
        if (p.value == null) {
          // No projection — show source-season CamPom as the fallback
          // chip (subdued styling) so users still see something. Common
          // for transfers that didn't pass the trajectory qual gate.
          if (cur == null) return <span className="text-gray-600 text-xs">—</span>;
          return (
            <span
              className={`px-1.5 rounded border text-xs ${campomTierColor(campomTier(cur))}`}
              title="No next-season projection (transfer didn't pass the trajectory qual gate or batch inference failed). Source-season CamPom shown for reference."
            >
              {cur.toFixed(1)}
            </span>
          );
        }
        const tier = campomTier(p.value);
        // Regression-to-the-mean honesty note — same conditional as
        // PlayerDetail / PlayerProgression / TeamDetail PlayerCard.
        // Anchors on the model's *input* (source-season CamPom), not
        // the projection.
        const regressionNote =
          cur != null && cur >= 15
            ? ' Regression-to-the-mean: model under-projects elite-tier returners (≈−3 CamPom bias on inputs ≥+15). Read the q90 ceiling for the optimistic case.'
            : cur != null && cur >= 10
              ? ' Mild regression expected on this tier (≈−0.3 CamPom bias on +10..+15 inputs).'
              : '';
        const deltaStr =
          cur != null
            ? `. Source-season ${cur.toFixed(1)}, Δ ${p.value - cur >= 0 ? '+' : ''}${(p.value - cur).toFixed(1)}.`
            : '.';
        const bandStr =
          lo != null && hi != null
            ? `Projected next-season CamPom: ${p.value.toFixed(1)} (${lo.toFixed(1)}–${hi.toFixed(1)})${tier ? ` · ${tier}` : ''}${deltaStr}${regressionNote} Trajectory model is destination-agnostic — projection assumes a role similar to source team.`
            : `Projected next-season CamPom: ${p.value.toFixed(1)}${tier ? ` · ${tier}` : ''}${deltaStr}${regressionNote}`;
        return (
          <span className="inline-flex items-center gap-1.5">
            {cur != null && (
              <>
                <span className="text-[10px] text-gray-500" title="Source-season CamPom v3">
                  {cur.toFixed(1)}
                </span>
                <span className="text-gray-600 text-[10px]">→</span>
              </>
            )}
            <span
              className={`px-1.5 rounded border text-xs ${campomTierColor(tier)}`}
              title={bandStr}
            >
              {p.value.toFixed(1)}
            </span>
          </span>
        );
      },
    },
    {
      headerName: 'CPO',
      field: 'campom_o',
      ...flexCol(1, 70),
      headerTooltip:
        "Source-season CamPom offensive half (O + D = CamPom). Hidden where the decomposition is numerically unstable (±30 sanity envelope).",
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomHalfCell('o'),
    } as ColDef<RankedTransfer>,
    {
      headerName: 'CPD',
      field: 'campom_d',
      ...flexCol(1, 70),
      headerTooltip:
        "Source-season CamPom defensive half — positive is GOOD (defensive value added; O + D = CamPom). Hidden where the decomposition is numerically unstable.",
      sortingOrder: ['desc', 'asc', null],
      comparator: agNullsBottom,
      cellRenderer: campomHalfCell('d'),
    } as ColDef<RankedTransfer>,
    // Hidden when the source season has no RAPM fit (e.g. portal cycle 2019,
    // the corrupt-gated season) rather than an all-"—" column.
    ...(hasOnOff
      ? [
          {
            headerName: 'Adj On/Off',
            field: 'rapm_net',
            ...flexCol(1, 95),
            headerTooltip:
              'Source-season adj on/off (RAPM) at their old school: per-100 swing with teammates and opponents held constant (removes the garbage-time/bench bias raw on/off carries; stabilized with decayed prior-season stints). Hover a value for the raw on/off breakdown. NULL when unmatched or below the sample floor.',
            // Sort by the DISPLAYED value (null below the floor) so "—" rows
            // sink instead of clustering mid-table on hidden coefficients.
            valueGetter: (p: { data?: RankedTransfer }) =>
              p.data ? adjOnOff(p.data) : null,
            comparator: agNullsBottom,
            cellRenderer: (p: { value: number | null; data?: RankedTransfer }) =>
              p.value != null ? (
                <span style={{ color: onOffColor(p.value, 8) }} title={p.data ? adjOnOffTitle(p.data) : ''}>
                  {signedRtg(p.value)}
                </span>
              ) : (
                <span className="text-gray-600 text-xs">—</span>
              ),
          } as ColDef<RankedTransfer>,
        ]
      : []),
    {
      headerName: '247',
      field: 'rank_247',
      ...flexCol(1, 70),
      headerTooltip: '247Sports rank (— for unranked portal entries)',
      comparator: agNullsBottom,
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
      ...flexCol(1, 80),
      headerTooltip:
        'Rank value vs. 247: 247 portal rank − our projected-CamPom rank. Positive (green) means cstat rates the player higher than 247 does after factoring in next-season projection — sort desc to find best values. Negative (red) means cstat is lower on the player. This is a RANK comparison; for the projection-vs-current point delta on a single player, see ΔCP.',
      // Unranked rows pin to the bottom regardless of sort direction.
      comparator: agNullsBottom,
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
    {
      headerName: 'ΔCP',
      // Derived field — projected − current — so AG Grid sorts by the
      // signed delta directly. Computed by the parent component (see
      // RankedTransfer.campom_delta) so AG Grid can sort/compare. NULL
      // when either input is NULL.
      field: 'campom_delta',
      ...flexCol(1, 80),
      headerTooltip:
        "Projection vs. current: projected next-season CamPom − current CamPom, rounded to one decimal. Negative (red) means the model expects regression — common for elite transfers (≥+15 current) due to regression-to-the-mean in the trajectory model. Positive (green) means the model expects growth — typical for younger players still on the rising curve. Read alongside the q10–q90 band on the Proj column for the honest framing. Distinct from Δ247 (which is a RANK comparison).",
      comparator: agNullsBottom,
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-gray-600">—</span>;
        // Round once and derive both color and sign from the rounded
        // value so we never show "+0.0" in green or "-0.1" in gray
        // because the raw float sat just under the color threshold.
        const rounded = parseFloat(p.value.toFixed(1));
        const color =
          rounded > 0
            ? 'text-emerald-400'
            : rounded < 0
              ? 'text-rose-400'
              : 'text-gray-500';
        const text =
          rounded > 0
            ? `+${rounded.toFixed(1)}`
            : rounded === 0
              ? '0.0'
              : rounded.toFixed(1);
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
  const [rows, setRows] = useState<RankedTransfer[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const isMobile = useIsMobile();

  useEffect(() => {
    let canceled = false;
    fetchTransfers(year)
      .then((r) => {
        if (canceled) return;
        // Sort by PROJECTED CamPom desc — the forward-looking ranking. Rows
        // lacking either projection or 247 rank stay in the array but skip
        // the rank counter so the displayed `rank_cstat` matches on-screen
        // position. Tiebreak in the no-projection cohort uses source-season
        // CamPom desc so those rows are at least internally sorted. The
        // endpoint returns the full portal (including unranked-by-247
        // entries) for the 2027-projection roster aggregator; those don't
        // compete for a rank here. Δ247 reads directly off `rank_cstat`,
        // so this switch propagates the projection-based interpretation
        // into the value-vs-247 column too.
        const sorted = [...r.transfers].sort((a, b) => {
          const ap = a.projected_campom_mean;
          const bp = b.projected_campom_mean;
          if (ap != null && bp != null) return bp - ap;
          if (ap != null) return -1;
          if (bp != null) return 1;
          // Both projections null — tiebreak on source-season campom.
          if (a.campom == null && b.campom == null) return 0;
          if (a.campom == null) return 1;
          if (b.campom == null) return -1;
          return b.campom - a.campom;
        });
        let i = 0;
        const withRank: RankedTransfer[] = sorted.map((t) => {
          // Rank only when BOTH projection and 247 rank present — keeps
          // the rank a single-meaning number (= position when sorted by
          // projected CamPom among 247-ranked transfers).
          const rank_cstat =
            t.projected_campom_mean != null && t.rank_247 != null ? ++i : null;
          // campom_delta needs BOTH current and projected CamPom. The
          // route serves projection NULLs for unmatched / sub-qual rows;
          // we don't fabricate one here just because current CamPom is
          // available, since the model couldn't actually run.
          const campom_delta =
            t.campom != null && t.projected_campom_mean != null
              ? t.projected_campom_mean - t.campom
              : null;
          return {
            ...t,
            rank_cstat,
            rank_delta:
              rank_cstat != null ? t.rank_247! - rank_cstat : null,
            campom_delta,
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

  const hasOnOff = useMemo(() => (rows ?? []).some((r) => adjOnOff(r) != null), [rows]);
  const columns = useMemo(
    () => buildColumns(isMobile, year, hasOnOff),
    [isMobile, year, hasOnOff],
  );

  const filtered = useMemo(() => {
    if (!rows) return null;
    // `rank_cstat` is only assigned when both projected CamPom and 247
    // rank are present (see useEffect above), so this single check drops
    // the unranked-by-247 long tail and the no-projection rows in one
    // pass. Sub-qual transfers (those without a trajectory projection)
    // fall out of the ranked view; they still ride along on the page-
    // level state for the 2027 roster aggregator.
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
          {hidden > 0 && ` · ${hidden} hidden (unranked by 247 or no projection)`} ·{' '}
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
      <div style={{ height: 'calc(100dvh - 220px)', minHeight: '400px', width: '100%' }}>
        <AgGridReact<RankedTransfer>
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
