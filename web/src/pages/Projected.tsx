import { useEffect, useMemo, useState } from 'react';
import { useParams, Navigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchProjections, type ProjectedTeam } from '../api/client';
import { gridTheme } from '../theme';
import { SeasonLink } from '../components/SeasonLink';
import {
  projectableSeasons,
  setPageSeasons,
  upcomingProjectionSeason,
  useSeason,
} from '../components/season';
import { useIsMobile } from '../components/useIsMobile';
import { caeColor, fmtCae } from '../components/cae';
import { pctileTextColor } from '../components/pctile';
import { Disclaimer, DisclaimerFooter } from '../components/Disclaimer';
import { recruitTooltipLine } from '../lib/recruitDisplay';

// Projectable-year definitions are shared with the team projection ledger via
// `season.ts` so both surfaces publish the same list (incl. the upcoming
// forecast year) to the navbar picker.
const UPCOMING_YEAR = upcomingProjectionSeason();
const PROJECTABLE_YEARS = projectableSeasons();

// cstat-season year → "2026-27"-style college-season label.
const seasonLabel = (year: number) => `${year - 1}-${String(year).slice(2)}`;

// AdjEM tier coloring (tuned for D-I 2025 distribution where teams
// range ~-30 to +45). Reused for floor/ceiling/midpoint/actual chips.
function adjEmTone(v: number): string {
  if (v >= 25) return 'bg-emerald-900/50 border-emerald-700 text-emerald-200';
  if (v >= 15) return 'bg-emerald-950/40 border-emerald-800 text-emerald-300';
  if (v >= 5) return 'bg-teal-950/40 border-teal-800 text-teal-300';
  if (v >= -5) return 'bg-slate-800/40 border-slate-700 text-slate-300';
  if (v >= -15) return 'bg-amber-950/40 border-amber-800 text-amber-300';
  return 'bg-rose-950/40 border-rose-800 text-rose-300';
}

const adjEmChip = (v: number | null) => {
  if (v == null) return <span className="text-slate-600 text-xs">—</span>;
  return (
    <span className={`px-1.5 rounded border text-xs font-semibold ${adjEmTone(v)}`}>
      {v >= 0 ? `+${v.toFixed(1)}` : v.toFixed(1)}
    </span>
  );
};

// AG Grid sorts ascending then reverses for descending; to pin nulls
// (thin rosters / missing actuals) to the visual bottom regardless of
// direction, flip the null result by `isDescending`.
function nullsLast(
  a: number | null,
  b: number | null,
  _na: unknown,
  _nb: unknown,
  isDescending: boolean,
): number {
  if (a == null && b == null) return 0;
  if (a == null) return isDescending ? -1 : 1;
  if (b == null) return isDescending ? 1 : -1;
  return a - b;
}

