import { useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import {
  fetchCoaches,
  fetchCoachSeasonBoard,
  type CoachLeaderboardRow,
  type CoachSeasonLeaderboardRow,
} from '../api/client';
import { SortHeader, StickyHeader } from '../components/TableHeaders';
import { compareValues, type SortDir } from '../components/tableSort';
import { usePageTitle } from '../components/usePageTitle';
import { useSeason, setPageSeasons, type Season } from '../components/season';
import { caeColor, fmtCae } from '../components/cae';

type Mode = 'career' | 'season';

// Career mode shows every rated coach: `cae_shrunk` already pulls thin tenures
// toward 0 and the Rel. column flags low-confidence ratings, so an explicit
// min-seasons filter would be redundant.
const SHOW_ALL_MIN_SEASONS = 1;

const NEW_BADGE = (
  <span
    className="ml-1.5 text-[10px] px-1 py-0.5 rounded bg-amber-500/20 text-amber-300 border border-amber-500/40"
    title="First season at this team."
  >
    new
  </span>
);

/** Reliability shown as a thin bar + value, so a thin-tenure rating reads as
 *  low-confidence at a glance. reliability = n / (n + k) ∈ [0,1]. */
function ReliabilityBar({ value }: { value: number }) {
  return (
    <div className="inline-flex items-center gap-2 justify-end w-full">
      <div className="h-1.5 w-12 rounded bg-gray-700 overflow-hidden">
        <div className="h-full bg-blue-500" style={{ width: `${Math.round(value * 100)}%` }} />
      </div>
      <span className="text-[11px] tabular-nums text-gray-400 w-8">{value.toFixed(2)}</span>
    </div>
  );
}

function TeamCell({ teamId, teamName, season }: { teamId: string | null; teamName: string | null; season: number }) {
  if (teamId && teamName) {
    return (
      <Link to={`/teams/${teamId}?season=${season}`} className="hover:underline">
        {teamName}
      </Link>
    );
  }
  return <>{teamName ?? '—'}</>;
}

// --- Career board: ranked by career EB-shrunk CAE (season filters the list) ---

type CareerSortKey =
  | 'name'
  | 'cae_shrunk'
  | 'cae_adj_shrunk'
  | 'cae_centered_shrunk'
  | 'career_adj_em'
  | 'career_adj_o'
  | 'career_adj_d'
  | 'blend'
  | 'reliability'
  | 'n_seasons'
  | 'last_team_name';

/** Plain fixed-decimal for the display-only team-strength columns; `—` for the
 *  coaches whose scored seasons never resolved to a team-stats row. */
function fmtStrength(v: number | null, d = 1): string {
  return v == null ? '—' : v.toFixed(d);
}

function CareerTable({ rows }: { rows: CoachLeaderboardRow[] }) {
  const [sort, setSort] = useState<{ key: CareerSortKey; dir: SortDir }>({
    key: 'cae_shrunk',
    dir: 'desc',
  });
  const onSort = (key: CareerSortKey) =>
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'name' || key === 'last_team_name' ? 'asc' : 'desc' },
    );
  const sorted = useMemo(
    () => [...rows].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir)),
    [rows, sort],
  );

  return (
    <div className="overflow-x-auto">
      <table className="min-w-full text-sm whitespace-nowrap">
        <thead>
          <tr className="text-gray-400 border-b border-gray-700">
            <StickyHeader align="right" className="w-10">#</StickyHeader>
            <SortHeader label="Coach" sortKey="name" current={sort} onSort={onSort} />
            <SortHeader label="Team" sortKey="last_team_name" current={sort} onSort={onSort}
              title="Team coached in the selected season." />
            <SortHeader label="CAE" sortKey="cae_shrunk" current={sort} onSort={onSort} align="right"
              title="Shrunk Coach-Above-Expectation (AdjEM points above roster projection). The headline rating." />
            <StickyHeader align="right">95% CI</StickyHeader>
            <SortHeader label="Adj" sortKey="cae_adj_shrunk" current={sort} onSort={onSort} align="right"
              title="Prestige-adjusted CAE (projection-quartile-de-biased) — a conservative lower bound that strips the program component." />
            <SortHeader label="Era±" sortKey="cae_centered_shrunk" current={sort} onSort={onSort} align="right"
              title="Season-centered CAE — each season's mean residual removed for era-neutral COMPARISON between coaches. Use to rank coaches on equal footing across eras; it deliberately discards season-level signal, so it is not a 'how much' measure like the headline CAE." />
            <SortHeader label="AdjEM" sortKey="career_adj_em" current={sort} onSort={onSort} align="right"
              title="Career-mean team AdjEM (adjusted efficiency margin) across scored seasons — how strong the coach's teams actually were. Opponent-adjusted, so it already rewards hard schedules. Descriptive context, NOT an input to any projection." />
            <SortHeader label="AdjO" sortKey="career_adj_o" current={sort} onSort={onSort} align="right"
              title="Career-mean team adjusted offensive efficiency (points per 100 possessions, opponent-adjusted)." />
            <SortHeader label="AdjD" sortKey="career_adj_d" current={sort} onSort={onSort} align="right"
              title="Career-mean team adjusted defensive efficiency (points allowed per 100 possessions; lower is better)." />
            <SortHeader label="Blend" sortKey="blend" current={sort} onSort={onSort} align="right"
              title="Evaluative composite: z(CAE) + z(career AdjEM) over this board — rewards coaches who field strong, tough-schedule teams AND squeeze extra out of the roster. A lens for human comparison, not a rigorous metric, and never fed back into forecasts." />
            <SortHeader label="Rel." sortKey="reliability" current={sort} onSort={onSort} align="right"
              title="Reliability = n / (n + k). Shrinkage weight; low = thin tenure, treat the rating as soft." />
            <SortHeader label="Yrs" sortKey="n_seasons" current={sort} onSort={onSort} align="right"
              title="Scored seasons (bounded by roster-projection coverage, not career length)." />
          </tr>
        </thead>
        <tbody>
          {sorted.map((c, i) => (
            <tr key={c.coach_id} className="border-b border-gray-800 hover:bg-gray-800/50">
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-500">{i + 1}</td>
              <td className="py-1.5 px-2 font-medium">
                <Link to={`/coaches/${c.coach_id}`} className="hover:underline text-blue-300">
                  {c.name}
                </Link>
              </td>
              <td className="py-1.5 px-2 text-gray-300">
                <TeamCell teamId={c.last_team_id} teamName={c.last_team_name}
                  season={c.last_team_season ?? c.last_season} />
              </td>
              <td className="py-1.5 px-2 text-right tabular-nums font-semibold" style={{ color: caeColor(c.cae_shrunk) }}>
                {fmtCae(c.cae_shrunk)}
              </td>
              <td className="py-1.5 px-2 text-right tabular-nums text-[11px] text-gray-500">
                {fmtCae(c.ci_low)} … {fmtCae(c.ci_high)}
              </td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">{fmtCae(c.cae_adj_shrunk)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">{fmtCae(c.cae_centered_shrunk)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-300">{fmtStrength(c.career_adj_em)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">{fmtStrength(c.career_adj_o)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">{fmtStrength(c.career_adj_d)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums font-medium" style={{ color: caeColor(c.blend) }}>
                {c.blend == null ? '—' : fmtCae(c.blend, 2)}
              </td>
              <td className="py-1.5 px-2"><ReliabilityBar value={c.reliability} /></td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-300">{c.n_seasons}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {sorted.length === 0 && (
        <div className="text-gray-500 py-6 text-center">No coaches match this filter.</div>
      )}
    </div>
  );
}

// --- Season board: ranked by the selected year's single-season raw CAE ---

type SeasonSortKey = 'name' | 'team_name' | 'actual_adjem' | 'projection' | 'cae_raw';

function SeasonTable({ rows }: { rows: CoachSeasonLeaderboardRow[] }) {
  const [sort, setSort] = useState<{ key: SeasonSortKey; dir: SortDir }>({
    key: 'cae_raw',
    dir: 'desc',
  });
  const onSort = (key: SeasonSortKey) =>
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'name' || key === 'team_name' ? 'asc' : 'desc' },
    );
  const sorted = useMemo(
    () => [...rows].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir)),
    [rows, sort],
  );

  return (
    <div className="overflow-x-auto">
      <table className="min-w-full text-sm whitespace-nowrap">
        <thead>
          <tr className="text-gray-400 border-b border-gray-700">
            <StickyHeader align="right" className="w-10">#</StickyHeader>
            <SortHeader label="Coach" sortKey="name" current={sort} onSort={onSort} />
            <SortHeader label="Team" sortKey="team_name" current={sort} onSort={onSort} />
            <SortHeader label="Actual" sortKey="actual_adjem" current={sort} onSort={onSort} align="right"
              title="The team's actual AdjEM that season." />
            <SortHeader label="Proj" sortKey="projection" current={sort} onSort={onSort} align="right"
              title="Roster-only projected AdjEM (what the talent on hand was worth)." />
            <SortHeader label="CAE" sortKey="cae_raw" current={sort} onSort={onSort} align="right"
              title="Single-season Coach-Above-Expectation = actual − projection. Noisy; the career view shrinks this." />
          </tr>
        </thead>
        <tbody>
          {sorted.map((c, i) => (
            <tr key={c.coach_id} className="border-b border-gray-800 hover:bg-gray-800/50">
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-500">{i + 1}</td>
              <td className="py-1.5 px-2 font-medium">
                <Link to={`/coaches/${c.coach_id}`} className="hover:underline text-blue-300">
                  {c.name}
                </Link>
              </td>
              <td className="py-1.5 px-2 text-gray-300">
                <TeamCell teamId={c.team_id} teamName={c.team_name} season={c.season} />
                {c.is_new_hc && NEW_BADGE}
              </td>
              <td className="py-1.5 px-2 text-right tabular-nums">{c.actual_adjem.toFixed(1)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">{c.projection.toFixed(1)}</td>
              <td className="py-1.5 px-2 text-right tabular-nums font-semibold" style={{ color: caeColor(c.cae_raw) }}>
                {fmtCae(c.cae_raw)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {sorted.length === 0 && (
        <div className="text-gray-500 py-6 text-center">No CAE data for this season.</div>
      )}
    </div>
  );
}

export function Coaches() {
  usePageTitle('Coaches');
  const { season, setSeason } = useSeason();
  const [mode, setMode] = useState<Mode>('career');
  const [board, setBoard] = useState<
    | { mode: 'career'; rows: CoachLeaderboardRow[] }
    | { mode: 'season'; rows: CoachSeasonLeaderboardRow[] }
    | null
  >(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // No synchronous `setLoading(true)` — initial `loading` covers first paint;
  // a mode/filter/season change keeps the prior board visible until the new one
  // lands (project convention; see Rankings.tsx). Career mode ranks by the
  // season-independent shrunk rating (season filters the list); season mode
  // re-ranks by the selected year's single-season residual.
  useEffect(() => {
    let cancelled = false;
    const applyMeta = (available: number[]) => {
      // Constrain the navbar picker to CAE coverage (2022–2026). Snap an
      // out-of-coverage season (e.g. a stale ?season= from another page) to the
      // newest covered year so the picker and board stay consistent.
      setPageSeasons(available);
      if (available.length > 0 && !available.includes(season)) {
        setSeason(available[0] as Season);
      }
    };
    const req =
      mode === 'season'
        ? fetchCoachSeasonBoard(season).then((res) => {
            if (cancelled) return;
            setBoard({ mode: 'season', rows: res.coaches });
            setError(null);
            applyMeta(res.available_seasons);
          })
        : fetchCoaches({ minSeasons: SHOW_ALL_MIN_SEASONS, season }).then((res) => {
            if (cancelled) return;
            setBoard({ mode: 'career', rows: res.coaches });
            setError(null);
            applyMeta(res.available_seasons);
          });
    req
      .catch((e) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [mode, season, setSeason]);

  // Release the season-selector override on unmount so the dropdown returns to
  // the global list when navigating away.
  useEffect(() => () => setPageSeasons(null), []);

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="text-3xl font-bold">Coaches</h1>
          <p className="text-sm text-gray-400 mt-1 max-w-2xl">
            <span className="font-semibold text-gray-300">Coach-Above-Expectation (CAE)</span> — how
            much a team out- or under-performs the talent on its roster, attributed to the coach. A
            descriptive grade, not a prediction. Positive = beat the roster projection.
          </p>
          {mode === 'career' ? (
            <p className="text-xs text-gray-500 mt-1 max-w-2xl">
              Coaches active in <span className="text-gray-300 font-semibold">{season}</span>, ranked
              by their <span className="text-gray-300">career</span> CAE (shrunk over tenure, so thin
              tenures pull toward 0 — see the Rel. column for confidence). Changing the season swaps
              which coaches show and their team; the ranking is career-wide. Coverage 2016–2026. The{' '}
              <span className="text-gray-300">AdjEM/AdjO/AdjD</span> columns show how strong the
              coach's teams actually were (descriptive context, not a projection input); sort by{' '}
              <span className="text-gray-300">Blend</span> for a "results + overperformance" view.
            </p>
          ) : (
            <p className="text-xs text-gray-500 mt-1 max-w-2xl">
              Each coach's <span className="text-gray-300 font-semibold">{season}</span> single-season
              CAE — who beat their roster the most that year. Single seasons are noisy (the career
              view shrinks them); treat this as a snapshot, not a rating.
            </p>
          )}
        </div>
        {/* Career / Season ranking toggle — pinned top-right in both modes.
            `shrink-0` + the left column's `min-w-0 flex-1` keep it fixed even
            as the caption length changes between modes. */}
        <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs self-start shrink-0">
          {(['career', 'season'] as const).map((m) => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={`px-3 py-1.5 capitalize ${
                mode === m ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
              }`}
            >
              {m}
            </button>
          ))}
        </div>
      </div>

      {error && <div className="text-red-400">{error}</div>}
      {loading && !board ? (
        <div className="text-gray-400">Loading…</div>
      ) : board?.mode === 'season' ? (
        <SeasonTable rows={board.rows} />
      ) : board ? (
        <CareerTable rows={board.rows} />
      ) : null}
    </div>
  );
}

export default Coaches;
