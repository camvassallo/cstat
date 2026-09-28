import { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { fetchConference, type ConferenceDetail } from '../api/client';
import { conferenceLabel } from '../lib/conferences';
import { SeasonLink } from '../components/SeasonLink';
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

  const nonConf = `${data.non_conference_wins}-${data.non_conference_losses}`;

  return (
    <div>
      <h1 className="text-2xl font-bold">{label}</h1>
      <div className="text-sm text-gray-400 mt-1 mb-4">
        {data.season - 1}-{(data.season % 100).toString().padStart(2, '0')} · {data.teams.length}{' '}
        teams
        {data.mean_adj_em != null && data.strength_rank != null && (
          <>
            {' · '}
            <span
              title="Mean adjusted efficiency margin across every team in the league, and where that ranks among all leagues. The mean rather than the best team — a league is its whole membership."
            >
              league rating {data.mean_adj_em >= 0 ? '+' : ''}
              {data.mean_adj_em.toFixed(1)} ({ordinal(data.strength_rank)} of{' '}
              {data.conference_count})
            </span>
          </>
        )}
        {' · '}
        <span title="Combined record for the league's teams against everyone outside it.">
          non-conference {nonConf}
        </span>
      </div>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <th className="py-2 px-2 text-left font-medium">#</th>
              <th className="py-2 px-2 text-left font-medium">Team</th>
              <th className="py-2 px-2 text-center font-medium">Conf</th>
              <th className="py-2 px-2 text-center font-medium">Overall</th>
              <th className="py-2 px-2 text-right font-medium">AdjEM</th>
              <th className="py-2 px-2 text-right font-medium">AdjO</th>
              <th className="py-2 px-2 text-right font-medium">AdjD</th>
              <th className="py-2 px-2 text-right font-medium">SOS</th>
            </tr>
          </thead>
          <tbody>
            {data.teams.map((t, i) => (
              <tr key={t.team_id} className="border-b border-gray-800">
                <td className="py-2 px-2 text-gray-500">{i + 1}</td>
                <td className="py-2 px-2">
                  <SeasonLink to={`/teams/${t.team_id}`} className="text-blue-400 hover:underline">
                    {t.team_name}
                  </SeasonLink>
                </td>
                <td className="py-2 px-2 text-center font-medium">
                  {t.conference_wins}-{t.conference_losses}
                </td>
                <td className="py-2 px-2 text-center text-gray-300">
                  {t.wins}-{t.losses}
                </td>
                <td className="py-2 px-2 text-right">
                  {fmt(t.adj_em, true)}
                  {t.adj_em_rank != null && (
                    <span className="text-gray-500 text-xs ml-1">#{t.adj_em_rank}</span>
                  )}
                </td>
                <td className="py-2 px-2 text-right text-gray-300">{fmt(t.adj_o)}</td>
                <td className="py-2 px-2 text-right text-gray-300">{fmt(t.adj_d)}</td>
                <td className="py-2 px-2 text-right text-gray-300">
                  {fmt(t.sos, true)}
                  {t.sos_rank != null && (
                    <span className="text-gray-500 text-xs ml-1">#{t.sos_rank}</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
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