const dashCell = <span className="text-slate-600 text-xs">—</span>;
const fmtSigned = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(1)}`;

// Tooltip line for a cohort's prior-season CamPom O/D split. Empty when
// both halves are 0 (no O/D coverage for the cohort — e.g. the server
// degraded the split fetch, or every member is envelope-gated).
const odTipLine = (o: number, d: number) =>
  o !== 0 || d !== 0 ? `\nprior O/D split: O ${fmtSigned(o)} / D ${fmtSigned(d)}` : '';

// Last season's roster value (CamPom) — the shared denominator that makes
// the roster-flow columns mesh: `returning + departures + uncertain`, all
// on the prior-season frame (every player who was on last year's team).
// Returns null when the base is too small to normalize against (very weak
// or thin rosters, where cam sums can be near zero / negative).
function lastSeasonBase(t: ProjectedTeam): number | null {
  const base = t.returning_cam_v3_sum + t.departures_cam_v3_sum + t.uncertain_cam_v3_sum;
  return base > 0.5 ? base : null;
}
const pctOfBase = (t: ProjectedTeam, numerator: number): number | null => {
  const base = lastSeasonBase(t);
  return base == null ? null : numerator / base;
};

// One roster-flow cell: a CamPom value (forward-projected for staying /
// incoming cohorts, prior for departures) with its share of last season's
// roster value beneath it. The shared base is the mesh — kept% + lost%
// partition the old roster; transfers / recruits are additions on the same
// scale, so the columns finally add up instead of floating independently.
function flowCellView(
  valueText: string,
  pctText: string | null,
  tone: string,
  tooltip: string,
) {
  return (
    <span title={tooltip} className="inline-flex items-baseline gap-1 whitespace-nowrap">
      <span className={`text-xs font-mono font-semibold ${tone}`}>{valueText}</span>
      {pctText && <span className="text-[10px] text-slate-500 font-mono">{pctText}</span>}
    </span>
  );
}

// `showActual` adds the Actual + projection-error columns — populated
// only for past seasons (the live forecast year has no actual yet).
// `projRank`/`actRank` are team_id → 1-based rank (by projected and actual
// AdjEM respectively), precomputed over the whole field so the rank-error
// column can read them per row.
// Per-team rank + color for a single O/D metric (rank 1 = best → greenest).
type RankCell = { rank: number; color: string };
// The projected O/D columns' rank cells, keyed by metric. Partial — a team
// with no value for a metric (e.g. a too-thin roster) simply has no cell.
type EffRank = { ao: RankCell; ad: RankCell };

function buildColumns(
  isMobile: boolean,
  year: number,
  showActual: boolean,
  projRank: Map<string, number>,
  actRank: Map<string, number>,
  effRank: Map<string, Partial<EffRank>>,
): ColDef<ProjectedTeam>[] {
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  // Spread onto the leading column of each logical group to draw the
  // vertical divider. Inline cellStyle (body cells only — header stays clean).
  // gray-800 (matching Rankings/roster) reads as the grid background here, so
  // we use a visibly lighter slate-600 line to actually separate the groups.
  const divider = { cellStyle: { borderLeft: '1px solid #4b5563' } };

  // Projected offensive / defensive efficiency (absolute ~105, KenPom
  // convention). The NET+SPLIT halves of the headline: AdjEM = AdjO − AdjD,
  // so they reconcile exactly. AdjO sorts high-first (better offense); AdjD
  // sorts low-first (better defense). Descriptive, never a coach grade.
  const projEffCol = (side: 'o' | 'd'): ColDef<ProjectedTeam> => {
    const field = side === 'o' ? 'projected_adj_o' : 'projected_adj_d';
    return {
      headerName: side === 'o' ? 'Proj AdjO' : 'Proj AdjD',
      colId: side === 'o' ? 'proj_adjo' : 'proj_adjd',
      ...flexCol(1, 92),
      headerTooltip:
        side === 'o'
          ? 'Projected offensive efficiency (points scored per 100 possessions, ~105 scale; higher is better). The offensive half of the net projection — AdjEM = AdjO − AdjD. Descriptive, not a coach grade.'
          : 'Projected defensive efficiency (points allowed per 100 possessions, ~105 scale; LOWER is better). Derived as AdjO − AdjEM so the split reconciles exactly to the Proj AdjEM headline.',
      sortingOrder: side === 'o' ? ['desc', 'asc', null] : ['asc', 'desc', null],
      valueGetter: (p) => (p.data as ProjectedTeam | undefined)?.[field] ?? null,
      comparator: nullsLast,
      // Value with a small "#rank" subscript colored by field-wide rank
      // percentile — the Rankings-page convention (RankedCell). Direction
      // baked into the rank: #1 = best offense (AdjO) / best defense (AdjD).
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        if (p.value == null) return dashCell;
        const cell = p.data && effRank.get(p.data.team_id)?.[side === 'o' ? 'ao' : 'ad'];
        return (
          <div className="leading-tight">
            <div className="text-xs font-mono text-slate-200">{p.value.toFixed(1)}</div>
            {cell && (
              <div className="text-[10px] font-mono" style={{ color: cell.color }}>
                #{cell.rank}
              </div>
            )}
          </div>
        );
      },
    };
  };

  const actualColumns: ColDef<ProjectedTeam>[] = showActual
    ? [
        {
          headerName: 'Actual',
          field: 'actual_adj_em',
          ...flexCol(1, 90),
          ...divider,
          headerTooltip: `The team's actual AdjEM for ${seasonLabel(year)} — what really happened. Shown for completed seasons so the projection can be graded.`,
          comparator: nullsLast,
          cellRenderer: (p: { value: number | null }) => adjEmChip(p.value),
        },
        {
          headerName: 'Δ vs act',
          colId: 'proj_error',
          ...flexCol(1, 100),
          headerTooltip:
            'Projected midpoint minus actual AdjEM — the forecast error. Positive = we over-projected, negative = under-projected. Near zero is a good call.',
          valueGetter: (p) => {
            const t = p.data;
            if (!t || t.midpoint_adj_em == null || t.actual_adj_em == null) return null;
            return t.midpoint_adj_em - t.actual_adj_em;
          },
          comparator: nullsLast,
          cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
            if (p.value == null) return <span className="text-slate-600 text-xs">—</span>;
            const v = p.value;
            const mag = Math.abs(v);
            const tone =
              mag <= 3 ? 'text-emerald-300' : mag <= 7 ? 'text-amber-300' : 'text-rose-300';
            const t = p.data;
            const title =
              t && t.midpoint_adj_em != null && t.actual_adj_em != null
                ? `Projected ${t.midpoint_adj_em.toFixed(1)} vs actual ${t.actual_adj_em.toFixed(1)}`
                : undefined;
            return (
              <span className={`text-xs font-mono font-semibold ${tone}`} title={title}>
                {v >= 0 ? `+${v.toFixed(1)}` : v.toFixed(1)}
              </span>
            );
          },
        },
        {
          headerName: 'Δ rank',
          colId: 'rank_error',
          ...flexCol(1, 100),
          headerTooltip:
            'Projected rank minus actual rank (both by AdjEM across the field this season). Positive = the team finished higher than projected (climbed the ranking); negative = fell short. Color tracks the magnitude of the miss, not its direction — green is an accurate ranking, red a large one.',
          valueGetter: (p) => {
            const t = p.data;
            if (!t) return null;
            const pr = projRank.get(t.team_id);
            const ar = actRank.get(t.team_id);
            if (pr == null || ar == null) return null;
            return pr - ar;
          },
          comparator: nullsLast,
          cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
            if (p.value == null) return <span className="text-slate-600 text-xs">—</span>;
            const v = p.value;
            const mag = Math.abs(v);
            const tone =
              mag <= 10 ? 'text-emerald-300' : mag <= 25 ? 'text-amber-300' : 'text-rose-300';
            const t = p.data;
            const pr = t ? projRank.get(t.team_id) : undefined;
            const ar = t ? actRank.get(t.team_id) : undefined;
            const title =
              pr != null && ar != null ? `Projected #${pr} vs actual #${ar}` : undefined;
            return (
              <span className={`text-xs font-mono font-semibold ${tone}`} title={title}>
                {v >= 0 ? `+${v}` : v}
              </span>
            );
          },
        },
      ]
    : [];

  return [
    {
      headerName: 'Rank',
      colId: 'rank',
      width: 60,
      pinned: 'left',
      sortable: false,
      // The team's true field-wide rank (by projected AdjEM, the default sort),
      // NOT its position in the currently displayed rows. Reading `rowIndex`
      // here renumbers 1..N over whatever subset is visible, so a search that
      // matches one team showed it as rank 1 (issue #121). `projRank` is the
      // stable rank over the full field, so it stays fixed under filter/re-sort.
      valueGetter: (p) =>
        p.data && !p.data.too_thin ? (projRank.get(p.data.team_id) ?? null) : null,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        if (p.value == null || p.data?.too_thin)
          return <span className="text-slate-600">—</span>;
        return <span className="font-bold">{p.value}</span>;
      },
    },
    {
      headerName: 'Team',
      field: 'team_name',
      ...flexCol(2, 130),
      pinned: 'left',
      cellRenderer: (p: { value: string; data?: ProjectedTeam }) => (
        // `&view=projected` lands on TeamDetail's projection branch even
        // for a played season — clicking a team *from the projections
        // context* shows its forecast + the held-out report card (a normal
        // team link elsewhere stays on Actual). For the upcoming year the
        // page is projection-only regardless, so the param is harmless.
        <SeasonLink
          to={`/teams/${p.data?.team_id}?season=${year}&view=projected`}
          onClick={(e) => e.stopPropagation()}
          className="text-blue-400 hover:underline"
        >
          {p.value}
        </SeasonLink>
      ),
    },
    {
      headerName: 'Proj AdjEM',
      field: 'midpoint_adj_em',
      ...flexCol(1, 100),
      sort: 'desc',
      headerTooltip:
        "The projected AdjEM for this roster: the roster-impact model's projected-roster output blended with last season's actual AdjEM. The blend is ~50/50 for continuity rosters, but leans toward the roster model for heavy-turnover teams (last year's result is a stale anchor when the roster overhauls).",
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        const chip = adjEmChip(p.value);
        const baseline = p.data?.baseline_adj_em;
        if (p.value == null || baseline == null) return chip;
        const w = p.data?.baseline_weight ?? 0.5;
        const bw = Math.round(w * 100);
        // The stable cap is PROJECTION_SHRINK_WEIGHT = 0.45, but it's an f32 on
        // the backend and serializes to JSON as 0.44999998…, so a naive `w <
        // 0.45` was TRUE for every team (including the ~75% at the stable cap)
        // and the badge fired on all 364. Threshold just under the served cap
        // so only genuine roster-overhaul teams (retained < ~40%, weight pulled
        // materially below 0.45) light up.
        const leansRoster = w < 0.449;
        const title =
          `${bw}% last year's actual AdjEM (${baseline >= 0 ? '+' : ''}${baseline.toFixed(1)}) ` +
          `+ ${100 - bw}% the roster model's projection` +
          (leansRoster ? ' — leaning on the new roster (heavy turnover)' : '');
        return (
          <span title={title} className="inline-flex items-center gap-1">
            {chip}
            {leansRoster && <span className="text-amber-400/80 text-[10px]" aria-hidden>⟳</span>}
          </span>
        );
      },
    },
    projEffCol('o'),
    projEffCol('d'),
    ...actualColumns,
    {
      // Display-only. The coach grade is NOT in the projected AdjEM — a PIT
      // backtest found its forecast lift is program-level bias, not coaching
      // (see ROADMAP §6 / pit_cae_backtest.py), so the projection stays
      // roster-only and this column is purely contextual.
      headerName: 'Coach +/-',
      colId: 'coach_cae',
      ...flexCol(1.4, 130),
      ...divider,
      headerTooltip:
        "Descriptive only — NOT included in the projected AdjEM. The head coach's career Coach-Above-Expectation: how much this program has historically beaten (or missed) its roster-only projection under them, EB-shrunk toward 0 for short tenures. A point-in-time backtest showed this signal is program-level, not coaching, so it never moves the forecast — it's shown for context.",
      valueGetter: (p) => (p.data as ProjectedTeam | undefined)?.coach_cae_shrunk ?? null,
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.coach_name == null) return <span className="text-slate-600 text-xs">—</span>;
        const v = t.coach_cae_shrunk;
        const rel = t.coach_cae_reliability;
        const n = t.coach_n_seasons;
        const isNew = t.coach_is_new_hc === true;
        const from = t.coach_prev_team;
        const newNote = isNew
          ? `\nNew head coach${from ? ` — arrived from ${from}` : ' (first season)'}`
          : '';
        const tip =
          (v == null
            ? `${t.coach_name} — no career CAE rating yet (thin/unscored tenure)`
            : `${t.coach_name}: career CAE ${fmtCae(v)} AdjEM over ${n ?? '?'} scored season${n === 1 ? '' : 's'}` +
              `${rel != null ? ` (reliability ${(rel * 100).toFixed(0)}%)` : ''}. Descriptive — not in the projection.`) +
          newNote;
        const label = fmtCae(v);
        // Amber "new" tag flags an incoming HC (display-only — coachdict is_new_hc).
        const newBadge = isNew ? (
          <span
            className="text-amber-400 text-[9px] font-semibold uppercase tracking-wide shrink-0"
            title={from ? `New HC — from ${from}` : 'New head coach'}
          >
            new
          </span>
        ) : null;
        // Two-line cell (name over the +/- value), matching the Returning /
        // Proj AdjO convention: the name is now legible without hovering, and
        // the extra column width has somewhere useful to go.
        const inner = (
          <div
            className="leading-tight text-center flex flex-col justify-center h-full"
            title={tip}
          >
            <div className="text-[11px] text-slate-300 truncate">{t.coach_name}</div>
            <div className="flex items-center justify-center gap-1">
              {v == null ? (
                <span className="text-[10px] font-mono text-slate-500">—</span>
              ) : (
                <span
                  className="text-[10px] font-mono font-semibold"
                  style={{ color: caeColor(v), opacity: rel != null ? 0.4 + 0.6 * rel : 1 }}
                >
                  {label}
                </span>
              )}
              {newBadge}
            </div>
          </div>
        );
        return t.coach_id ? (
          <SeasonLink
            to={`/coaches/${t.coach_id}`}
            onClick={(e) => e.stopPropagation()}
            className="hover:underline block h-full"
          >
            {inner}
          </SeasonLink>
        ) : (
          inner
        );
      },
    },
    {
      headerName: 'Returning',
      colId: 'returning',
      ...flexCol(1, 120),
      ...divider,
      headerTooltip:
        "Returning players, shown as their *projected* next-season CamPom (trajectory forecast) with the share of last season's roster value retained beneath — i.e. roster continuity. 51% kept = a stable veteran core; 20% = a near-total rebuild. Excludes graduating seniors, outbound portal, and firm draft departures.",
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.returning_projected_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.returning_projected_cam_v3_sum ?? 0),
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.returning_count === 0) return dashCell;
        const val = t.returning_projected_cam_v3_sum;
        const pct = pctOfBase(t, t.returning_cam_v3_sum);
        const pctText = pct != null ? `${Math.round(pct * 100)}% kept` : null;
        const tip =
          `${t.returning_count} returning · prior Σ ${fmtSigned(t.returning_cam_v3_sum)} → projected ${fmtSigned(val)}` +
          (pct != null
            ? `\n${Math.round(pct * 100)}% of last season's roster value retained (continuity)`
            : '') +
          odTipLine(t.returning_cam_o_sum, t.returning_cam_d_sum);
        return flowCellView(fmtSigned(val), pctText, 'text-slate-200', tip);
      },
    },
    {
      headerName: 'Incoming transfers',
      colId: 'incoming',
      ...flexCol(1, 130),
      headerTooltip:
        "Incoming portal arrivals — their *projected* next-season CamPom, with how much they add relative to last season's roster value beneath. Hover for the prior-school total + headcount.",
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.arrivals_projected_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.arrivals_projected_cam_v3_sum ?? 0),
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.arrivals_count === 0) return dashCell;
        const val = t.arrivals_projected_cam_v3_sum;
        const pct = pctOfBase(t, t.arrivals_cam_v3_sum);
        const tone = val >= 0 ? 'text-emerald-400' : 'text-rose-400';
        const pctText = pct != null ? `+${Math.round(pct * 100)}%` : null;
        const tip =
          `${t.arrivals_count} transfers in · prior-school Σ ${fmtSigned(t.arrivals_cam_v3_sum)} → projected ${fmtSigned(val)}` +
          (pct != null ? `\nadds ${Math.round(pct * 100)}% relative to last season's roster value` : '') +
          odTipLine(t.arrivals_cam_o_sum, t.arrivals_cam_d_sum);
        return flowCellView(fmtSigned(val), pctText, tone, tip);
      },
    },
    {
      headerName: 'Recruits',
      colId: 'recruits',
      ...flexCol(1, 130),
      headerTooltip:
        "Incoming HS recruits — projected freshman-season CamPom from cstat's freshman-impact model (no prior season, so this is a forward projection), with how much they add relative to last season's roster value beneath. Hover for the top commits.",
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.recruits_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.recruits_cam_v3_sum ?? 0),
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.recruits_count === 0) return dashCell;
        const val = t.recruits_cam_v3_sum;
        const pct = pctOfBase(t, t.recruits_cam_v3_sum);
        const pctText = pct != null ? `+${Math.round(pct * 100)}%` : null;
        const names = t.top_recruits.map(recruitTooltipLine).join('\n');
        const tip =
          `${t.recruits_count} recruits · Σ projected CamPom ${fmtSigned(val)}` +
          (pct != null ? `\nadds ${Math.round(pct * 100)}% relative to last season's roster value` : '') +
          (names ? `\n${names}` : '');
        return flowCellView(fmtSigned(val), pctText, 'text-slate-300', tip);
      },
    },
    {
      headerName: 'Departures',
      colId: 'departures',
      ...flexCol(1, 130),
      headerTooltip:
        "Graduating seniors + outbound portal + firm draft departures — the CamPom leaving the program (prior-season production), with the share of last season's roster value lost beneath. Mirror of Returning: kept% + lost% ≈ 100%.",
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.departures_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.departures_cam_v3_sum ?? 0),
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.departures_count === 0) return dashCell;
        // Stored positive (talent leaving); show as a negative loss.
        const display = -t.departures_cam_v3_sum;
        const pct = pctOfBase(t, t.departures_cam_v3_sum);
        const pctText = pct != null ? `−${Math.round(pct * 100)}%` : null;
        const tip =
          `${t.departures_count} departures (Sr + portal-out + draft) · Σ prior CamPom ${fmtSigned(t.departures_cam_v3_sum)} leaving` +
          (pct != null ? `\n${Math.round(pct * 100)}% of last season's roster value lost` : '') +
          odTipLine(t.departures_cam_o_sum, t.departures_cam_d_sum);
        return flowCellView(fmtSigned(display), pctText, 'text-rose-400', tip);
      },
    },
  ];
}

