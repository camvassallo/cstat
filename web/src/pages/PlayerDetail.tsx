import { useEffect, useMemo, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import { Radar, RadarChart, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine } from 'recharts';
import {
  fetchPlayerDetail,
  fetchPlayerSimilar,
  type PlayerProfile,
  type PlayerSeasonStats,
  type Percentiles,
  type GameLogEntry,
  type LeagueAverages,
  type TorkvikStats,
  type PlayerArchetype,
  type SimilarPlayer,
} from '../api/client';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';
import { ArchetypeBadge, SimilarPlayers } from '../components/Archetype';
import { campomTier, campomTierColor } from '../components/campom';
import { compareValues, type SortDir } from '../components/tableSort';
import { SortHeader, StickyHeader } from '../components/TableHeaders';

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
  const [archetype, setArchetype] = useState<PlayerArchetype | null>(null);
  const [similar, setSimilar] = useState<SimilarPlayer[]>([]);
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
        setArchetype(r.archetype);
        if (r.archetype) {
          fetchPlayerSimilar(id, 8)
            .then((s) => setSimilar(s.players))
            .catch(() => setSimilar([]));
        } else {
          setSimilar([]);
        }
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
        { stat: 'Steals', value: (percentiles.stl_pct_pct ?? torvik?.stl_pct_pct ?? percentiles.spg_pct ?? 0) * 100 },
        { stat: 'Blocks', value: (percentiles.blk_pct_pct ?? torvik?.blk_pct_pct ?? percentiles.bpg_pct ?? 0) * 100 },
        { stat: 'Rebounding', value: (percentiles.drb_pct_pct ?? torvik?.drb_pct_pct ?? percentiles.rpg_pct ?? 0) * 100 },
        { stat: 'Def Rating', value: (torvik?.adj_de_pct ?? percentiles.defensive_rating_pct ?? 0) * 100 },
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
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <div className="flex items-center gap-3 flex-wrap">
            <h1 className="text-3xl font-bold">{player.name}</h1>
            {archetype && <ArchetypeBadge archetype={archetype} />}
            {torvik?.campom != null && (() => {
              const tier = campomTier(torvik.campom);
              const pctStr = torvik.campom_pct != null ? Math.round(torvik.campom_pct * 100) : null;
              return (
                <span
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border ${campomTierColor(tier)}`}
                  title="CamPom: composite player valuation. See methodology in docs/campom_methodology.md."
                >
                  <span className="text-xs uppercase tracking-wide opacity-70">CamPom</span>
                  <span className="font-bold">{torvik.campom.toFixed(1)}</span>
                  {pctStr != null && <span className="text-xs opacity-80">{pctStr} pct</span>}
                  {tier && <span className="text-xs opacity-80">· {tier}</span>}
                </span>
              );
            })()}
          </div>
          <div className="text-gray-400 flex gap-2 items-center flex-wrap mt-1">
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
        <Link
          to={`/players/compare?ids=${player.id}`}
          className="text-sm bg-blue-600 hover:bg-blue-700 text-white px-3 py-1.5 rounded font-medium"
        >
          Compare
        </Link>
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
            <PercentileBar label="eFG%" value={pct(stats.effective_fg_pct)} pctile={percentiles?.effective_fg_pct_pct ?? null} />
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

      {/* Rate Stats + Advanced Metrics */}
      {stats && (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          <div className="bg-gray-800 rounded-lg p-5">
            <h2 className="text-lg font-bold mb-3">Rate Stats</h2>
            <PercentileBar label="AST%" value={pct(stats.ast_pct)} pctile={percentiles?.ast_pct_pct ?? null} />
            <PercentileBar label="TOV%" value={pct(stats.tov_pct)} pctile={percentiles?.tov_pct_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="OR%" value={stats.orb_pct != null ? `${fmt(stats.orb_pct)}%` : '—'} pctile={percentiles?.orb_pct_pct ?? null} />
            <PercentileBar label="DR%" value={stats.drb_pct != null ? `${fmt(stats.drb_pct)}%` : '—'} pctile={percentiles?.drb_pct_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="STL%" value={stats.stl_pct != null ? `${fmt(stats.stl_pct)}%` : '—'} pctile={percentiles?.stl_pct_pct ?? null} />
            <PercentileBar label="BLK%" value={stats.blk_pct != null ? `${fmt(stats.blk_pct)}%` : '—'} pctile={percentiles?.blk_pct_pct ?? null} />
            <div className="border-t border-gray-700 my-2" />
            <PercentileBar label="FT Rate" value={stats.ft_rate != null ? fmt(stats.ft_rate, 2) : '—'} pctile={percentiles?.ft_rate_pct ?? null} />
            {torvik?.personal_foul_rate != null && (
              <PercentileBar label="FC/40" value={fmt(torvik.personal_foul_rate)} pctile={torvik.fc_rate_pct} />
            )}

            {torvik && (
              <>
                <h2 className="text-lg font-bold mt-5 mb-3">Advanced Metrics</h2>
                <PercentileBar label="GBPM" value={fmt(torvik.gbpm)} pctile={torvik.gbpm_pct} />
                <PercentileBar label="OGBPM" value={fmt(torvik.ogbpm)} pctile={torvik.ogbpm_pct} />
                <PercentileBar label="DGBPM" value={fmt(torvik.dgbpm)} pctile={torvik.dgbpm_pct} />
                <div className="border-t border-gray-700 my-2" />
                <PercentileBar label="Adj ORTG" value={fmt(torvik.adj_oe)} pctile={torvik.adj_oe_pct} />
                <PercentileBar label="Adj DRTG" value={fmt(torvik.adj_de)} pctile={torvik.adj_de_pct} />
              </>
            )}
          </div>

          {/* Shot Diet */}
          {torvik && (
            <div className="bg-gray-800 rounded-lg p-5">
              <h2 className="text-lg font-bold mb-3">Shot Diet</h2>
              <div className="flex flex-col items-center">
                <ShotDietCourt torvik={torvik} />
              </div>
              <div className="mt-6">
                <h2 className="text-lg font-bold mb-3">Shot Distribution</h2>
                <ShotDistributionBar torvik={torvik} />
              </div>
            </div>
          )}
        </div>
      )}

      {/* Similar Players */}
      {similar.length > 0 && (
        <SimilarPlayers players={similar} currentPlayerId={player.id} />
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
        <GameLogTable gameLog={gameLog} seasonPpg={stats?.ppg ?? null} />
      )}
    </div>
  );
}

type GameLogSortKey =
  | 'game_date'
  | 'opponent_name'
  | 'minutes'
  | 'points'
  | 'total_rebounds'
  | 'assists'
  | 'steals'
  | 'blocks'
  | 'turnovers'
  | 'game_score';

function GameLogTable({
  gameLog,
  seasonPpg,
}: {
  gameLog: GameLogEntry[];
  seasonPpg: number | null;
}) {
  const [sort, setSort] = useState<{ key: GameLogSortKey; dir: SortDir }>({
    key: 'game_date',
    dir: 'desc',
  });
  const onSort = (key: GameLogSortKey) => {
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'opponent_name' ? 'asc' : 'desc' },
    );
  };

  // Standout thresholds: PTS ≥ 1.5× season PPG; GmSc ≥ 1.5× this player's mean game_score.
  const meanGameScore = useMemo(() => {
    const xs = gameLog.map((g) => g.game_score).filter((x): x is number => x != null);
    if (xs.length === 0) return null;
    return xs.reduce((s, x) => s + x, 0) / xs.length;
  }, [gameLog]);
  const ptsHi = seasonPpg != null ? seasonPpg * 1.5 : null;
  const gmscHi = meanGameScore != null ? meanGameScore * 1.5 : null;

  const sorted = useMemo(() => {
    return [...gameLog].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir));
  }, [gameLog, sort]);

  return (
    <div>
      <h2 className="text-xl font-bold mb-3">Game Log</h2>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <SortHeader label="Date" sortKey="game_date" current={sort} onSort={onSort} />
              <SortHeader label="Opponent" sortKey="opponent_name" current={sort} onSort={onSort} />
              <SortHeader label="MIN" sortKey="minutes" current={sort} onSort={onSort} align="right" />
              <SortHeader label="PTS" sortKey="points" current={sort} onSort={onSort} align="right" />
              <StickyHeader align="right">FG</StickyHeader>
              <StickyHeader align="right">3P</StickyHeader>
              <SortHeader label="REB" sortKey="total_rebounds" current={sort} onSort={onSort} align="right" />
              <SortHeader label="AST" sortKey="assists" current={sort} onSort={onSort} align="right" />
              <SortHeader label="STL" sortKey="steals" current={sort} onSort={onSort} align="right" />
              <SortHeader label="BLK" sortKey="blocks" current={sort} onSort={onSort} align="right" />
              <SortHeader label="TO" sortKey="turnovers" current={sort} onSort={onSort} align="right" />
              <SortHeader label="GmSc" sortKey="game_score" current={sort} onSort={onSort} align="right" />
            </tr>
          </thead>
          <tbody>
            {sorted.map((g) => {
              const ptsHot = ptsHi != null && g.points != null && g.points >= ptsHi;
              const gmscHot = gmscHi != null && g.game_score != null && g.game_score >= gmscHi;
              return (
                <tr key={g.game_id} className="border-b border-gray-800 hover:bg-gray-800/50">
                  <td className="py-1.5 px-2 text-gray-400">{g.game_date}</td>
                  <td className="py-1.5 px-2">
                    {g.is_home === false && '@ '}
                    {g.opponent_id ? (
                      <Link to={`/teams/${g.opponent_id}`} className="text-blue-400 hover:underline">
                        {g.opponent_name ?? 'Unknown'}
                      </Link>
                    ) : (
                      g.opponent_name ?? 'Unknown'
                    )}
                  </td>
                  <td className="py-1.5 px-2 text-right">{fmt(g.minutes, 0)}</td>
                  <td className={`py-1.5 px-2 text-right font-medium ${ptsHot ? 'text-green-400' : ''}`}>
                    {g.points ?? '—'}
                  </td>
                  <td className="py-1.5 px-2 text-right">{g.fgm != null ? `${g.fgm}-${g.fga}` : '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.tpm != null ? `${g.tpm}-${g.tpa}` : '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.total_rebounds ?? '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.assists ?? '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.steals ?? '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.blocks ?? '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.turnovers ?? '—'}</td>
                  <td className={`py-1.5 px-2 text-right ${gmscHot ? 'text-green-400' : ''}`}>
                    {fmt(g.game_score)}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
