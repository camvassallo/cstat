import { useEffect, useMemo, useState } from 'react';
import { fetchConferenceRankings, type ConferenceRankings } from '../api/client';
import { conferenceLabel } from '../lib/conferences';
import {
  nextSort,
  sortRankings,
  type RankingSortKey,
  type Sort,
} from '../lib/conferenceTable';
import { SeasonLink } from '../components/SeasonLink';
import { SortHeader } from '../components/TableHeaders';
import { setPageSeasons, useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

/// Every league in a season, strongest first. Each row opens that league's
/// table, and the rating here is the one that table's header quotes.
export default function Conferences() {
  usePageTitle('Conference Rankings');
  const { season } = useSeason();
  const [data, setData] = useState<ConferenceRankings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<Sort<RankingSortKey>>({ key: 'strength_rank', dir: 'asc' });

  // Release any page-scoped season override a previous page installed, so the
  // navbar picker offers the full list here.
  useEffect(() => {
    setPageSeasons(null);
  }, []);

  useEffect(() => {
    let cancelled = false;
    fetchConferenceRankings(season)
      .then((r) => {
        if (cancelled) return;
        setError(null);
        setData(r);
      })
      .catch((e) => {
        if (cancelled) return;
        setData(null);
        setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [season]);

  const rows = useMemo(
    () => sortRankings(data?.conferences ?? [], sort, conferenceLabel),
    [data, sort],
  );
  const onSort = (key: RankingSortKey) => setSort((s) => nextSort(s, key));

  if (error) {
    return <div className="p-4 text-amber-300/90 text-sm">Could not load the rankings.</div>;
  }
  if (!data) return <div className="text-gray-400 p-4">Loading…</div>;

  const projected = data.projected === true;
  const seasonLabel = `${data.season - 1}-${(data.season % 100).toString().padStart(2, '0')}`;

  return (
    <div>
      <h1 className="text-2xl font-bold">
        Conference Rankings
        {projected && (
          <span className="ml-2 align-middle text-[10px] font-medium uppercase tracking-wide bg-indigo-900/60 text-indigo-300 px-1.5 py-0.5 rounded">
            Projected
          </span>
        )}
      </h1>
      <div className="text-sm text-gray-400 mt-1 mb-4">
        {seasonLabel} · {rows.length} conferences · ranked by league rating, the mean AdjEM across
        every team in the league
      </div>

      {rows.length === 0 ? (
        <p className="text-sm text-gray-400">No conference ratings for {seasonLabel}.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm whitespace-nowrap">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700">
                <SortHeader label="#" sortKey="strength_rank" current={sort} onSort={onSort}
                  className="font-medium" />
                <SortHeader label="Conference" sortKey="conference" current={sort} onSort={onSort}
                  className="font-medium" />
                <SortHeader label="Teams" sortKey="teams" current={sort} onSort={onSort}
                  align="right" className="font-medium" />
                <SortHeader label="Rating" sortKey="mean_adj_em" current={sort} onSort={onSort}
                  align="right" className="font-medium"
                  title="Mean adjusted efficiency margin across every team in the league. The mean rather than the best team — a league is its whole membership." />
                {!projected && (
                  <SortHeader label="AdjO" sortKey="mean_adj_o" current={sort} onSort={onSort}
                    align="right" className="font-medium"
                    title="Mean adjusted offense across the league." />
                )}
                {!projected && (
                  <SortHeader label="AdjD" sortKey="mean_adj_d" current={sort} onSort={onSort}
                    align="right" className="font-medium"
                    title="Mean adjusted defense across the league (lower is better)." />
                )}
                {!projected && (
                  <SortHeader label="Non-conf" sortKey="non_conference" current={sort}
                    onSort={onSort} align="center" className="font-medium"
                    title="Combined record for the league's teams against everyone outside it." />
                )}
                <SortHeader label="Top team" sortKey="top_team_adj_em" current={sort}
                  onSort={onSort} className="font-medium"
                  title="The league's highest-rated team." />
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.conference} className="border-b border-gray-800 hover:bg-gray-800/50">
                  <td className="py-2 px-2 text-gray-500 tabular-nums">{r.strength_rank}</td>
                  <td className="py-2 px-2">
                    <SeasonLink
                      to={`/conferences/${encodeURIComponent(r.conference)}`}
                      className="text-blue-400 hover:underline"
                    >
                      {conferenceLabel(r.conference)}
                    </SeasonLink>
                  </td>
                  <td className="py-2 px-2 text-right text-gray-300 tabular-nums">{r.teams}</td>
                  <td className="py-2 px-2 text-right font-medium tabular-nums">
                    {signed(r.mean_adj_em)}
                  </td>
                  {!projected && (
                    <td className="py-2 px-2 text-right text-gray-300 tabular-nums">
                      {r.mean_adj_o?.toFixed(1) ?? '—'}
                    </td>
                  )}
                  {!projected && (
                    <td className="py-2 px-2 text-right text-gray-300 tabular-nums">
                      {r.mean_adj_d?.toFixed(1) ?? '—'}
                    </td>
                  )}
                  {!projected && (
                    <td className="py-2 px-2 text-center text-gray-300 tabular-nums">
                      {r.non_conference_wins}-{r.non_conference_losses}
                    </td>
                  )}
                  <td className="py-2 px-2">
                    <SeasonLink
                      to={`/teams/${r.top_team_id}`}
                      className="text-blue-400 hover:underline"
                    >
                      {r.top_team_name}
                    </SeasonLink>
                    <span className="text-gray-500 text-xs ml-1 tabular-nums">
                      {signed(r.top_team_adj_em)}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function signed(v: number): string {
  return `${v >= 0 ? '+' : ''}${v.toFixed(1)}`;
}
