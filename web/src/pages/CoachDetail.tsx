import { useEffect, useMemo, useState } from 'react';
import { Link, useParams } from 'react-router-dom';
import {
  fetchCoachDetail,
  type CoachRating,
  type CoachSeasonRow,
} from '../api/client';
import { usePageTitle } from '../components/usePageTitle';
import { caeColor, fmtCae, tenureSpan } from '../components/cae';

/** Per-season CAE sparkline: the raw residual (actual − projection) over time,
 *  with a zero baseline. Points tinted by sign; this is the visual the detail
 *  page leads with. Values are pre-stored, so no inference runs here. */
function Sparkline({ seasons }: { seasons: CoachSeasonRow[] }) {
  const W = 520;
  const H = 120;
  const PAD = 24;

  const { xy, zeroY } = useMemo(() => {
    // Graded seasons only — ungraded (no projection → null cae_raw) teams have
    // no residual to plot.
    const pts = seasons
      .filter((s): s is CoachSeasonRow & { cae_raw: number } => s.cae_raw != null)
      .map((s) => ({ season: s.season, v: s.cae_raw }));
    if (pts.length === 0) return { xy: [], zeroY: H / 2 };
    const vs = pts.map((p) => p.v);
    const lo = Math.min(0, ...vs);
    const hi = Math.max(0, ...vs);
    const span = hi - lo || 1;
    const x = (i: number) =>
      pts.length === 1 ? W / 2 : PAD + (i * (W - 2 * PAD)) / (pts.length - 1);
    const y = (v: number) => H - PAD - ((v - lo) / span) * (H - 2 * PAD);
    return {
      xy: pts.map((p, i) => ({ ...p, x: x(i), y: y(p.v) })),
      zeroY: y(0),
    };
  }, [seasons]);

  if (xy.length === 0) return null;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="w-full" role="img" aria-label="CAE by season">
      {/* zero baseline */}
      <line x1={PAD} y1={zeroY} x2={W - PAD} y2={zeroY} stroke="#374151" strokeWidth={1} strokeDasharray="3 3" />
      {/* connecting path */}
      {xy.length > 1 && (
        <polyline
          fill="none"
          stroke="#4b5563"
          strokeWidth={1.5}
          points={xy.map((p) => `${p.x},${p.y}`).join(' ')}
        />
      )}
      {xy.map((p) => (
        <g key={p.season}>
          <line x1={p.x} y1={zeroY} x2={p.x} y2={p.y} stroke={caeColor(p.v)} strokeWidth={1} opacity={0.4} />
          <circle cx={p.x} cy={p.y} r={4} fill={caeColor(p.v)} />
          <text x={p.x} y={H - 6} textAnchor="middle" className="fill-gray-500" fontSize={10}>
            {String(p.season).slice(2)}
          </text>
        </g>
      ))}
    </svg>
  );
}

function Stat({ label, value, color, title }: { label: string; value: string; color?: string; title?: string }) {
  return (
    <div className="bg-gray-800 rounded-lg p-4 text-center" title={title}>
      <div className="text-xs text-gray-400 uppercase tracking-wide mb-1">{label}</div>
      <div className="text-2xl font-bold tabular-nums" style={color ? { color } : undefined}>
        {value}
      </div>
    </div>
  );
}

