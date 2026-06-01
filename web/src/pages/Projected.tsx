import { useEffect, useMemo, useState } from 'react';
import { useParams, Navigate } from 'react-router-dom';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchProjections, type ProjectedTeam } from '../api/client';
import { gridTheme } from '../theme';
import { SeasonLink } from '../components/SeasonLink';
import { AVAILABLE_SEASONS_FALLBACK, setPageSeasons, useSeason } from '../components/season';
import { useIsMobile } from '../components/useIsMobile';

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

type CamSumField =
  | 'returning_cam_v3_sum'
  | 'arrivals_cam_v3_sum'
  | 'recruits_cam_v3_sum'
  | 'departures_cam_v3_sum';
type CamCountField =
  | 'returning_count'
  | 'arrivals_count'
  | 'recruits_count'
  | 'departures_count';

// Render a Σ-CamPom roster-flow column as a signed number. `polarity`
// sets the framing:
//   gain — incoming talent (green +); a negative Σ flips to a red loss.
//   loss — departing talent (red −); the stored Σ is positive (talent
//          leaving), so we negate it for display.
//   base — the standing cohort (returning / recruits). Neutral slate;
//          it's "what you have", not a flow, so no gain/loss coloring.
// The player count and raw Σ live in the tooltip. `campomBasis` labels
// whether the Σ is prior-season production or a forward projection.
function camSumRenderer(
  sumField: CamSumField,
  countField: CamCountField,
  polarity: 'gain' | 'loss' | 'base',
  noun: string,
  campomBasis: string,
) {
  return (p: { data?: ProjectedTeam }) => {
    const t = p.data;
    // ΣCamPom is a fact about the roster (not a gated prediction), so it
    // renders even for thin-roster teams — for a gutted roster the
    // departures total is exactly the signal that explains the thinness.
    if (!t || t[countField] === 0) {
      return <span className="text-slate-600 text-xs">—</span>;
    }
    const sum = t[sumField];
    const effect = polarity === 'loss' ? -sum : sum;
    const tone =
      polarity === 'base'
        ? 'text-slate-300'
        : effect >= 0
          ? 'text-emerald-400'
          : 'text-rose-400';
    const text = effect >= 0 ? `+${effect.toFixed(1)}` : effect.toFixed(1);
    return (
      <span
        className={`text-xs font-mono font-semibold ${tone}`}
        title={`${t[countField]} ${noun} · Σ ${campomBasis} CamPom ${sum >= 0 ? '+' : ''}${sum.toFixed(1)}`}
      >
        {text}
      </span>
    );
  };
}

