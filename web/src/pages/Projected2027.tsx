import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchProjections, type ProjectedTeam } from '../api/client';
import { gridTheme } from '../theme';
import { SeasonLink } from '../components/SeasonLink';
import { useIsMobile } from '../components/useIsMobile';

// AdjEM tier coloring (tuned for D-I 2025 distribution where teams
// range ~-30 to +45). Reused for floor/ceiling/midpoint chips.
function adjEmTone(v: number): string {
  if (v >= 25) return 'bg-emerald-900/50 border-emerald-700 text-emerald-200';
  if (v >= 15) return 'bg-emerald-950/40 border-emerald-800 text-emerald-300';
  if (v >= 5) return 'bg-teal-950/40 border-teal-800 text-teal-300';
  if (v >= -5) return 'bg-slate-800/40 border-slate-700 text-slate-300';
  if (v >= -15) return 'bg-amber-950/40 border-amber-800 text-amber-300';
  return 'bg-rose-950/40 border-rose-800 text-rose-300';
}

const adjEmChip = (v: number | null) => {
  if (v == null)
    return <span className="text-slate-600 text-xs">—</span>;
  return (
    <span className={`px-1.5 rounded border text-xs font-semibold ${adjEmTone(v)}`}>
      {v >= 0 ? `+${v.toFixed(1)}` : v.toFixed(1)}
    </span>
  );
};

// Render the floor → ceiling band as a small horizontal bar. Width
// proportional to (ceiling - floor); flagged with a tooltip when the
// spread is negative (declared cohort is a net drag per the model —
// counterintuitive but a real signal worth surfacing per ROADMAP §5b
// spot-check notes).
function bandRenderer(p: { data?: ProjectedTeam }) {
  const t = p.data;
  if (!t) return null;
  if (t.too_thin) {
    return (
      <span className="text-slate-500 text-xs italic" title="Roster too thin to project — <7 qualifying players (returning + arrivals + recruits). The model over-weights rate stats when too few players carry them. See honesty banner.">
        thin roster
      </span>
    );
  }
  const f = t.floor_adj_em;
  const c = t.ceiling_adj_em;
  if (f == null || c == null) return <span className="text-slate-600">—</span>;
  const spread = c - f;
  const isNegative = spread < -0.1;
  // Render a thin range bar. Spread <= 0.1 collapses to a single dot.
  const collapsed = Math.abs(spread) < 0.1;
  return (
    <div
      className="flex items-center gap-2"
      title={
        isNegative
          ? `Declared cohort is a net drag per the model (floor ${f.toFixed(1)} > ceiling ${c.toFixed(1)}). Probably means the declared players have box-score profiles the model penalizes; surface as-is rather than hiding.`
          : `Floor ${f.toFixed(1)} → Ceiling ${c.toFixed(1)} (range = ${spread.toFixed(1)} AdjEM)`
      }
    >
      <span className={`text-xs font-mono ${isNegative ? 'text-amber-400' : 'text-slate-400'}`}>
        {f.toFixed(1)}
      </span>
      <span
        className={`inline-block h-1 rounded ${collapsed ? 'w-1' : 'w-12'} ${isNegative ? 'bg-amber-700' : 'bg-slate-600'}`}
      />
      <span className={`text-xs font-mono ${isNegative ? 'text-amber-400' : 'text-slate-400'}`}>
        {c.toFixed(1)}
      </span>
    </div>
  );
}

