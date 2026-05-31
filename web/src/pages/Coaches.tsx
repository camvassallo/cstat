import { useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { fetchCoaches, type CoachLeaderboardRow } from '../api/client';
import { SortHeader } from '../components/TableHeaders';
import { compareValues, type SortDir } from '../components/tableSort';
import { usePageTitle } from '../components/usePageTitle';
import { caeColor, fmtCae } from '../components/cae';

type SortKey =
  | 'name'
  | 'cae_shrunk'
  | 'cae_adj_shrunk'
  | 'reliability'
  | 'n_seasons'
  | 'last_team_name';

// Minimum-seasons options. Default 3 (locked in PR3 scope): thin tenures shrink
// toward 0 and would otherwise top the board on noise. 1 shows everyone (lean
// on the credibility band); 5 keeps only well-established coaches.
const MIN_SEASONS_OPTIONS = [1, 3, 5] as const;

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

export function Coaches() {
  usePageTitle('Coaches');
  const [minSeasons, setMinSeasons] = useState(3);
  const [rows, setRows] = useState<CoachLeaderboardRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({
    key: 'cae_shrunk',
    dir: 'desc',
  });

  // No synchronous `setLoading(true)` here — initial `loading` covers the
  // first paint, and a filter change keeps the prior rows visible until the
  // new ones land (project convention; see Rankings.tsx).
  useEffect(() => {
    let cancelled = false;
    fetchCoaches({ minSeasons })
      .then((res) => {
        if (cancelled) return;
        setRows(res.coaches);
        setError(null);
      })
      .catch((e) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [minSeasons]);

  const onSort = (key: SortKey) => {
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'name' || key === 'last_team_name' ? 'asc' : 'desc' },
    );
  };

  const sorted = useMemo(
    () => [...rows].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir)),
    [rows, sort],
  );

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between flex-wrap gap-3">
        <div>
          <h1 className="text-3xl font-bold">Coaches</h1>
          <p className="text-sm text-gray-400 mt-1 max-w-2xl">
            <span className="font-semibold text-gray-300">Coach-Above-Expectation (CAE)</span> —
            how much a team out- or under-performs the talent on its roster, attributed to the
            coach and averaged over their tenure with shrinkage. A descriptive grade, not a
            prediction. Positive = beat the roster projection.
          </p>
        </div>
        <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs self-start">
          {MIN_SEASONS_OPTIONS.map((n) => (
            <button
              key={n}
              onClick={() => setMinSeasons(n)}
              className={`px-3 py-1.5 ${
                minSeasons === n
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
              }`}
            >
              ≥{n} {n === 1 ? 'season' : 'seasons'}
            </button>
          ))}
        </div>
      </div>

      {error && <div className="text-red-400">{error}</div>}
      {loading ? (
        <div className="text-gray-400">Loading…</div>
      ) : (
        <div className="overflow-x-auto">
          <table className="min-w-full text-sm whitespace-nowrap">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700">
                <th className="sticky top-0 z-10 bg-gray-900 py-2 px-2 text-right w-10">#</th>
                <SortHeader label="Coach" sortKey="name" current={sort} onSort={onSort} />
                <SortHeader
                  label="Team"
                  sortKey="last_team_name"
                  current={sort}
                  onSort={onSort}
                  title="Most recent team coached."
                />
                <SortHeader
                  label="CAE"
                  sortKey="cae_shrunk"
                  current={sort}
                  onSort={onSort}
                  align="right"
                  title="Shrunk Coach-Above-Expectation (AdjEM points above roster projection). The headline rating."
                />
                <StickyRight>95% CI</StickyRight>
                <SortHeader
                  label="Adj"
                  sortKey="cae_adj_shrunk"
                  current={sort}
                  onSort={onSort}
                  align="right"
                  title="Prestige-adjusted CAE (projection-quartile-de-biased) — a conservative lower bound that strips the program component."
                />
                <SortHeader
                  label="Rel."
                  sortKey="reliability"
                  current={sort}
                  onSort={onSort}
                  align="right"
                  title="Reliability = n / (n + k). Shrinkage weight; low = thin tenure, treat the rating as soft."
                />
                <SortHeader
                  label="Yrs"
                  sortKey="n_seasons"
                  current={sort}
                  onSort={onSort}
                  align="right"
                  title="Scored seasons (bounded by roster-projection coverage, not career length)."
                />
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
                    {c.last_team_id && c.last_team_name ? (
                      <Link
                        to={`/teams/${c.last_team_id}?season=${c.last_season}`}
                        className="hover:underline"
                      >
                        {c.last_team_name}
                      </Link>
                    ) : (
                      c.last_team_name ?? '—'
                    )}
                  </td>
                  <td className="py-1.5 px-2 text-right tabular-nums font-semibold"
                    style={{ color: caeColor(c.cae_shrunk) }}>
                    {fmtCae(c.cae_shrunk)}
                  </td>
                  <td className="py-1.5 px-2 text-right tabular-nums text-[11px] text-gray-500">
                    {fmtCae(c.ci_low)} … {fmtCae(c.ci_high)}
                  </td>
                  <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">
                    {fmtCae(c.cae_adj_shrunk)}
                  </td>
                  <td className="py-1.5 px-2">
                    <ReliabilityBar value={c.reliability} />
                  </td>
                  <td className="py-1.5 px-2 text-right tabular-nums text-gray-300">
                    {c.n_seasons}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {sorted.length === 0 && (
            <div className="text-gray-500 py-6 text-center">No coaches match this filter.</div>
          )}
        </div>
      )}
    </div>
  );
}

function StickyRight({ children }: { children: React.ReactNode }) {
  return (
    <th className="sticky top-0 z-10 bg-gray-900 py-2 px-2 text-right text-gray-400">{children}</th>
  );
}

export default Coaches;