// `showActual` adds the Actual + projection-error columns — populated
// only for past seasons (the live forecast year has no actual yet).
function buildColumns(
  isMobile: boolean,
  year: number,
  showActual: boolean,
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
          headerName: 'Proj − Act',
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
        // For the upcoming forecast year, `?season={year}` lands on
        // TeamDetail's projection-mode branch (the season hasn't been
        // played). For a past season it lands on the actual played
        // team page — "here's how that roster really did".
        <SeasonLink
          to={`/teams/${p.data?.team_id}?season=${year}`}
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
        "The projected AdjEM for this roster: the Phase B impact model's projected-roster output blended 55/45 with last season's actual AdjEM (55% last year, 45% the model).",
      comparator: nullsLast,
      cellRenderer: (p: { value: number | null; data?: ProjectedTeam }) => {
        const chip = adjEmChip(p.value);
        const baseline = p.data?.baseline_adj_em;
        if (p.value == null || baseline == null) return chip;
        return (
          <span
            title={`55% last year's actual AdjEM (${baseline >= 0 ? '+' : ''}${baseline.toFixed(1)}) + 45% the Phase B model's projected-roster output`}
          >
            {chip}
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
      headerName: 'Returning',
      colId: 'returning',
      ...flexCol(1, 120),
      headerTooltip:
        'Returning players — total CamPom retained, summing each returner\'s prior-season production (excludes graduating seniors, outbound portal, and firm draft departures). Hover for the headcount.',
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.returning_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.returning_cam_v3_sum ?? 0),
      cellRenderer: camSumRenderer(
        'returning_cam_v3_sum',
        'returning_count',
        'base',
        'returning',
        'prior-season',
      ),
    },
    {
      headerName: 'Incoming transfers',
      colId: 'incoming',
      ...flexCol(1, 130),
      headerTooltip:
        'Incoming portal arrivals — total CamPom gained, summing each arrival\'s prior-school production. Hover for the headcount.',
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.arrivals_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.arrivals_cam_v3_sum ?? 0),
      cellRenderer: camSumRenderer(
        'arrivals_cam_v3_sum',
        'arrivals_count',
        'gain',
        'transfers in',
        'prior-season',
      ),
    },
    {
      headerName: 'Recruits',
      colId: 'recruits',
      ...flexCol(1, 130),
      headerTooltip:
        "Incoming HS recruits — total projected freshman-season CamPom from cstat's freshman-impact model (recruits have no prior season, so this is a forward projection). Hover for the top commits by composite rank.",
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.recruits_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.recruits_cam_v3_sum ?? 0),
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.recruits_count === 0) {
          return <span className="text-slate-600 text-xs">—</span>;
        }
        const sum = t.recruits_cam_v3_sum;
        const names = t.top_recruits
          .map((r) => `${r.composite_rank ? `#${r.composite_rank} ` : ''}${r.name} (${r.star_rating ?? '?'}★)`)
          .join('\n');
        const tooltip = `${t.recruits_count} recruits · Σ projected CamPom ${sum >= 0 ? '+' : ''}${sum.toFixed(1)}${names ? `\n${names}` : ''}`;
        return (
          <span className="text-xs font-mono font-semibold text-slate-300" title={tooltip}>
            {sum >= 0 ? `+${sum.toFixed(1)}` : sum.toFixed(1)}
          </span>
        );
      },
    },
    {
      headerName: 'Departures',
      colId: 'departures',
      ...flexCol(1, 130),
      headerTooltip:
        'Graduating seniors + outbound portal + firm draft departures — total CamPom leaving the program, summing each departure\'s prior-season production. Hover for the headcount.',
      comparator: (_a, _b, na, nb) =>
        ((na.data as ProjectedTeam | undefined)?.departures_cam_v3_sum ?? 0) -
        ((nb.data as ProjectedTeam | undefined)?.departures_cam_v3_sum ?? 0),
      cellRenderer: camSumRenderer(
        'departures_cam_v3_sum',
        'departures_count',
        'loss',
        'departures',
        'prior-season',
      ),
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

  const columns = useMemo(
    () => buildColumns(isMobile, year, hasActuals),
    [isMobile, year, hasActuals],
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
      <div className="rounded border border-amber-800/40 bg-amber-950/20 text-amber-200 text-xs p-3 mb-3 leading-relaxed">
        <strong className="text-amber-300">v3 honesty caveats:</strong>{' '}
        Holistic projection: returners (minus seniors, outbound portal,
        and firm draft departures) + incoming portal commits +{' '}
        <strong>incoming HS recruits</strong>. Every
        player is scored on a <strong>projected</strong> next-season
        CamPom v3 — the trajectory model for returners and arrivals, the
        freshman-impact model for recruits — so returner growth and
        freshman upside both count (a junior breaking out as a senior, or
        an elite recruit, moves the number). The roster's
        projected-CamPom distribution is scored by the Phase B
        impact-aggregation model, then blended <strong>55/45 with last
        season's actual AdjEM</strong> (no calibration offset — the model
        is near-unbiased). The pipeline backtests at <strong>5.88 AdjEM
        MAE</strong> against actual next-season results (2025 + 2026) —
        treat the ordering as <em>directional</em>, not point-estimates.
        Elite returners regress hard: the trajectory model under-projects
        the +15-and-up CamPom tail <em>by design</em> (it's calibrated on
        returners who stayed, and +20 is past its training range), so a
        reigning star projects below his current number. Rosters with
        &lt;7 qualifying players (returning + arrivals + recruits) are
        flagged "thin roster" and not scored.
      </div>
      {accuracy && (
        <div className="rounded border border-emerald-800/40 bg-emerald-950/20 text-emerald-200 text-xs p-3 mb-4 leading-relaxed">
          <strong className="text-emerald-300">Backtest receipt:</strong>{' '}
          this is the forecast we'd have made going into {seasonLabel(year)},
          graded against what actually happened. Across {accuracy.n} teams,
          mean absolute error <strong>{accuracy.mae.toFixed(1)} AdjEM</strong>,
          mean bias {accuracy.bias >= 0 ? '+' : ''}
          {accuracy.bias.toFixed(1)} (
          {accuracy.bias >= 0 ? 'over-projected on average' : 'under-projected on average'}
          ). See the <strong>Actual</strong> and <strong>Proj − Act</strong>{' '}
          columns per team.
        </div>
      )}
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
          height: 'calc(100dvh - 280px)',
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
    </div>
  );
}
