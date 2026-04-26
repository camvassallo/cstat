import { useEffect, useMemo, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import {
  fetchTeamDetail,
  type TeamProfile,
  type ScheduleEntry,
  type RosterEntry,
  type ArchetypeCount,
} from '../api/client';
import { classColor } from '../components/archetypeColors';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');
const rkStr = (v: number | null | undefined) => (v != null ? `#${v}` : undefined);

function StatCard({ label, value, rank }: { label: string; value: string; rank?: string }) {
  return (
    <div className="bg-gray-800 rounded-lg p-4 text-center">
      <div className="text-xs text-gray-400 uppercase tracking-wide mb-1">{label}</div>
      <div className="text-2xl font-bold">{value}</div>
      {rank && <div className="text-xs text-gray-500 mt-1">{rank}</div>}
    </div>
  );
}

function FourFactors({ team, label }: { team: TeamProfile; label: string }) {
  const isOffense = label === 'Offense';
  const items = isOffense
    ? [
        { label: 'eFG%', value: pct(team.effective_fg_pct), rank: rkStr(team.effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.turnover_pct), rank: rkStr(team.turnover_pct_rank) },
        { label: 'ORB%', value: pct(team.off_rebound_pct), rank: rkStr(team.off_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.ft_rate, 2), rank: rkStr(team.ft_rate_rank) },
      ]
    : [
        { label: 'eFG%', value: pct(team.opp_effective_fg_pct), rank: rkStr(team.opp_effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.opp_turnover_pct), rank: rkStr(team.opp_turnover_pct_rank) },
        { label: 'DRB%', value: pct(team.def_rebound_pct), rank: rkStr(team.def_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.opp_ft_rate, 2), rank: rkStr(team.opp_ft_rate_rank) },
      ];

  return (
    <div className="bg-gray-800 rounded-lg p-4">
      <h3 className="text-sm font-semibold text-gray-400 uppercase mb-3">{label} Four Factors</h3>
      <div className="grid grid-cols-4 gap-3 text-center">
        {items.map((item) => (
          <div key={item.label}>
            <div className="text-xs text-gray-500">{item.label}</div>
            <div className="font-semibold">{item.value}</div>
            {item.rank && <div className="text-[10px] text-gray-500">{item.rank}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}

export default function TeamDetail() {
  const { id } = useParams<{ id: string }>();
  const [team, setTeam] = useState<TeamProfile | null>(null);
  const [schedule, setSchedule] = useState<ScheduleEntry[]>([]);
  const [roster, setRoster] = useState<RosterEntry[]>([]);
  const [archetypeDist, setArchetypeDist] = useState<ArchetypeCount[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    fetchTeamDetail(id)
      .then((r) => {
        setTeam(r.team);
        setSchedule(r.schedule);
        setRoster(r.roster);
        setArchetypeDist(r.archetype_distribution);
      })
      .finally(() => setLoading(false));
  }, [id]);

  // Balance score: normalized Shannon entropy of archetype distribution.
  // 1.0 = maximally diverse, 0 = single-class roster.
  const balance = useMemo(() => {
    const total = archetypeDist.reduce((s, a) => s + a.count, 0);
    if (total < 2 || archetypeDist.length < 2) return null;
    const entropy = archetypeDist.reduce((s, a) => {
      const p = a.count / total;
      return s + (p > 0 ? -p * Math.log(p) : 0);
    }, 0);
    return entropy / Math.log(archetypeDist.length);
  }, [archetypeDist]);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!team) return <div className="text-red-400">Team not found</div>;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-3xl font-bold">{team.name}</h1>
        <div className="text-gray-400">
          {team.conference ?? 'Independent'} &middot; {team.wins ?? 0}-{team.losses ?? 0}
        </div>
      </div>

      {/* Stat Cards */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
        <StatCard label="AdjEM" value={fmt(team.adj_efficiency_margin)} rank={rkStr(team.adj_efficiency_margin_rank)} />
        <StatCard label="AdjO" value={fmt(team.adj_offense)} rank={rkStr(team.adj_offense_rank)} />
        <StatCard label="AdjD" value={fmt(team.adj_defense)} rank={rkStr(team.adj_defense_rank)} />
        <StatCard label="Tempo" value={fmt(team.adj_tempo)} rank={rkStr(team.adj_tempo_rank)} />
        <StatCard label="SOS" value={fmt(team.sos, 2)} rank={team.sos_rank ? `#${team.sos_rank}` : undefined} />
        <StatCard label="ELO" value={fmt(team.elo_rating, 0)} rank={team.elo_rank ? `#${team.elo_rank}` : undefined} />
      </div>

      {/* Four Factors */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FourFactors team={team} label="Offense" />
        <FourFactors team={team} label="Defense" />
      </div>

      {/* Archetype Distribution */}
      {archetypeDist.length > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
            <h2 className="text-lg font-bold">Roster Archetypes</h2>
            {balance != null && (
              <span className="text-xs text-gray-400">
                Balance:{' '}
                <span className="font-bold text-gray-200">
                  {(balance * 100).toFixed(0)}%
                </span>
                <span className="text-gray-500 ml-1">
                  ({balance >= 0.85 ? 'very diverse'
                    : balance >= 0.7 ? 'diverse'
                    : balance >= 0.5 ? 'balanced'
                    : balance >= 0.3 ? 'narrow'
                    : 'single-style'})
                </span>
              </span>
            )}
          </div>
          {(() => {
            const total = archetypeDist.reduce((s, a) => s + a.count, 0);
            return (
              <div className="space-y-2">
                <div className="flex h-3 rounded overflow-hidden bg-gray-900">
                  {archetypeDist.map((a) => (
                    <div
                      key={a.primary_class}
                      className="h-3"
                      style={{
                        flexBasis: `${(a.count / total) * 100}%`,
                        background: classColor(a.primary_class),
                      }}
                      title={`${a.primary_class}: ${a.count}`}
                    />
                  ))}
                </div>
                <div className="flex flex-wrap gap-2 mt-2">
                  {archetypeDist.map((a) => (
                    <span
                      key={a.primary_class}
                      className="inline-flex items-center gap-1.5 text-xs px-2 py-1 rounded bg-gray-900"
                    >
                      <span
                        className="inline-block w-2 h-2 rounded-full"
                        style={{ background: classColor(a.primary_class) }}
                      />
                      <span style={{ color: classColor(a.primary_class) }} className="font-semibold">
                        {a.primary_class}
                      </span>
                      <span className="text-gray-400">× {a.count}</span>
                    </span>
                  ))}
                </div>
              </div>
            );
          })()}
        </div>
      )}

      {/* Roster */}
      <div>
        <h2 className="text-xl font-bold mb-3">Roster</h2>
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700 text-left">
                <th className="py-2 px-2">Player</th>
                <th className="py-2 px-2">Class</th>
                <th className="py-2 px-2 text-right">GP</th>
                <th className="py-2 px-2 text-right">MPG</th>
                <th className="py-2 px-2 text-right">PPG</th>
                <th className="py-2 px-2 text-right">RPG</th>
                <th className="py-2 px-2 text-right">APG</th>
                <th className="py-2 px-2 text-right">eFG%</th>
                <th className="py-2 px-2 text-right">TS%</th>
                <th className="py-2 px-2 text-right">GBPM</th>
                <th className="py-2 px-2 text-right">ORTG</th>
              </tr>
            </thead>
            <tbody>
              {roster.map((p) => (
                <tr key={p.player_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                  <td className="py-2 px-2">
                    <Link to={`/players/${p.player_id}`} className="text-blue-400 hover:underline">
                      {p.name}
                    </Link>
                  </td>
                  <td className="py-2 px-2">
                    {p.primary_class ? (
                      <span
                        className="text-[10px] font-bold uppercase tracking-wide px-1.5 py-0.5 rounded"
                        style={{
                          color: classColor(p.primary_class),
                          background: classColor(p.primary_class) + '22',
                        }}
                      >
                        {p.primary_class}
                      </span>
                    ) : (
                      <span className="text-gray-600 text-xs">—</span>
                    )}
                  </td>
                  <td className="py-2 px-2 text-right">{p.games_played}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.minutes_per_game)}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.ppg)}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.rpg)}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.apg)}</td>
                  <td className="py-2 px-2 text-right">{pct(p.effective_fg_pct)}</td>
                  <td className="py-2 px-2 text-right">{pct(p.true_shooting_pct)}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.gbpm)}</td>
                  <td className="py-2 px-2 text-right">{fmt(p.offensive_rating)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* Schedule */}
      <div>
        <h2 className="text-xl font-bold mb-3">Schedule</h2>
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-gray-400 border-b border-gray-700 text-left">
                <th className="py-2 px-2">Date</th>
                <th className="py-2 px-2">Opponent</th>
                <th className="py-2 px-2 text-center">Result</th>
                <th className="py-2 px-2 text-center">Score</th>
              </tr>
            </thead>
            <tbody>
              {schedule.map((g) => {
                const won = g.team_score != null && g.opponent_score != null && g.team_score > g.opponent_score;
                const lost = g.team_score != null && g.opponent_score != null && g.team_score < g.opponent_score;
                return (
                  <tr key={g.game_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                    <td className="py-2 px-2 text-gray-400">{g.game_date}</td>
                    <td className="py-2 px-2">
                      {g.is_home === false && '@ '}
                      {g.opponent_id ? (
                        <Link to={`/teams/${g.opponent_id}`} className="text-blue-400 hover:underline">
                          {g.opponent_name ?? 'Unknown'}
                        </Link>
                      ) : (
                        g.opponent_name ?? 'Unknown'
                      )}
                      {g.is_neutral && ' (N)'}
                      {g.is_conference && <span className="text-gray-500 ml-1">*</span>}
                    </td>
                    <td className={`py-2 px-2 text-center font-semibold ${won ? 'text-green-400' : lost ? 'text-red-400' : ''}`}>
                      {g.team_score != null ? (won ? 'W' : 'L') : '—'}
                    </td>
                    <td className="py-2 px-2 text-center">
                      {g.team_score != null ? `${g.team_score}-${g.opponent_score}` : '—'}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
