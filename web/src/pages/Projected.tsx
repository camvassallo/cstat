import { useEffect, useMemo, useState } from 'react';
import { useParams, Navigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchProjections, type ProjectedTeam } from '../api/client';
import { gridTheme } from '../theme';
import { SeasonLink } from '../components/SeasonLink';
import { AVAILABLE_SEASONS_FALLBACK, setPageSeasons, useSeason } from '../components/season';
import { useIsMobile } from '../components/useIsMobile';
import { caeColor, fmtCae } from '../components/cae';
import { Disclaimer, DisclaimerFooter } from '../components/Disclaimer';

// The upcoming (not-yet-played) season — the default projection target.
// Projections compose from `year - 1`, so the upcoming year is
// newest-played + 1; the backend route floors at 2016.
const UPCOMING_YEAR = AVAILABLE_SEASONS_FALLBACK[0] + 1;

// Earliest target we can project: the backend composes from `year - 1` and
// needs that base season's trajectory_oof_predictions, which start at
// target_season 2016 (a 2015 target would need un-ingested 2014 base data).
const EARLIEST_PROJECTABLE_YEAR = 2016;

// Every projectable year, newest first: the upcoming forecast plus the
// played seasons we can show a projected-vs-actual backtest for.
const PROJECTABLE_YEARS: number[] = (() => {
  const ys: number[] = [];
  for (let y = UPCOMING_YEAR; y >= EARLIEST_PROJECTABLE_YEAR; y--) ys.push(y);
  return ys;
})();

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
function buildColumns(
  isMobile: boolean,
  year: number,
  showActual: boolean,
  projRank: Map<string, number>,
  actRank: Map<string, number>,
): ColDef<ProjectedTeam>[] {
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };

  const actualColumns: ColDef<ProjectedTeam>[] = showActual
    ? [
        {
          headerName: 'Actual',
          field: 'actual_adj_em',
          ...flexCol(1, 90),
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
      valueGetter: (p) =>
        p.node && typeof p.node.rowIndex === 'number' ? p.node.rowIndex + 1 : null,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        if (p.value == null || p.data?.too_thin)
          return <span className="text-slate-600">—</span>;
        return <span className="font-bold">{p.value}</span>;
      },
    },
    {
      headerName: 'Team',
      field: 'team_name',
      ...flexCol(3, 200),
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
        const leansRoster = w < 0.45; // materially below the stable 0.50
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
    ...actualColumns,
    {
      headerName: 'Δ vs last',
      colId: 'delta_baseline',
      ...flexCol(1, 90),
      headerTooltip:
        "Projected midpoint minus last season's actual AdjEM. Positive (green) = the model thinks this roster improves on last year. Negative (red) = regression. Null when we lack a baseline (new D-I) or the projection is gated.",
      valueGetter: (p) => {
        const t = p.data;
        if (!t || t.midpoint_adj_em == null || t.baseline_adj_em == null) return null;
        return t.midpoint_adj_em - t.baseline_adj_em;
      },
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        if (p.value == null) return <span className="text-slate-600 text-xs">—</span>;
        const v = p.value;
        const baseline = p.data?.baseline_adj_em ?? 0;
        const tone =
          v >= 3
            ? 'text-emerald-300'
            : v >= 1
              ? 'text-emerald-400'
              : v > -1
                ? 'text-slate-400'
                : v > -3
                  ? 'text-rose-400'
                  : 'text-rose-300';
        const text = v >= 0 ? `+${v.toFixed(1)}` : v.toFixed(1);
        return (
          <span
            className={`text-xs font-mono font-semibold ${tone}`}
            title={`Projected ${(baseline + v).toFixed(1)} vs last year's ${baseline.toFixed(1)}`}
          >
            {text}
          </span>
        );
      },
    },
    {
      // Display-only. The coach grade is NOT in the projected AdjEM — a PIT
      // backtest found its forecast lift is program-level bias, not coaching
      // (see ROADMAP §6 / pit_cae_backtest.py), so the projection stays
      // roster-only and this column is purely contextual.
      headerName: 'Coach +/-',
      colId: 'coach_cae',
      ...flexCol(1, 100),
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
            className="text-amber-400 text-[10px] font-semibold uppercase tracking-wide mr-1"
            title={from ? `New HC — from ${from}` : 'New head coach'}
          >
            new
          </span>
        ) : null;
        const body =
          v == null ? (
            <span className="text-slate-500 text-xs font-mono" title={tip}>
              —
            </span>
          ) : (
            <span
              className="text-xs font-mono font-semibold px-1.5 py-0.5 rounded"
              style={{ color: caeColor(v), opacity: rel != null ? 0.4 + 0.6 * rel : 1 }}
              title={tip}
            >
              {label}
            </span>
          );
        const inner = (
          <span className="inline-flex items-center">
            {newBadge}
            {body}
          </span>
        );
        return t.coach_id ? (
          <SeasonLink
            to={`/coaches/${t.coach_id}`}
            onClick={(e) => e.stopPropagation()}
            className="hover:underline"
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
            : '');
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
          (pct != null ? `\nadds ${Math.round(pct * 100)}% relative to last season's roster value` : '');
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
        const names = t.top_recruits
          .map((r) => `${r.composite_rank ? `#${r.composite_rank} ` : ''}${r.name} (${r.star_rating ?? '?'}★)`)
          .join('\n');
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
          (pct != null ? `\n${Math.round(pct * 100)}% of last season's roster value lost` : '');
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

  const columns = useMemo(
    () => buildColumns(isMobile, year, hasActuals, projRank, actRank),
    [isMobile, year, hasActuals, projRank, actRank],
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
      <div className="flex items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-64"
        />
        <span className="text-xs text-gray-500">
          {scoredCount} teams scored · {thinCount} flagged thin roster
          {baseSeason != null &&
            ` · based on ${seasonLabel(baseSeason)} → projecting ${seasonLabel(year)}`}
        </span>
      </div>
      <div
        style={{
          height: 'calc(100dvh - 200px)',
          minHeight: '400px',
          width: '100%',
        }}
      >
        <AgGridReact<ProjectedTeam>
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
