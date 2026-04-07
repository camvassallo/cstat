import { useEffect, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import { Radar, RadarChart, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid } from 'recharts';
import { fetchPlayerDetail, type PlayerProfile, type PlayerSeasonStats, type Percentiles, type GameLogEntry } from '../api/client';

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
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!id) return;
    fetchPlayerDetail(id)
      .then((r) => {
        setPlayer(r.player);
        setStats(r.season_stats);
        setPercentiles(r.percentiles);
        setGameLog(r.game_log);
      })
      .finally(() => setLoading(false));
  }, [id]);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!player) return <div className="text-red-400">Player not found</div>;

  const radarData = percentiles
    ? [
        { stat: 'Scoring', value: (percentiles.ppg_pct ?? 0) * 100 },
        { stat: 'Efficiency', value: (percentiles.true_shooting_pct_pct ?? 0) * 100 },
        { stat: 'ORTG', value: (percentiles.offensive_rating_pct ?? 0) * 100 },
        { stat: 'Rebounds', value: (percentiles.rpg_pct ?? 0) * 100 },
        { stat: 'Assists', value: (percentiles.apg_pct ?? 0) * 100 },
        { stat: 'Defense', value: (percentiles.bpm_pct ?? 0) * 100 },
        { stat: 'Steals', value: (percentiles.spg_pct ?? 0) * 100 },
        { stat: 'Blocks', value: (percentiles.bpg_pct ?? 0) * 100 },
      ]
    : [];

  const rollingData = gameLog
    .filter((g) => g.rolling_game_score != null)
    .map((g) => ({
      date: g.game_date,
      gameScore: g.rolling_game_score,
      ppg: g.rolling_ppg,
    }));

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
        </div>
      </div>

      {stats && (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Season Stats with Percentile Bars */}
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Season Stats</h2>
            <div className="text-xs text-gray-500 mb-2">{stats.games_played} GP &middot; {fmt(stats.minutes_per_game)} MPG</div>
            <PercentileBar label="PPG" value={fmt(stats.ppg)} pctile={percentiles?.ppg_pct ?? null} />
            <PercentileBar label="RPG" value={fmt(stats.rpg)} pctile={percentiles?.rpg_pct ?? null} />
            <PercentileBar label="APG" value={fmt(stats.apg)} pctile={percentiles?.apg_pct ?? null} />
            <PercentileBar label="SPG" value={fmt(stats.spg)} pctile={percentiles?.spg_pct ?? null} />
            <PercentileBar label="BPG" value={fmt(stats.bpg)} pctile={percentiles?.bpg_pct ?? null} />
            <PercentileBar label="TS%" value={pct(stats.true_shooting_pct)} pctile={percentiles?.true_shooting_pct_pct ?? null} />
            <PercentileBar label="USG%" value={pct(stats.usage_rate)} pctile={percentiles?.usage_rate_pct ?? null} />
            <PercentileBar label="ORTG" value={fmt(stats.offensive_rating)} pctile={percentiles?.offensive_rating_pct ?? null} />
            <PercentileBar label="DRTG" value={fmt(stats.defensive_rating)} pctile={percentiles?.defensive_rating_pct ?? null} />
            <PercentileBar label="BPM" value={fmt(stats.bpm)} pctile={percentiles?.bpm_pct ?? null} />
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