/// Routing shim — the navbar season picker drives this page through the
/// global `?season=` param (same control the rest of the site uses). We
/// read the year from the very same `useSeason()` the navbar binds to,
/// so the two can't disagree — crucially, when the user selects the
/// default season the picker drops the param, and `useSeason()` resolves
/// the absent param right back to that default. The `Future` nav link
/// carries `?season=2027`, so the page still *lands* on the upcoming
/// forecast. A non-projectable season (e.g. one carried over from
/// elsewhere) redirects to the forecast instead of 400-ing the API.
export default function Projected() {
  const { season: year } = useSeason();
  if (!PROJECTABLE_YEARS.includes(year)) {
    return <Navigate to={`/projected?season=${UPCOMING_YEAR}`} replace />;
  }
  // `key={year}` remounts the view on a year switch, so its state
  // (teams / error) resets to the loading state without an in-effect
  // setState reset.
  return <ProjectionView key={year} year={year} />;
}

/// Back-compat shim for the page's old `/projected/:year` home, before
/// the navbar picker took over via `?season=`. Redirects to the new form.
export function ProjectedYearRedirect() {
  const { year } = useParams<{ year: string }>();
  return <Navigate to={`/projected?season=${year ?? UPCOMING_YEAR}`} replace />;
}

function ProjectionView({ year }: { year: number }) {
  const [teams, setTeams] = useState<ProjectedTeam[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [baseSeason, setBaseSeason] = useState<number | null>(null);
  const [search, setSearch] = useState('');
  const isMobile = useIsMobile();

  // Publish the projectable years to the site-wide season picker in the
  // navbar (the same mechanism the team/player detail pages use). The
  // navbar dropdown then lists the forecast + backtest years and its
  // selection flows back in through `?season=`. Released on unmount.
  useEffect(() => {
    setPageSeasons(PROJECTABLE_YEARS);
    return () => setPageSeasons(null);
  }, []);

  useEffect(() => {
    let canceled = false;
    fetchProjections(year)
      .then((r) => {
        if (canceled) return;
        setTeams(r.teams);
        setBaseSeason(r.base_season);
      })
      .catch((e) => {
        if (!canceled) setError(String(e));
      });
    return () => {
      canceled = true;
    };
  }, [year]);

  // Past seasons carry actuals; the live forecast year doesn't.
  const hasActuals = useMemo(
    () => teams?.some((t) => t.actual_adj_em != null) ?? false,
    [teams],
  );

  // Field-wide ranks for the Proj − Act rank column: by projected AdjEM
  // (excluding too-thin rosters, whose projection is unreliable) and by
  // actual AdjEM. team_id → 1-based rank, newest computed on each fetch.
  const { projRank, actRank } = useMemo(() => {
    const projRank = new Map<string, number>();
    const actRank = new Map<string, number>();
    if (teams) {
      [...teams]
        .filter((t) => !t.too_thin && t.midpoint_adj_em != null)
        .sort((a, b) => (b.midpoint_adj_em as number) - (a.midpoint_adj_em as number))
        .forEach((t, i) => projRank.set(t.team_id, i + 1));
      [...teams]
        .filter((t) => t.actual_adj_em != null)
        .sort((a, b) => (b.actual_adj_em as number) - (a.actual_adj_em as number))
        .forEach((t, i) => actRank.set(t.team_id, i + 1));
    }
    return { projRank, actRank };
  }, [teams]);

  // Field-wide ranks for the projected O/D columns — Proj AdjO / Proj AdjD —
  // over scored teams, with the better direction as rank 1 (AdjD
  // lower-is-better, AdjO higher). Each cell carries its 1-based rank and the
  // rank-percentile color (`pctileTextColor`), rendered as a small "#N"
  // subscript under the value — the Rankings-page convention.
  const effRank = useMemo(() => {
    const m = new Map<string, Partial<EffRank>>();
    const scored = teams?.filter((t) => !t.too_thin && t.projected_adj_o != null) ?? [];
    if (scored.length < 2) return m;
    const assign = (slot: keyof EffRank, key: (t: ProjectedTeam) => number | null, dir: 1 | -1) => {
      const rows = [...scored].filter((t) => key(t) != null);
      rows.sort((a, b) => dir * ((key(b) as number) - (key(a) as number)));
      rows.forEach((t, i) => {
        const pct = 1 - i / (rows.length - 1);
        const cur = m.get(t.team_id) ?? {};
        cur[slot] = { rank: i + 1, color: pctileTextColor(pct) };
        m.set(t.team_id, cur);
      });
    };
    assign('ao', (t) => t.projected_adj_o, 1);
    assign('ad', (t) => t.projected_adj_d, -1); // lower = better
    return m;
  }, [teams]);

  const columns = useMemo(
    () => buildColumns(isMobile, year, hasActuals, projRank, actRank, effRank),
    [isMobile, year, hasActuals, projRank, actRank, effRank],
  );

  const filtered = useMemo(() => {
    if (!teams) return null;
    const q = search.trim().toLowerCase();
    if (!q) return teams;
    return teams.filter(
      (t) =>
        t.team_name.toLowerCase().includes(q) ||
        t.team_full_name.toLowerCase().includes(q),
    );
  }, [teams, search]);

  // Forecast accuracy: midpoint vs actual over teams that have both.
  // `null` for the live year (no actuals) — the banner is then hidden.
  const accuracy = useMemo(() => {
    if (!teams) return null;
    const errs = teams
      .filter((t) => t.midpoint_adj_em != null && t.actual_adj_em != null)
      .map((t) => (t.midpoint_adj_em as number) - (t.actual_adj_em as number));
    if (errs.length === 0) return null;
    const mae = errs.reduce((s, e) => s + Math.abs(e), 0) / errs.length;
    const bias = errs.reduce((s, e) => s + e, 0) / errs.length;
    return { n: errs.length, mae, bias };
  }, [teams]);

  if (error) {
    return (
      <div className="p-4 text-rose-300">Failed to load projections: {error}</div>
    );
  }

  const scoredCount = teams?.filter((t) => !t.too_thin).length ?? 0;
  const thinCount = teams?.filter((t) => t.too_thin).length ?? 0;

  return (
    <div className="p-4">
      <div className="flex flex-wrap items-center gap-3 mb-2">
        {/* The year is driven by the site-wide season picker in the navbar
            (see the `setPageSeasons` effect above) — no page-local picker. */}
        <h1 className="text-2xl font-bold">Projected {seasonLabel(year)}</h1>
      </div>
      <div className="flex flex-wrap items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-full sm:w-64"
        />
        <span className="text-xs text-gray-500">
          {scoredCount} teams scored · {thinCount} flagged thin roster
          {baseSeason != null &&
            ` · based on ${seasonLabel(baseSeason)} → projecting ${seasonLabel(year)}`}
        </span>
      </div>
      <div
        style={{
          // Leave room below the fold so the methodology footer peeks in (the
          // top caveat boxes moved down there, so the grid no longer fills the
          // whole viewport).
          height: 'calc(100dvh - 240px)',
          minHeight: '400px',
          width: '100%',
        }}
      >
        <AgGridReact<ProjectedTeam>
          theme={gridTheme}
          columnDefs={columns}
          rowData={filtered ?? []}
          rowHeight={48}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
          }}
        />
      </div>
      <DisclaimerFooter>
        {accuracy && (
          <Disclaimer tone="emerald" label="Backtest receipt:">
            this is the forecast we'd have made going into {seasonLabel(year)},
            graded against what actually happened. Across {accuracy.n} teams,
            mean absolute error{' '}
            <strong>{accuracy.mae.toFixed(1)} AdjEM</strong>, mean bias{' '}
            {accuracy.bias >= 0 ? '+' : ''}
            {accuracy.bias.toFixed(1)} (
            {accuracy.bias >= 0
              ? 'over-projected on average'
              : 'under-projected on average'}
            ). See the <strong>Actual</strong> and <strong>Proj − Act</strong>{' '}
            columns per team.
          </Disclaimer>
        )}
        <Disclaimer label="v3 honesty caveats:">
          Holistic projection: returners (minus seniors, outbound portal, and
          firm draft departures) + incoming portal commits +{' '}
          <strong>incoming HS recruits</strong>. Every player is scored on a{' '}
          <strong>projected</strong> next-season CamPom v3 — the trajectory
          model for returners and arrivals, the freshman-impact model for
          recruits — so returner growth and freshman upside both count (a
          junior breaking out as a senior, or an elite recruit, moves the
          number). The roster's projected-CamPom distribution is scored by the
          Phase B impact-aggregation model, then blended{' '}
          <strong>55/45 with last season's actual AdjEM</strong> (no
          calibration offset — the model is near-unbiased). The pipeline
          backtests at <strong>5.7 AdjEM MAE</strong> against actual next-season
          results across the <strong>2016–2026</strong> seasons. Rosters with
          &lt;7 qualifying players (returning + arrivals + recruits) are flagged
          "thin roster" and not scored.
        </Disclaimer>
        <Disclaimer label="Regression to the mean:">
          these are <strong>preseason</strong> projections, so the ordering is
          compressed toward average — the bottom of the table trends{' '}
          <em>up</em> and the top trends <em>down</em> relative to last season.
          That's not a bug: roughly <strong>23% of team-AdjEM variance is
          preseason-invisible</strong> (no roster signal can see it), an
          irreducible floor, so read ranks as <em>directional</em>, not
          point-estimates. Elite returners regress hardest — the trajectory
          model under-projects the +15-and-up CamPom tail by design, so a
          reigning star projects below his current number. Heavy-turnover /
          new-coach rosters (marked{' '}
          <span className="text-amber-400/80" aria-hidden>
            ⟳
          </span>
          ) lean off last year's stale record and carry the widest error bands.
        </Disclaimer>
      </DisclaimerFooter>
    </div>
  );
}
