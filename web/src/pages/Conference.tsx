import { useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router-dom';
import { fetchConference, type ConferenceDetail } from '../api/client';
import { conferenceLabel } from '../lib/conferences';
import {
  nextSort,
  sortStandings,
  type Sort,
  type StandingsSortKey,
} from '../lib/conferenceTable';
import { SeasonLink } from '../components/SeasonLink';
import { SortHeader } from '../components/TableHeaders';
import { setPageSeasons, useAvailableSeasons, useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

/// A single league's table: how its teams finished against each other, and
/// how the league itself stacks up.
///
/// The conference cell on the rankings board links here. Two things the
/// team-level board cannot answer live on this page — the conference record
/// (which is not stored anywhere; the API derives it from
/// `games.is_conference`) and the league's own strength and non-conference
/// record.
export default function Conference() {
  const { code = '' } = useParams<{ code: string }>();
  const { season } = useSeason();
  // Distinguishes the two reasons a league can have no table, which are not
  // the same statement and must not share copy.
  const { seasons: playedSeasons } = useAvailableSeasons();
  const [data, setData] = useState<ConferenceDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<Sort<StandingsSortKey>>({ key: 'standing', dir: 'asc' });

  const label = conferenceLabel(code);
  usePageTitle(data ? `${label} ${data.season}` : label);

  // Release any page-scoped season override a previous page installed, so the
  // navbar picker offers the full list here.
  useEffect(() => {
    setPageSeasons(null);
  }, []);

  // No synchronous reset of `data`/`error` here — `react-hooks/set-state-in-effect`
  // forbids it, and the previous league's table staying up for the length of
  // one fetch is the same mild staleness the rankings board accepts.
  // `key`-less remounts are avoided by keying the reset off the response.
  useEffect(() => {
    let cancelled = false;
    fetchConference(code, season)
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
  }, [code, season]);

  const projected = data?.projected === true;
  const rows = useMemo(
    () => sortStandings(data?.teams ?? [], sort, projected),
    [data, sort, projected],
  );
  const onSort = (key: StandingsSortKey) => setSort((s) => nextSort(s, key));

  if (error) {
    // A season nobody has played has no standings for ANY league — saying the
    // conference did not exist would be flatly untrue, and in a realignment
    // year it is exactly the kind of wrong thing a reader would believe.
    const notPlayedYet = !playedSeasons.includes(season);
    return (
      <div className="p-4 text-amber-300/90 text-sm">
        {notPlayedYet ? (
          <>
            No games have been played in {season - 1}-
            {(season % 100).toString().padStart(2, '0')} yet, so there are no standings. The{' '}
            <SeasonLink to="/projected" className="underline">
              projected board
            </SeasonLink>{' '}
            has this season&rsquo;s forecast.
          </>
        ) : (
          <>
            No standings for {label} in {season}. Conferences come and go — a league that did not
            exist that year has no table.
          </>
        )}
      </div>
    );
  }
  if (!data) return <div className="text-gray-400 p-4">Loading…</div>;

  return (
    <div>
      <h1 className="text-2xl font-bold">
        {label}
        {projected && (
          <span className="ml-2 align-middle text-[10px] font-medium uppercase tracking-wide bg-indigo-900/60 text-indigo-300 px-1.5 py-0.5 rounded">
            Projected
          </span>
        )}
      </h1>
      <div className="text-sm text-gray-400 mt-1 mb-4">
        {data.season - 1}-{(data.season % 100).toString().padStart(2, '0')} · {data.teams.length}{' '}
        teams
        {data.mean_adj_em != null && data.strength_rank != null && (
          <>
            {' · '}
            <SeasonLink
              to="/conferences"
              className="hover:underline"
              title="Mean adjusted efficiency margin across every team in the league, and where that ranks among all leagues. The mean rather than the best team — a league is its whole membership."
            >
              league rating {data.mean_adj_em >= 0 ? '+' : ''}
              {data.mean_adj_em.toFixed(1)} ({ordinal(data.strength_rank)} of{' '}
              {data.conference_count})
            </SeasonLink>
          </>
        )}
        {!projected && data.non_conference_wins != null && (
          <>
            {' · '}
            <span title="Combined record for the league's teams against everyone outside it.">
              non-conference {data.non_conference_wins}-{data.non_conference_losses}
            </span>
          </>
        )}
      </div>

      {projected && (
        <p className="text-xs text-gray-500 mb-3">
          Expected records from each game&rsquo;s projected win probability — no games have been
          played. This is an <strong>expectation, not a simulation</strong>: it is the average
          number of wins, and cannot say who takes the league. The schedule is still being
          published, so each row covers the games listed so far — the &ldquo;of&rdquo; column is
          that count, and the table is ordered by expected win rate rather than raw wins so a
          team with more games listed does not simply rank higher.
        </p>
      )}

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <SortHeader label="#" sortKey="standing" current={sort} onSort={onSort}
                className="font-medium" />
              <SortHeader label="Team" sortKey="team_name" current={sort} onSort={onSort}
                className="font-medium" />
              <SortHeader label={projected ? 'xConf' : 'Conf'} sortKey="conference_record"
                current={sort} onSort={onSort} align="center" className="font-medium"
                title={projected ? 'Sorts by expected win rate over the games listed.' : 'Sorts by win rate, then wins.'} />
              {projected && (
                <SortHeader label="of" sortKey="expected_conference_games" current={sort}
                  onSort={onSort} align="center" className="font-medium" />
              )}
              <SortHeader label={projected ? 'xOverall' : 'Overall'} sortKey="overall_record"
                current={sort} onSort={onSort} align="center" className="font-medium"
                title={projected ? 'Sorts by expected win rate over the games listed.' : 'Sorts by win rate, then wins.'} />
              {projected && (
                <SortHeader label="of" sortKey="projected_games" current={sort} onSort={onSort}
                  align="center" className="font-medium" />
              )}
              <SortHeader label="AdjEM" sortKey="adj_em" current={sort} onSort={onSort}
                align="right" className="font-medium" />
              {!projected && (
                <SortHeader label="AdjO" sortKey="adj_o" current={sort} onSort={onSort}
                  align="right" className="font-medium" />
              )}
              {!projected && (
                <SortHeader label="AdjD" sortKey="adj_d" current={sort} onSort={onSort}
                  align="right" className="font-medium" title="Lower is better." />
              )}
              {!projected && (
                <SortHeader label="SOS" sortKey="sos" current={sort} onSort={onSort}
                  align="right" className="font-medium" />
              )}
            </tr>
          </thead>
          <tbody>
            {rows.map((t) => (
              <tr key={t.team_id} className="border-b border-gray-800">
                <td className="py-2 px-2 text-gray-500">{t.standing}</td>
                <td className="py-2 px-2">
                  <SeasonLink to={`/teams/${t.team_id}`} className="text-blue-400 hover:underline">
                    {t.team_name}
                  </SeasonLink>
                </td>
                <td className="py-2 px-2 text-center font-medium">
                  {projected
                    ? fmt1(t.expected_conference_wins)
                    : `${t.conference_wins}-${t.conference_losses}`}
                </td>
                {projected && (
                  <td className="py-2 px-2 text-center text-gray-500">
                    {t.expected_conference_games ?? '—'}
                  </td>
                )}
                <td className="py-2 px-2 text-center text-gray-300">
                  {projected ? fmt1(t.expected_wins) : `${t.wins}-${t.losses}`}
                </td>
                {projected && (
                  <td className="py-2 px-2 text-center text-gray-500">
                    {t.projected_games ?? '—'}
                  </td>
                )}
                <td className="py-2 px-2 text-right">
                  {fmt(t.adj_em, true)}
                  {t.adj_em_rank != null && (
                    <span className="text-gray-500 text-xs ml-1">#{t.adj_em_rank}</span>
                  )}
                </td>
                {!projected && (
                  <td className="py-2 px-2 text-right text-gray-300">{fmt(t.adj_o)}</td>
                )}
                {!projected && (
                  <td className="py-2 px-2 text-right text-gray-300">{fmt(t.adj_d)}</td>
                )}
                {!projected && (
                  <td className="py-2 px-2 text-right text-gray-300">
                    {fmt(t.sos, true)}
                    {t.sos_rank != null && (
                      <span className="text-gray-500 text-xs ml-1">#{t.sos_rank}</span>
                    )}
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/// One decimal, for an expected-wins figure that is a mean rather than a
/// count. "10.9" says "this is a projection" in a way "11" does not.
function fmt1(v: number | null | undefined): string {
  return v == null ? '—' : v.toFixed(1);
}

function fmt(v: number | null, signed = false): string {
  if (v == null) return '—';
  return `${signed && v >= 0 ? '+' : ''}${v.toFixed(1)}`;
}

function ordinal(n: number): string {
  const rem100 = n % 100;
  if (rem100 >= 11 && rem100 <= 13) return `${n}th`;
  return `${n}${['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`;
}