function buildColumns(isMobile: boolean): ColDef<ProjectedTeam>[] {
  const flexCol = (flex: number, min: number) =>
    isMobile ? { width: min } : { flex, minWidth: min };
  return [
    {
      headerName: 'Rank',
      colId: 'rank',
      width: 60,
      pinned: 'left',
      sortable: false,
      valueGetter: (p) =>
        p.node && typeof p.node.rowIndex === 'number'
          ? p.node.rowIndex + 1
          : null,
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
        // Link to the projected 2027 team page so users land on the
        // forward-looking roster view (returners + transfers +
        // recruits), not the played-2026 page. `season=2027` triggers
        // TeamDetail's projection-mode branch.
        <SeasonLink
          to={`/teams/${p.data?.team_id}?season=2027`}
          onClick={(e) => e.stopPropagation()}
          className="text-blue-400 hover:underline"
        >
          {p.value}
        </SeasonLink>
      ),
    },
    {
      headerName: 'Mid AdjEM',
      field: 'midpoint_adj_em',
      ...flexCol(1, 100),
      sort: 'desc',
      headerTooltip:
        'Probability-weighted blend of the floor and ceiling bounds (by the declared-draft cohort\'s mock-draft-implied odds of returning), then blended 55/45 with last year\'s actual AdjEM (45% the Phase B impact model\'s projected-roster output, 55% last year).',
      // AG Grid's sort engine applies the comparator in ascending order
      // and then reverses for descending. So to anchor nulls (thin
      // rosters) to the visual bottom regardless of direction, we flip
      // the null-vs-non-null result based on `isDescending`.
      comparator: (
        a: number | null,
        b: number | null,
        _na: unknown,
        _nb: unknown,
        isDescending: boolean,
      ) => {
        if (a == null && b == null) return 0;
        if (a == null) return isDescending ? -1 : 1;
        if (b == null) return isDescending ? 1 : -1;
        return a - b;
      },
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
    {
      headerName: 'Δ vs last',
      colId: 'delta_baseline',
      ...flexCol(1, 90),
      headerTooltip:
        'Projected midpoint minus last season\'s actual AdjEM. Positive (green) = the model thinks this roster improves on last year. Negative (red) = regression. Null when we lack a baseline (new D-I) or the projection is gated.',
      valueGetter: (p) => {
        const t = p.data;
        if (!t || t.midpoint_adj_em == null || t.baseline_adj_em == null) return null;
        return t.midpoint_adj_em - t.baseline_adj_em;
      },
      comparator: (
        a: number | null,
        b: number | null,
        _na: unknown,
        _nb: unknown,
        isDescending: boolean,
      ) => {
        if (a == null && b == null) return 0;
        if (a == null) return isDescending ? -1 : 1;
        if (b == null) return isDescending ? 1 : -1;
        return a - b;
      },
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
      headerName: 'Floor ↔ Ceiling',
      colId: 'band',
      ...flexCol(2, 160),
      sortable: false,
      headerTooltip:
        'Floor = AdjEM if every declared NBA-draft player is gone. Ceiling = if they all withdraw and return. Spread is roughly proportional to the count of uncertain players.',
      cellRenderer: bandRenderer,
    },
    {
      headerName: 'Ret',
      field: 'returning_count',
      ...flexCol(1, 60),
      headerTooltip:
        'Returning players (excludes Sr, outbound portal, firm draft departures, declared draft cohort)',
      cellRenderer: (p: { value: number }) => (
        <span className="text-slate-300 text-xs">{p.value}</span>
      ),
    },
    {
      headerName: 'Arr',
      field: 'arrivals_count',
      ...flexCol(1, 60),
      headerTooltip: 'Incoming portal arrivals committed to this team',
      cellRenderer: (p: { value: number }) => (
        <span className="text-emerald-400 text-xs">{p.value > 0 ? `+${p.value}` : '0'}</span>
      ),
    },
    {
      headerName: 'Rec',
      field: 'recruits_count',
      ...flexCol(2, 140),
      headerTooltip:
        "Incoming HS recruits committed to this team. Per-tier breakdown by cstat's freshman-impact model: each recruit is reassigned to T1 (≈+9 projected CamPom) / T2 (≈+2.4) / T3 (≈+0.7) / T4 (≈-0.6) based on which tier centroid is closest to their model-predicted freshman CamPom — so a 247-T3 recruit at a top-tier program with a strong peer class can move up to T2, and a 247-T1 at a thin program can move down. The synthesised PlayerRow uses the chosen tier's per-game profile; the recruit's continuous prediction surfaces on the Recruits tab.",
      // Sort by count of T1 + T2 commits (the impactful end of the
      // class) rather than total count — Florida with 0 commits should
      // sort below a team with 1 elite recruit and 4 walk-ons.
      comparator: (_a, _b, na, nb) => {
        const a = na.data as ProjectedTeam | undefined;
        const b = nb.data as ProjectedTeam | undefined;
        const wa = (a?.recruits_by_tier?.t1 ?? 0) * 3 + (a?.recruits_by_tier?.t2 ?? 0);
        const wb = (b?.recruits_by_tier?.t1 ?? 0) * 3 + (b?.recruits_by_tier?.t2 ?? 0);
        return wa - wb;
      },
      cellRenderer: (p: { data?: ProjectedTeam }) => {
        const t = p.data;
        if (!t || t.recruits_count === 0) {
          return <span className="text-slate-600 text-xs">—</span>;
        }
        const by = t.recruits_by_tier;
        const top = t.top_recruits;
        const tooltip = top.length
          ? top
              .map((r) => `${r.composite_rank ? `#${r.composite_rank} ` : ''}${r.name} (${r.star_rating ?? '?'}★)`)
              .join('\n')
          : 'recruits';
        return (
          <span className="inline-flex items-center gap-1 text-xs" title={tooltip}>
            {by.t1 > 0 && (
              <span className="px-1.5 py-0.5 rounded border border-amber-700/60 bg-amber-900/30 text-amber-300">
                {by.t1}×T1
              </span>
            )}
            {by.t2 > 0 && (
              <span className="px-1.5 py-0.5 rounded border border-cyan-700/60 bg-cyan-900/30 text-cyan-300">
                {by.t2}×T2
              </span>
            )}
            {by.t3 > 0 && (
              <span className="text-slate-400">{by.t3}×T3</span>
            )}
            {by.t4 > 0 && (
              <span className="text-slate-500">{by.t4}×T4</span>
            )}
          </span>
        );
      },
    },
    {
      headerName: 'Unc',
      field: 'uncertain_count',
      ...flexCol(1, 60),
      headerTooltip:
        "Declared NBA-draft entrants with status still pending (treated as 'gone' in floor, 'staying' in ceiling)",
      cellRenderer: (p: { value: number }) =>
        p.value > 0 ? (
          <span className="text-amber-400 text-xs">?{p.value}</span>
        ) : (
          <span className="text-slate-600 text-xs">—</span>
        ),
    },
    {
      headerName: 'Dep',
      field: 'departures_count',
      ...flexCol(1, 60),
      headerTooltip: 'Sr graduations + outbound portal + firm draft departures',
      cellRenderer: (p: { value: number }) => (
        <span className="text-rose-400 text-xs">−{p.value}</span>
      ),
    },
  ];
}

export default function Projected2027() {
  const [teams, setTeams] = useState<ProjectedTeam[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [baseSeason, setBaseSeason] = useState<number | null>(null);
  const [search, setSearch] = useState('');
  const isMobile = useIsMobile();

  useEffect(() => {
    let canceled = false;
    fetchProjections(2027)
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
  }, []);

  const columns = useMemo(() => buildColumns(isMobile), [isMobile]);

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

  if (error) {
    return (
      <div className="p-4 text-rose-300">
        Failed to load projections: {error}
      </div>
    );
  }

  const scoredCount = teams?.filter((t) => !t.too_thin).length ?? 0;
  const thinCount = teams?.filter((t) => t.too_thin).length ?? 0;

  return (
    <div className="p-4">
      <h1 className="text-2xl font-bold mb-2">Projected 2026-27 (v3)</h1>
      <div className="rounded border border-amber-800/40 bg-amber-950/20 text-amber-200 text-xs p-3 mb-4 leading-relaxed">
        <strong className="text-amber-300">v3 honesty caveats:</strong>{' '}
        Holistic projection: returners (minus seniors, outbound portal,
        firm draft departures, declared-draft `?` cohort) + incoming
        portal commits + <strong>incoming HS recruits</strong>. Every
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
      <div className="flex items-center gap-3 mb-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search teams…"
          className="px-2 py-1 text-sm bg-gray-800 border border-gray-700 rounded text-gray-200 placeholder:text-gray-500 w-64"
        />
        <span className="text-xs text-gray-500">
          {scoredCount} teams scored · {thinCount} flagged thin roster ·{' '}
          based on 2025-26 → projecting 2026-27
          {baseSeason && baseSeason !== 2026 && ` (base season ${baseSeason})`}
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
