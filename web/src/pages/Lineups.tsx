import { useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { fetchLineupRankings, type LineupRanking } from '../api/client';
import { SortHeader, StickyHeader } from '../components/TableHeaders';
import { compareValues, type SortDir } from '../components/tableSort';
import { classColor, classTitle, textOnClass } from '../components/archetypeColors';
import { SeasonLink } from '../components/SeasonLink';
import { usePageTitle } from '../components/usePageTitle';
import { useSeason } from '../components/season';

type Size = 2 | 3 | 5;

const SIZE_LABEL: Record<Size, string> = { 5: '5-man', 3: 'Trios', 2: 'Duos' };

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const signed = (v: number | null, d = 1) =>
  v == null ? '—' : `${v > 0 ? '+' : ''}${v.toFixed(d)}`;
const netColor = (v: number | null) =>
  v == null
    ? 'text-gray-400'
    : v > 0
      ? 'text-green-400'
      : v < 0
        ? 'text-red-400'
        : 'text-gray-300';

/** Per-row breakdown of the adjusted net (AdjEM), for its hover tooltip. */
function adjTitle(r: LineupRanking): string {
  if (r.adj_net == null) return 'Opponent-adjusted rating unavailable for this team/season.';
  const raw = r.net_rtg == null ? '—' : signed(r.net_rtg);
  const sch = signed(r.adj_net - (r.net_rtg ?? 0));
  return `Raw on-court net ${raw} · schedule adjustment ${sch} → AdjEM ${signed(r.adj_net)} (opponent-adjusted).`;
}

/** The combo's players as solid archetype-colored rectangle pills (one per
 *  player, height-ordered server-side), identical in style to the TeamDetail
 *  lineup waffle. Pills are fixed-width so the spacing stays uniform across
 *  rows and combo sizes; below `sm` they collapse to first/last initials. */
function LineupCell({ row }: { row: LineupRanking }) {
  return (
    <div className="flex items-center gap-1.5 sm:gap-2">
      {row.lineup.map((pid, i) => {
        const cls = row.player_classes[i];
        const name = row.player_names[i] ?? 'Unknown';
        const parts = name.split(/\s+/).filter(Boolean);
        const initials = (
          parts.length > 1
            ? parts[0][0] + parts[parts.length - 1][0]
            : (parts[0] ?? '?').slice(0, 2)
        ).toUpperCase();
        const bg = classColor(cls);
        return (
          <SeasonLink
            key={pid}
            to={`/players/${pid}`}
            className="block w-12 sm:w-28 truncate text-center px-1 sm:px-2 py-1.5 rounded-md text-xs font-medium hover:opacity-90 transition-opacity"
            style={{ background: bg, color: textOnClass(cls) }}
            title={`${name}${cls ? ` · ${classTitle(cls)}` : ''}`}
          >
            <span className="sm:hidden">{initials}</span>
            <span className="hidden sm:inline">{name}</span>
          </SeasonLink>
        );
      })}
    </div>
  );
}

type SortKey = 'team_name' | 'minutes' | 'plus_minus' | 'adj_ortg' | 'adj_drtg' | 'adj_net';

export function Lineups() {
  usePageTitle('Lineups');
  const { season } = useSeason();
  const [params, setParams] = useSearchParams();
  const size = ((): Size => {
    const s = Number(params.get('size'));
    return s === 2 || s === 3 ? s : 5;
  })();
  const playerFilter = params.get('player') ?? undefined;
  const teamFilter = params.get('team') ?? undefined;

  const [rows, setRows] = useState<LineupRanking[]>([]);
  const [minMinutes, setMinMinutes] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({
    key: 'adj_net',
    dir: 'desc',
  });

  // Mutate only the `size` param, preserving season (handled by the navbar) and
  // any active player/team filter.
  const setSize = (s: Size) => {
    const next = new URLSearchParams(params);
    next.set('size', String(s));
    setParams(next, { replace: true });
  };
  const clearFilter = () => {
    const next = new URLSearchParams(params);
    next.delete('player');
    next.delete('team');
    setParams(next, { replace: true });
  };

  useEffect(() => {
    let cancelled = false;
    // Drill-down mode: when filtered to one player or team, this is no longer
    // the cross-team leaderboard, so the per-size denoising floor (50/100/150)
    // is wrong — it hides every exact 5-man unit of a deep-rotation player whose
    // floor-time scatters across many small combos (e.g. a player whose top duo
    // pools 580 min but whose best single 5-man is ~45 min would show duos/trios
    // but no 5-man). Mirror the team-page panels: order by most-used and drop to
    // a token floor that only trims 1-possession blowout noise. The unfiltered
    // global ranking keeps its default floor + adj_net ordering.
    const drill = Boolean(playerFilter || teamFilter);
    // No synchronous reset — the prior table stays up until the new one lands
    // (project convention; see Rankings.tsx).
    fetchLineupRankings({
      size,
      season,
      player: playerFilter,
      team: teamFilter,
      order: drill ? 'minutes' : undefined,
      minMinutes: drill ? 10 : undefined,
    })
      .then((res) => {
        if (cancelled) return;
        setRows(res.lineups);
        setMinMinutes(res.min_minutes);
        setError(null);
      })
      .catch((e) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [size, season, playerFilter, teamFilter]);

  const onSort = (key: SortKey) =>
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : // Team opens ascending (A→Z), AdjD lower-is-better opens ascending;
          // every other metric opens descending.
          { key, dir: key === 'team_name' || key === 'adj_drtg' ? 'asc' : 'desc' },
    );
  const sorted = useMemo(
    () => [...rows].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir)),
    [rows, sort],
  );

  // Fixed AdjEM rank over the loaded board (best = 1), keyed by the row's
  // identity. Bound to the data, so the rank stays with the lineup under
  // re-sort — only the server-side player/team filter (which re-fetches `rows`)
  // changes it. Mirrors the Rankings/Projected convention (issue #121): the `#`
  // is a stable rank, not the displayed row position.
  const adjNetRank = useMemo(() => {
    const m = new Map<string, number>();
    [...rows]
      .filter((r) => r.adj_net != null)
      .sort((a, b) => (b.adj_net as number) - (a.adj_net as number))
      .forEach((r, i) => m.set(`${r.team_id}-${r.lineup.join('-')}`, i + 1));
    return m;
  }, [rows]);

  // Label for an active player filter: the matching name from any returned row.
  const filterLabel = useMemo(() => {
    if (teamFilter) return rows[0]?.team_name ?? 'this team';
    if (playerFilter) {
      for (const r of rows) {
        const idx = r.lineup.indexOf(playerFilter);
        if (idx >= 0) return r.player_names[idx];
      }
      return 'this player';
    }
    return null;
  }, [rows, playerFilter, teamFilter]);

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="text-3xl font-bold">Lineups</h1>
          <p className="text-sm text-gray-400 mt-1 max-w-2xl">
            The most effective on-floor combinations across every team, ranked by{' '}
            <span className="text-gray-300 font-semibold">AdjEM</span> — opponent-adjusted net
            points per 100 possessions.
          </p>
          {minMinutes != null && (
            <p className="text-xs text-gray-500 mt-1">
              Minimum {Math.round(minMinutes)} shared minutes to qualify
            </p>
          )}
        </div>
        {/* 5-man / Trios / Duos toggle — pinned top-right, default 5-man. */}
        <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs self-start shrink-0">
          {([5, 3, 2] as const).map((s) => (
            <button
              key={s}
              onClick={() => setSize(s)}
              className={`px-3 py-1.5 ${
                size === s ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
              }`}
            >
              {SIZE_LABEL[s]}
            </button>
          ))}
        </div>
      </div>

      {filterLabel && (
        <div className="flex items-center gap-2 text-sm">
          <span className="text-gray-400">
            Filtered to <span className="text-gray-200 font-medium">{filterLabel}</span>
          </span>
          <button onClick={clearFilter} className="text-blue-400 hover:underline text-xs">
            clear ×
          </button>
        </div>
      )}

      {error && <div className="text-red-400">{error}</div>}
      {loading && rows.length === 0 ? (
        <div className="text-gray-400">Loading…</div>
      ) : (
        <div className="overflow-x-auto">
          <table className="min-w-full text-sm whitespace-nowrap">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700">
                <StickyHeader align="right" className="w-10">#</StickyHeader>
                <th className="sticky top-0 z-10 bg-gray-900 py-2 px-2 text-left font-medium">Lineup</th>
                <SortHeader label="Team" sortKey="team_name" current={sort} onSort={onSort} />
                <SortHeader label="Min" sortKey="minutes" current={sort} onSort={onSort} align="right"
                  title="Shared on-floor minutes (replay-reconstructed)." />
                <SortHeader label="+/−" sortKey="plus_minus" current={sort} onSort={onSort} align="right"
                  title="Raw point differential while the group was on the floor." />
                <SortHeader label="AdjO" sortKey="adj_ortg" current={sort} onSort={onSort} align="right"
                  title="Opponent-adjusted offensive rating: points scored per 100 possessions with the group on, corrected for schedule — same scale as the team rankings." />
                <SortHeader label="AdjD" sortKey="adj_drtg" current={sort} onSort={onSort} align="right"
                  title="Opponent-adjusted defensive rating: points allowed per 100 possessions with the group on (lower is better)." />
                <SortHeader label="AdjEM" sortKey="adj_net" current={sort} onSort={onSort} align="right"
                  title="Opponent-adjusted efficiency margin (AdjO − AdjD): net points per 100 possessions, schedule-corrected. The ranking metric — same scale as the team rankings' AdjEM. Hover a value for the raw → adjusted breakdown." />
              </tr>
            </thead>
            <tbody>
              {sorted.map((r) => (
                <tr key={`${r.team_id}-${r.lineup.join('-')}`} className="border-b border-gray-800 hover:bg-gray-800/50">
                  <td className="py-2 px-2 text-right tabular-nums text-gray-500">
                    {adjNetRank.get(`${r.team_id}-${r.lineup.join('-')}`) ?? '—'}
                  </td>
                  <td className="py-2 px-2 pr-4"><LineupCell row={r} /></td>
                  <td className="py-2 px-2 text-gray-300">
                    <SeasonLink to={`/teams/${r.team_id}`} className="hover:underline">
                      {r.team_name}
                    </SeasonLink>
                  </td>
                  <td className="py-2 px-2 text-right tabular-nums text-gray-300">{r.minutes.toFixed(0)}</td>
                  <td className={`py-2 px-2 text-right tabular-nums font-medium ${netColor(r.plus_minus)}`}>
                    {signed(r.plus_minus, 0)}
                  </td>
                  <td className="py-2 px-2 text-right tabular-nums text-gray-400">{fmt(r.adj_ortg, 0)}</td>
                  <td className="py-2 px-2 text-right tabular-nums text-gray-400">{fmt(r.adj_drtg, 0)}</td>
                  <td
                    className={`py-2 px-2 text-right tabular-nums font-semibold ${netColor(r.adj_net)}`}
                    title={adjTitle(r)}
                  >
                    {signed(r.adj_net)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {sorted.length === 0 && (
            <div className="text-gray-500 py-6 text-center">
              No lineups for {season}
              {filterLabel ? ` matching ${filterLabel}` : ''}. Lineup data exists only for
              PBP-covered seasons.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default Lineups;
