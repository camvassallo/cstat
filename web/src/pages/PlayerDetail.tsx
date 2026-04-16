import { useEffect, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import { Radar, RadarChart, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine } from 'recharts';
import { fetchPlayerDetail, type PlayerProfile, type PlayerSeasonStats, type Percentiles, type GameLogEntry, type LeagueAverages, type TorkvikStats } from '../api/client';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');

function PercentileBar({ label, value, pctile }: { label: string; value: string; pctile: number | null }) {
  const p = pctile != null ? Math.round(pctile * 100) : null;
  const color = p == null ? 'bg-gray-600' : p >= 80 ? 'bg-green-500' : p >= 60 ? 'bg-blue-500' : p >= 40 ? 'bg-yellow-500' : p >= 20 ? 'bg-orange-500' : 'bg-red-500';

  return (
    <div className="flex items-center gap-3 py-1">
      <div className="w-24 text-xs text-gray-400">{label}</div>
      <div className="w-16 text-sm font-medium text-right">{value}</div>
      <div className="flex-1 bg-gray-700 rounded-full h-2.5">
        <div className={`h-2.5 rounded-full ${color}`} style={{ width: `${p ?? 0}%` }} />
      </div>
      <div className="w-10 text-xs text-gray-400 text-right">{p != null ? `${p}th` : '—'}</div>
    </div>
  );
}

function heightString(inches: number | null) {
  if (inches == null) return null;
  return `${Math.floor(inches / 12)}'${inches % 12}"`;
}

export default function PlayerDetail() {
  const { id } = useParams<{ id: string }>();
  const [player, setPlayer] = useState<PlayerProfile | null>(null);
  const [stats, setStats] = useState<PlayerSeasonStats | null>(null);
  const [percentiles, setPercentiles] = useState<Percentiles | null>(null);
  const [gameLog, setGameLog] = useState<GameLogEntry[]>([]);
  const [leagueAvg, setLeagueAvg] = useState<LeagueAverages | null>(null);
  const [torvik, setTorvik] = useState<TorkvikStats | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    fetchPlayerDetail(id)
      .then((r) => {
        setPlayer(r.player);
        setStats(r.season_stats);
        setPercentiles(r.percentiles);
        setGameLog(r.game_log);
        setLeagueAvg(r.league_averages);
        setTorvik(r.torvik_stats);
      })
      .finally(() => setLoading(false));
  }, [id]);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!player) return <div className="text-red-400">Player not found</div>;

  const radarData = percentiles
    ? [
        { stat: 'Scoring', value: (percentiles.ppg_pct ?? 0) * 100 },
        { stat: 'Efficiency', value: (percentiles.true_shooting_pct_pct ?? 0) * 100 },
        { stat: '3PT', value: (percentiles.tp_pct_pct ?? 0) * 100 },
        { stat: 'Playmaking', value: (percentiles.ast_pct_pct ?? percentiles.apg_pct ?? 0) * 100 },
        { stat: 'Usage', value: (percentiles.usage_rate_pct ?? 0) * 100 },
        { stat: 'Steals', value: (torvik?.stl_pct_pct ?? percentiles.spg_pct ?? 0) * 100 },
        { stat: 'Blocks', value: (torvik?.blk_pct_pct ?? percentiles.bpg_pct ?? 0) * 100 },
        { stat: 'Rebounding', value: (torvik?.drb_pct_pct ?? percentiles.rpg_pct ?? 0) * 100 },
        { stat: 'Def Rating', value: (torvik?.adj_de_pct ?? 0) * 100 },
      ]
    : [];

  const rollingData = gameLog
    .filter((g) => g.rolling_game_score != null)
    .map((g) => ({
      date: g.game_date,
      gameScore: g.rolling_game_score,
      ppg: g.rolling_ppg,
    }));

  const d1AvgGameScore = leagueAvg?.avg_game_score ?? null;
  const d1AvgPpg = leagueAvg?.avg_ppg ?? null;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-3xl font-bold">{player.name}</h1>
        <div className="text-gray-400 flex gap-2 items-center flex-wrap">
          {player.jersey_number && <span>#{player.jersey_number}</span>}
          {player.position && <span>&middot; {player.position}</span>}
          {player.class_year && <span>&middot; {player.class_year}</span>}
          {player.height_inches && <span>&middot; {heightString(player.height_inches)}</span>}
          {player.weight_lbs && <span>&middot; {player.weight_lbs} lbs</span>}
          <span>&middot;</span>
          {player.team_id ? (
            <Link to={`/teams/${player.team_id}`} className="text-blue-400 hover:underline">
              {player.team_name}
            </Link>
          ) : (
            <span>{player.team_name ?? 'Unknown'}</span>
          )}
          {player.conference && <span className="text-gray-500">({player.conference})</span>}
          {stats && <><span>&middot;</span><span>{stats.games_played} GP</span></>}
          {torvik?.hometown && <><span>&middot;</span><span>{torvik.hometown}</span></>}
        </div>
      </div>

      {stats && (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Season Stats with Percentile Bars */}
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Season Stats</h2>
            <PercentileBar label="MPG" value={fmt(stats.minutes_per_game)} pctile={percentiles?.mpg_pct ?? null} />
            <PercentileBar label="USG%" value={pct(stats.usage_rate)} pctile={percentiles?.usage_rate_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="PPG" value={fmt(stats.ppg)} pctile={percentiles?.ppg_pct ?? null} />
            <PercentileBar label="RPG" value={fmt(stats.rpg)} pctile={percentiles?.rpg_pct ?? null} />
            <PercentileBar label="APG" value={fmt(stats.apg)} pctile={percentiles?.apg_pct ?? null} />
            <PercentileBar label="SPG" value={fmt(stats.spg)} pctile={percentiles?.spg_pct ?? null} />
            <PercentileBar label="BPG" value={fmt(stats.bpg)} pctile={percentiles?.bpg_pct ?? null} />
            <PercentileBar label="TOPG" value={fmt(stats.topg)} pctile={percentiles?.topg_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="TS%" value={pct(stats.true_shooting_pct)} pctile={percentiles?.true_shooting_pct_pct ?? null} />
            <PercentileBar label="eFG%" value={pct(stats.effective_fg_pct)} pctile={percentiles?.fg_pct_pct ?? null} />
          </div>

          {/* Radar Chart */}
          {radarData.length > 0 && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-3">Percentile Profile</h2>
              <ResponsiveContainer width="100%" height={300}>
                <RadarChart data={radarData}>
                  <PolarGrid stroke="#475569" />
                  <PolarAngleAxis dataKey="stat" tick={{ fill: '#94a3b8', fontSize: 12 }} />
                  <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
                  <Radar dataKey="value" stroke="#3b82f6" fill="#3b82f6" fillOpacity={0.3} />
                </RadarChart>
              </ResponsiveContainer>
            </div>
          )}
        </div>
      )}

      {/* Rate Stats */}
      {torvik && (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Rate Stats</h2>
            <PercentileBar label="AST%" value={pct(stats?.ast_pct)} pctile={percentiles?.ast_pct_pct ?? null} />
            <PercentileBar label="TOV%" value={pct(stats?.tov_pct)} pctile={percentiles?.tov_pct_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="OR%" value={torvik.orb_pct != null ? `${fmt(torvik.orb_pct)}%` : '—'} pctile={torvik.orb_pct_pct} />
            <PercentileBar label="DR%" value={torvik.drb_pct != null ? `${fmt(torvik.drb_pct)}%` : '—'} pctile={torvik.drb_pct_pct} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="STL%" value={torvik.stl_pct != null ? `${fmt(torvik.stl_pct)}%` : '—'} pctile={torvik.stl_pct_pct} />
            <PercentileBar label="BLK%" value={torvik.blk_pct != null ? `${fmt(torvik.blk_pct)}%` : '—'} pctile={torvik.blk_pct_pct} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="FT Rate" value={torvik.ft_rate != null ? `${fmt(torvik.ft_rate)}%` : '—'} pctile={torvik.ft_rate_pct} />
            <PercentileBar label="FC/40" value={fmt(torvik.personal_foul_rate)} pctile={torvik.fc_rate_pct} />
          </div>

          {/* Shot Profile */}
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Shot Profile</h2>

            {/* Shot distribution bar */}
            {(() => {
              const rim = torvik.rim_attempted ?? 0;
              const mid = torvik.mid_attempted ?? 0;
              const tp = torvik.tpa ?? 0;
              const total = rim + mid + tp;
              if (total === 0) return null;
              const rimPct = (rim / total) * 100;
              const midPct = (mid / total) * 100;
              const tpPct = (tp / total) * 100;
              return (
                <div className="mb-4">
                  <div className="text-xs text-gray-500 mb-1.5">Shot Distribution</div>
                  <div className="flex rounded-full h-5 overflow-hidden text-[10px] font-medium">
                    {rimPct > 0 && (
                      <div className="bg-green-600 flex items-center justify-center" style={{ width: `${rimPct}%` }}>
                        {rimPct >= 10 ? `Rim ${rimPct.toFixed(0)}%` : ''}
                      </div>
                    )}
                    {midPct > 0 && (
                      <div className="bg-yellow-600 flex items-center justify-center" style={{ width: `${midPct}%` }}>
                        {midPct >= 10 ? `Mid ${midPct.toFixed(0)}%` : ''}
                      </div>
                    )}
                    {tpPct > 0 && (
                      <div className="bg-blue-600 flex items-center justify-center" style={{ width: `${tpPct}%` }}>
                        {tpPct >= 10 ? `3PT ${tpPct.toFixed(0)}%` : ''}
                      </div>
                    )}
                  </div>
                  <div className="flex justify-between text-[10px] text-gray-500 mt-1">
                    <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-full bg-green-600 inline-block" />Rim</span>
                    <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-full bg-yellow-600 inline-block" />Mid</span>
                    <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-full bg-blue-600 inline-block" />3PT</span>
                  </div>
                </div>
              );
            })()}

            {/* Zone-by-zone efficiency */}
            <div className="space-y-3">
              {torvik.rim_attempted != null && torvik.rim_attempted > 0 && (
                <div>
                  <div className="flex justify-between text-sm mb-1">
                    <span className="text-gray-400">At Rim</span>
                    <span className="font-medium">{fmt(torvik.rim_pct, 1)}%</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="flex-1 bg-gray-700 rounded-full h-2">
                      <div className="h-2 rounded-full bg-green-500" style={{ width: `${Math.min((torvik.rim_pct ?? 0), 100)}%` }} />
                    </div>
                    <span className="text-xs text-gray-500 w-20 text-right">{fmt(torvik.rim_made, 0)}/{fmt(torvik.rim_attempted, 0)}</span>
                  </div>
                </div>
              )}
              {torvik.mid_attempted != null && torvik.mid_attempted > 0 && (
                <div>
                  <div className="flex justify-between text-sm mb-1">
                    <span className="text-gray-400">Mid-Range</span>
                    <span className="font-medium">{fmt(torvik.mid_pct, 1)}%</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="flex-1 bg-gray-700 rounded-full h-2">
                      <div className="h-2 rounded-full bg-yellow-500" style={{ width: `${Math.min((torvik.mid_pct ?? 0), 100)}%` }} />
                    </div>
                    <span className="text-xs text-gray-500 w-20 text-right">{fmt(torvik.mid_made, 0)}/{fmt(torvik.mid_attempted, 0)}</span>
                  </div>
                </div>
              )}
              {torvik.tpa != null && torvik.tpa > 0 && (
                <div>
                  <div className="flex justify-between text-sm mb-1">
                    <span className="text-gray-400">Three-Point</span>
                    <span className="font-medium">{fmt(torvik.tp_pct, 1)}%</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="flex-1 bg-gray-700 rounded-full h-2">
                      <div className="h-2 rounded-full bg-blue-500" style={{ width: `${Math.min((torvik.tp_pct ?? 0), 100)}%` }} />
                    </div>
                    <span className="text-xs text-gray-500 w-20 text-right">{torvik.tpm ?? '—'}/{torvik.tpa}</span>
                  </div>
                </div>
              )}
              {torvik.dunks_attempted != null && torvik.dunks_attempted > 0 && (
                <div>
                  <div className="flex justify-between text-sm mb-1">
                    <span className="text-gray-400">Dunks</span>
                    <span className="font-medium">{fmt(torvik.dunk_pct, 1)}%</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <div className="flex-1 bg-gray-700 rounded-full h-2">
                      <div className="h-2 rounded-full bg-purple-500" style={{ width: `${Math.min((torvik.dunk_pct ?? 0), 100)}%` }} />
                    </div>
                    <span className="text-xs text-gray-500 w-20 text-right">{fmt(torvik.dunks_made, 0)}/{fmt(torvik.dunks_attempted, 0)}</span>
                  </div>
                </div>
              )}
              {/* Shooting summary table */}
              <div className="pt-3 mt-3 border-t border-gray-700">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="text-gray-500 text-xs">
                      <th className="text-left pb-1">Zone</th>
                      <th className="text-right pb-1">Made-Att</th>
                      <th className="text-right pb-1">Pct</th>
                    </tr>
                  </thead>
                  <tbody className="text-gray-300">
                    {torvik.two_pm != null && (
                      <tr>
                        <td className="text-gray-400">2PT</td>
                        <td className="text-right">{torvik.two_pm}-{torvik.two_pa}</td>
                        <td className="text-right font-medium">{torvik.two_p_pct != null ? `${fmt(torvik.two_p_pct, 1)}%` : '—'}</td>
                      </tr>
                    )}
                    {torvik.tpm != null && (
                      <tr>
                        <td className="text-gray-400">3PT</td>
                        <td className="text-right">{torvik.tpm}-{torvik.tpa}</td>
                        <td className="text-right font-medium">{torvik.tp_pct != null ? `${fmt(torvik.tp_pct, 1)}%` : '—'}</td>
                      </tr>
                    )}
                    {torvik.ftm != null && (
                      <tr>
                        <td className="text-gray-400">FT</td>
                        <td className="text-right">{torvik.ftm}-{torvik.fta}</td>
                        <td className="text-right font-medium">{torvik.fta != null && torvik.fta > 0 ? `${((torvik.ftm! / torvik.fta) * 100).toFixed(1)}%` : '—'}</td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Advanced Metrics */}
      {torvik && (
        <div className="bg-gray-800 rounded-lg p-5 max-w-lg">
          <h2 className="text-lg font-bold mb-3">Advanced Metrics</h2>
          <PercentileBar label="BPM" value={fmt(torvik.gbpm)} pctile={torvik.gbpm_pct} />
          <PercentileBar label="OBPM" value={fmt(torvik.ogbpm)} pctile={torvik.ogbpm_pct} />
          <PercentileBar label="DBPM" value={fmt(torvik.dgbpm)} pctile={torvik.dgbpm_pct} />
          <PercentileBar label="Adj ORTG" value={fmt(torvik.adj_oe)} pctile={torvik.adj_oe_pct} />
          <PercentileBar label="Adj DRTG" value={fmt(torvik.adj_de)} pctile={torvik.adj_de_pct} />
        </div>
      )}

      {/* Rolling Performance Chart */}
      {rollingData.length > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <h2 className="text-lg font-bold mb-3">Rolling Performance (5-game avg)</h2>
          <ResponsiveContainer width="100%" height={250}>
            <LineChart data={rollingData}>
              <CartesianGrid stroke="#334155" />
              <XAxis dataKey="date" tick={{ fill: '#94a3b8', fontSize: 11 }} />
              <YAxis tick={{ fill: '#94a3b8', fontSize: 11 }} />
              <Tooltip contentStyle={{ background: '#1e293b', border: '1px solid #475569', borderRadius: '0.5rem' }} />
              {d1AvgGameScore != null && (
                <ReferenceLine y={d1AvgGameScore} stroke="#3b82f6" strokeDasharray="4 4" strokeOpacity={0.5} label={{ value: `D1 Avg GmSc: ${d1AvgGameScore.toFixed(1)}`, fill: '#3b82f6', fontSize: 11, position: 'insideTopLeft' }} />
              )}
              {d1AvgPpg != null && (
                <ReferenceLine y={d1AvgPpg} stroke="#22c55e" strokeDasharray="4 4" strokeOpacity={0.5} label={{ value: `D1 Avg PPG: ${d1AvgPpg.toFixed(1)}`, fill: '#22c55e', fontSize: 11, position: 'insideBottomLeft' }} />
              )}
              <Line type="monotone" dataKey="gameScore" name="Game Score" stroke="#3b82f6" dot={false} strokeWidth={2} />
              <Line type="monotone" dataKey="ppg" name="PPG" stroke="#22c55e" dot={false} strokeWidth={2} />
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}

      {/* Game Log */}
      {gameLog.length > 0 && (
        <div>
          <h2 className="text-xl font-bold mb-3">Game Log</h2>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-gray-400 border-b border-gray-700 text-left">
                  <th className="py-2 px-2">Date</th>
                  <th className="py-2 px-2">Opponent</th>
                  <th className="py-2 px-2 text-right">MIN</th>
                  <th className="py-2 px-2 text-right">PTS</th>
                  <th className="py-2 px-2 text-right">FG</th>
                  <th className="py-2 px-2 text-right">3P</th>
                  <th className="py-2 px-2 text-right">REB</th>
                  <th className="py-2 px-2 text-right">AST</th>
                  <th className="py-2 px-2 text-right">STL</th>
                  <th className="py-2 px-2 text-right">BLK</th>
                  <th className="py-2 px-2 text-right">TO</th>
                  <th className="py-2 px-2 text-right">GmSc</th>
                </tr>
              </thead>
              <tbody>
                {gameLog.map((g) => (
                  <tr key={g.game_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                    <td className="py-1.5 px-2 text-gray-400">{g.game_date}</td>
                    <td className="py-1.5 px-2">
                      {g.is_home === false && '@ '}
                      {g.opponent_name ?? 'Unknown'}
                    </td>
                    <td className="py-1.5 px-2 text-right">{fmt(g.minutes, 0)}</td>
                    <td className="py-1.5 px-2 text-right font-medium">{g.points ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.fgm != null ? `${g.fgm}-${g.fga}` : '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.tpm != null ? `${g.tpm}-${g.tpa}` : '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.total_rebounds ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.assists ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.steals ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.blocks ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{g.turnovers ?? '—'}</td>
                    <td className="py-1.5 px-2 text-right">{fmt(g.game_score)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