export function CoachDetail() {
  const { id } = useParams<{ id: string }>();
  const [name, setName] = useState<string | null>(null);
  const [rating, setRating] = useState<CoachRating | null>(null);
  const [seasons, setSeasons] = useState<CoachSeasonRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  usePageTitle(name ?? 'Coach');

  // No synchronous `setLoading(true)` — initial `loading` covers first paint
  // (project convention; see Rankings.tsx / PlayerDetail.tsx).
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    fetchCoachDetail(id)
      .then((res) => {
        if (cancelled) return;
        setName(res.name);
        setRating(res.rating);
        setSeasons(res.seasons);
        setError(null);
      })
      .catch((e) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [id]);

  if (loading) return <div className="text-gray-400">Loading…</div>;
  if (error) return <div className="text-red-400">{error}</div>;

  // Tenure + coverage from the full season list (includes ungraded seasons the
  // roster projection dropped), so the span reflects actual coaching, not just
  // the scored backtest. `seasons` arrives season-ascending from the API.
  const scoredCount = seasons.filter((s) => s.cae_raw != null).length;
  const tenure =
    seasons.length > 0
      ? tenureSpan(seasons[0].season, seasons[seasons.length - 1].season)
      : null;

  return (
    <div className="space-y-6">
      <div>
        <Link to="/coaches" className="text-sm text-blue-300 hover:underline">
          ← Coaches
        </Link>
        <h1 className="text-3xl font-bold mt-1">{name ?? '(coach)'}</h1>
        {tenure && (
          <div className="text-gray-400">
            {tenure} · {scoredCount} of {seasons.length}{' '}
            {seasons.length === 1 ? 'season' : 'seasons'} scored
          </div>
        )}
      </div>

      {rating ? (
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
          <Stat
            label="CAE"
            value={fmtCae(rating.cae_shrunk)}
            color={caeColor(rating.cae_shrunk)}
            title="Shrunk Coach-Above-Expectation — the headline rating, in AdjEM points above roster projection."
          />
          <Stat
            label="95% CI"
            value={`${fmtCae(rating.ci_low)} – ${fmtCae(rating.ci_high)}`}
            title="Credibility interval on the shrunk rating."
          />
          <Stat
            label="Reliability"
            value={rating.reliability.toFixed(2)}
            title="n / (n + k). Low = thin tenure; treat the rating as soft."
          />
          <Stat
            label="Prestige-adj"
            value={fmtCae(rating.cae_adj_shrunk)}
            title="Projection-quartile-de-biased CAE — a conservative lower bound that strips the program component."
          />
        </div>
      ) : (
        <div className="bg-gray-800 rounded-lg p-4 text-sm text-gray-400">
          No career rating — this coach didn't land in the scored roster-projection backtest.
        </div>
      )}

      {/* Career team strength — descriptive context (how strong the coach's
          teams actually were), explicitly NOT a CAE component or projection
          input. Only rendered when the scored seasons resolved to team stats. */}
      {rating && rating.career_adj_em != null && (
        <div>
          <div className="text-xs text-gray-500 uppercase tracking-wide mb-2">
            Career team strength{' '}
            <span className="normal-case tracking-normal text-gray-600">
              · descriptive context, not part of CAE
            </span>
          </div>
          <div className="grid grid-cols-3 gap-3">
            <Stat
              label="Team AdjEM"
              value={rating.career_adj_em.toFixed(1)}
              title="Career-mean adjusted efficiency margin of the coach's teams. Opponent-adjusted, so it already accounts for schedule strength."
            />
            <Stat
              label="Team AdjO"
              value={rating.career_adj_o != null ? rating.career_adj_o.toFixed(1) : '—'}
              title="Career-mean adjusted offensive efficiency (points per 100 possessions)."
            />
            <Stat
              label="Team AdjD"
              value={rating.career_adj_d != null ? rating.career_adj_d.toFixed(1) : '—'}
              title="Career-mean adjusted defensive efficiency (points allowed per 100 possessions; lower is better)."
            />
          </div>
        </div>
      )}

      {seasons.length > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <h2 className="text-lg font-bold mb-1">Above expectation by season</h2>
          <p className="text-xs text-gray-500 mb-3">
            Actual team AdjEM minus the roster-only projection. Positive bars = the team beat the
            talent on hand. Single seasons are noisy; the headline rating shrinks the average toward
            zero.
          </p>
          <Sparkline seasons={seasons} />

          <div className="overflow-x-auto mt-4">
            <table className="min-w-full text-sm whitespace-nowrap">
              <thead>
                <tr className="text-gray-400 border-b border-gray-700 text-left">
                  <th className="py-2 px-2">Season</th>
                  <th className="py-2 px-2">Team</th>
                  <th className="py-2 px-2 text-right" title="The team's actual AdjEM that season.">
                    AdjEM
                  </th>
                  <th className="py-2 px-2 text-right" title="Team adjusted offensive efficiency that season.">
                    AdjO
                  </th>
                  <th className="py-2 px-2 text-right" title="Team adjusted defensive efficiency that season (lower is better).">
                    AdjD
                  </th>
                  <th className="py-2 px-2 text-right">Projected</th>
                  <th className="py-2 px-2 text-right" title="Actual − projected (raw CAE).">
                    CAE
                  </th>
                </tr>
              </thead>
              <tbody>
                {seasons.map((s) => {
                  // Ungraded ⇔ no projection (roster too thin to score). The
                  // team + actual strength still show; projection/CAE read
                  // "not scored" so the gap is legible as coverage, not error.
                  const graded = s.cae_raw != null;
                  return (
                    <tr key={s.season} className="border-b border-gray-800">
                      <td className="py-1.5 px-2 tabular-nums">
                        {s.season}
                        {s.is_new_hc && (
                          <span
                            className="ml-1.5 text-[10px] px-1 py-0.5 rounded bg-amber-500/20 text-amber-300 border border-amber-500/40"
                            title="First season at this team."
                          >
                            new
                          </span>
                        )}
                      </td>
                      <td className="py-1.5 px-2 text-gray-300">
                        {s.team_id && s.team_name ? (
                          <Link
                            to={`/teams/${s.team_id}?season=${s.season}`}
                            className="hover:underline"
                          >
                            {s.team_name}
                          </Link>
                        ) : (
                          s.team_name ?? '—'
                        )}
                      </td>
                      <td className="py-1.5 px-2 text-right tabular-nums">
                        {s.actual_adjem != null ? s.actual_adjem.toFixed(1) : '—'}
                      </td>
                      <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">
                        {s.adj_offense != null ? s.adj_offense.toFixed(1) : '—'}
                      </td>
                      <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">
                        {s.adj_defense != null ? s.adj_defense.toFixed(1) : '—'}
                      </td>
                      {graded ? (
                        <>
                          <td className="py-1.5 px-2 text-right tabular-nums text-gray-400">
                            {s.projection!.toFixed(1)}
                          </td>
                          <td
                            className="py-1.5 px-2 text-right tabular-nums font-semibold"
                            style={{ color: caeColor(s.cae_raw) }}
                          >
                            {fmtCae(s.cae_raw)}
                          </td>
                        </>
                      ) : (
                        <td
                          className="py-1.5 px-2 text-right text-gray-500"
                          colSpan={2}
                          title="The roster projection dropped this team-season (too few prior-D-I players — a heavy portal/JUCO rebuild), so there's no expectation to grade against."
                        >
                          not scored
                        </td>
                      )}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

export default CoachDetail;
