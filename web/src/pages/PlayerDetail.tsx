import { useEffect, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import { Radar, RadarChart, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine } from 'recharts';
import { fetchPlayerDetail, type PlayerProfile, type PlayerSeasonStats, type Percentiles, type GameLogEntry, type LeagueAverages, type TorkvikStats } from '../api/client';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');

// Color based on percentile rank (0-1): red → orange → yellow → blue → green
const pctileColor = (pctile: number | null) => {
  if (pctile == null) return '#374151'; // gray-700
  const p = pctile * 100;
  if (p >= 80) return '#22c55e'; // green-500
  if (p >= 60) return '#3b82f6'; // blue-500
  if (p >= 40) return '#eab308'; // yellow-500
  if (p >= 20) return '#f97316'; // orange-500
  return '#ef4444'; // red-500
};

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

          {/* Shot Diet */}
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Shot Diet</h2>
            {(() => {
              // Convert 0-1 decimals to 0-100 for display and color scaling
              const rimPct = torvik.rim_pct != null ? torvik.rim_pct * 100 : null;
              const midPct = torvik.mid_pct != null ? torvik.mid_pct * 100 : null;
              const tpPctVal = torvik.tp_pct != null ? torvik.tp_pct * 100 : null;
              const ftPct = (torvik.ftm != null && torvik.fta != null && torvik.fta > 0)
                ? (torvik.ftm / torvik.fta) * 100 : null;

              // SVG half-court: broadcast view — baseline at top, half-court at bottom
              const cx = 150, hoopY = 14;

              return (
                <div className="flex flex-col items-center">
                  <svg viewBox="0 0 300 200" className="w-full max-w-xs" style={{ filter: 'drop-shadow(0 0 8px rgba(0,0,0,0.3))' }}>
                    {/* Dark background */}
                    <rect x="0" y="0" width="300" height="200" rx="4" fill="#1f2937" />
                    {/* Court fill = 3PT zone color (clipped to court boundary) */}
                    <rect x="10" y="0" width="280" height="200" fill={pctileColor(torvik.tp_pct_pct)} opacity="0.6" />

                    {/* Arc interior = mid-range color (inside arc, paint will cover its portion) */}
                    <path
                      d={`M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0 Z`}
                      fill={pctileColor(torvik.mid_pct_pct)}
                      opacity="0.7"
                    />

                    {/* Paint = rim zone */}
                    <rect x="105" y="0" width="90" height="108" fill={pctileColor(torvik.rim_pct_pct)} opacity="0.7" />

                    {/* FT semicircle — bottom half, diameter = paint width (90) */}
                    <path d={`M 105 108 A 45 45 0 0 0 195 108`} fill={pctileColor(ftPct != null ? Math.min(Math.max((ftPct - 55) / 35, 0), 1) : null)} opacity="0.75" />

                    {/* Court lines */}
                    {/* Outer boundary */}
                    <rect x="10" y="0" width="280" height="200" fill="none" stroke="#6b7280" strokeWidth="1.5" />
                    {/* Baseline (top) */}
                    <line x1="10" y1="0" x2="290" y2="0" stroke="#6b7280" strokeWidth="2" />
                    {/* Paint */}
                    <rect x="105" y="0" width="90" height="108" fill="none" stroke="#6b7280" strokeWidth="1" />
                    {/* FT semicircle (bottom half, diameter = paint width) */}
                    <path d="M 105 108 A 45 45 0 0 0 195 108" fill="none" stroke="#6b7280" strokeWidth="1" />
                    {/* 3PT arc — straight down from baseline then arc across */}
                    <path d={`M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0`} fill="none" stroke="#6b7280" strokeWidth="1.5" />
                    {/* Restricted area arc */}
                    <path d={`M ${cx - 20} ${hoopY} A 20 20 0 0 0 ${cx + 20} ${hoopY}`} fill="none" stroke="#6b7280" strokeWidth="1" />
                    {/* Hoop */}
                    <circle cx={cx} cy={hoopY} r="5" fill="none" stroke="#f97316" strokeWidth="1.5" />
                    {/* Backboard */}
                    <line x1={cx - 15} y1={hoopY - 6} x2={cx + 15} y2={hoopY - 6} stroke="#6b7280" strokeWidth="2" />

                    {/* Zone labels */}
                    {/* Rim — centered in paint */}
                    <text x={cx} y="48" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Rim</text>
                    <text x={cx} y="62" textAnchor="middle" fill="white" fontSize="10" opacity="0.8">
                      {rimPct != null ? `${rimPct.toFixed(1)}%` : '—'}
                    </text>
                    <text x={cx} y="74" textAnchor="middle" fill="#9ca3af" fontSize="8">
                      {torvik.rim_made ?? 0}-{torvik.rim_attempted ?? 0}
                    </text>

                    {/* FT — in the bottom semicircle */}
                    <text x={cx} y="124" textAnchor="middle" fill="white" fontSize="10" fontWeight="600">FT</text>
                    <text x={cx} y="135" textAnchor="middle" fill="white" fontSize="9" opacity="0.8">
                      {ftPct != null ? `${ftPct.toFixed(1)}%` : '—'}
                    </text>
                    <text x={cx} y="145" textAnchor="middle" fill="#9ca3af" fontSize="8">
                      {torvik.ftm ?? 0}-{torvik.fta ?? 0}
                    </text>

                    {/* Mid-range — right wing */}
                    <text x="232" y="55" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Mid</text>
                    <text x="232" y="68" textAnchor="middle" fill="white" fontSize="10" opacity="0.8">
                      {midPct != null ? `${midPct.toFixed(1)}%` : '—'}
                    </text>
                    <text x="232" y="78" textAnchor="middle" fill="#9ca3af" fontSize="8">
                      {torvik.mid_made ?? 0}-{torvik.mid_attempted ?? 0}
                    </text>

                    {/* 3PT — left corner */}
                    <text x="50" y="155" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">3PT</text>
                    <text x="50" y="168" textAnchor="middle" fill="white" fontSize="10" opacity="0.8">
                      {tpPctVal != null ? `${tpPctVal.toFixed(1)}%` : '—'}
                    </text>
                    <text x="50" y="180" textAnchor="middle" fill="#9ca3af" fontSize="8">
                      {torvik.tpm ?? 0}-{torvik.tpa ?? 0}
                    </text>
                  </svg>

                </div>
              );
            })()}
          </div>
        </div>
      )}

      {/* Advanced Metrics + Shot Distribution */}
      {torvik && (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Advanced Metrics</h2>
            <PercentileBar label="BPM" value={fmt(torvik.gbpm)} pctile={torvik.gbpm_pct} />
            <PercentileBar label="OBPM" value={fmt(torvik.ogbpm)} pctile={torvik.ogbpm_pct} />
            <PercentileBar label="DBPM" value={fmt(torvik.dgbpm)} pctile={torvik.dgbpm_pct} />
            <PercentileBar label="Adj ORTG" value={fmt(torvik.adj_oe)} pctile={torvik.adj_oe_pct} />
            <PercentileBar label="Adj DRTG" value={fmt(torvik.adj_de)} pctile={torvik.adj_de_pct} />
          </div>
          {(() => {
            const rimAtt = torvik.rim_attempted ?? 0;
            const midAtt = torvik.mid_attempted ?? 0;
            const tpAtt = torvik.tpa ?? 0;
            const totalAtt = rimAtt + midAtt + tpAtt;
            if (totalAtt === 0) return null;
            const rimW = (rimAtt / totalAtt) * 100;
            const midW = (midAtt / totalAtt) * 100;
            const tpW = (tpAtt / totalAtt) * 100;
            return (
              <div className="bg-gray-800 rounded-lg p-5">
                <h2 className="text-lg font-bold mb-3">Shot Distribution</h2>
                <div className="flex rounded-full h-6 overflow-hidden text-[10px] font-medium gap-[2px]">
                  {rimW > 0 && (
                    <div className="flex items-center justify-center first:rounded-l-full" style={{ width: `${rimW}%`, backgroundColor: pctileColor(torvik.rim_pct_pct) }}>
                      {rimW >= 12 ? `Rim ${rimAtt}` : ''}
                    </div>
                  )}
                  {midW > 0 && (
                    <div className="flex items-center justify-center" style={{ width: `${midW}%`, backgroundColor: pctileColor(torvik.mid_pct_pct) }}>
                      {midW >= 12 ? `Mid ${midAtt}` : ''}
                    </div>
                  )}
                  {tpW > 0 && (
                    <div className="flex items-center justify-center last:rounded-r-full" style={{ width: `${tpW}%`, backgroundColor: pctileColor(torvik.tp_pct_pct) }}>
                      {tpW >= 12 ? `3PT ${tpAtt}` : ''}
                    </div>
                  )}
                </div>
                <div className="flex justify-between text-[10px] text-gray-400 mt-1.5">
                  <span>Rim {rimW.toFixed(0)}%</span>
                  <span>Mid {midW.toFixed(0)}%</span>
                  <span>3PT {tpW.toFixed(0)}%</span>
                </div>
              </div>
            );
          })()}
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
