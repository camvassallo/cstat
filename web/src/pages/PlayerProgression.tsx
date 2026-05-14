import { useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router-dom';
import {
  Radar,
  RadarChart,
  PolarGrid,
  PolarAngleAxis,
  PolarRadiusAxis,
  ResponsiveContainer,
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  CartesianGrid,
  ReferenceLine,
} from 'recharts';
import {
  fetchPlayerProgression,
  type ProgressionSeason,
  type PlayerTrajectory,
} from '../api/client';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';
import { classColor } from '../components/archetypeColors';
import { ClassTooltip } from '../components/Archetype';
import { campomTier, campomTierColor } from '../components/campom';
import { pctileTextColor } from '../components/pctile';
import { SeasonLink } from '../components/SeasonLink';
import { useIsMobile } from '../components/useIsMobile';
import { usePageTitle } from '../components/usePageTitle';
import { resolveAxes } from '../components/radarAxes';
import { fracPct, pointPct } from '../components/format';

const fmt = (v: number | null | undefined, d = 1) =>
  v != null ? v.toFixed(d) : '—';

function seasonLabel(s: number): string {
  return `${s - 1}-${(s % 100).toString().padStart(2, '0')}`;
}

function heightString(inches: number | null): string {
  if (inches == null) return '';
  const ft = Math.floor(inches / 12);
  const inch = inches % 12;
  return `${ft}'${inch}"`;
}

export default function PlayerProgression() {
  const { id } = useParams<{ id: string }>();
  const isMobile = useIsMobile();
  const [data, setData] = useState<{
    available_seasons: number[];
    seasons: ProgressionSeason[];
    trajectory: PlayerTrajectory | null;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // The latest season is the canonical name + meta source. Use it for
  // the page title and the header.
  const latest = data?.seasons[0] ?? null;
  usePageTitle(latest ? `${latest.name} — progression` : null);

  useEffect(() => {
    if (!id) return;
    // No `setLoading(true)` on re-fetch — same pattern as PlayerDetail.
    // Stale data shows through the request and is replaced on resolve.
    let cancelled = false;
    fetchPlayerProgression(id)
      .then((r) => {
        if (cancelled) return;
        setData(r);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  // CamPom-over-time series. One point per season we have a torvik
  // CamPom value for; gaps appear as nulls so recharts draws across
  // them with `connectNulls`. The trajectory projection (if any) is
  // appended as a dashed extension point keyed `target_season`.
  const campomSeries = useMemo(() => {
    if (!data) return [];
    // Oldest → newest so the chart reads left → right.
    const sorted = [...data.seasons].sort((a, b) => a.season - b.season);
    const rows = sorted.map((s) => ({
      season: s.season,
      label: seasonLabel(s.season),
      campom: s.torvik_stats?.campom ?? null,
      projection: null as number | null,
      projection_lower: null as number | null,
      projection_upper: null as number | null,
    }));
    if (data.trajectory) {
      rows.push({
        season: data.trajectory.target_season,
        label: `${seasonLabel(data.trajectory.target_season)} (proj)`,
        campom: null,
        projection: data.trajectory.projected_mean,
        projection_lower: data.trajectory.projected_lower,
        projection_upper: data.trajectory.projected_upper,
      });
    }
    return rows;
  }, [data]);

  if (loading) return <div className="p-4 text-gray-400">Loading progression…</div>;
  if (error) return <div className="p-4 text-rose-300">Failed to load: {error}</div>;
  if (!data || !latest) return <div className="p-4 text-gray-400">No progression data.</div>;

  // Sort entries oldest → newest for stats table and per-season cards.
  // Reads naturally as career progression left → right.
  const seasonsAsc = [...data.seasons].sort((a, b) => a.season - b.season);

  // Pick a Y-domain that contains the rendered points (raw CamPom +
  // projection mean) with light headroom. The projection band's
  // lower/upper are intentionally excluded — they're not drawn on the
  // chart, so including them would stretch the Y-axis for nothing.
  const renderedCam = campomSeries.flatMap((r) =>
    [r.campom, r.projection].filter((v): v is number => v != null),
  );
  const camMin = renderedCam.length ? Math.min(...renderedCam) - 1 : -2;
  const camMax = renderedCam.length ? Math.max(...renderedCam) + 1 : 6;
  // Only render the chart if there's at least one real data point on
  // it — a multi-season player with no Torvik CamPom in any season
  // would otherwise render an empty plot with grid lines and nothing
  // else. The trajectory projection alone (renderedCam.length === 1
  // for a 0-data + 1-projection case) doesn't justify a "time series".
  const hasCamSeries =
    campomSeries.some((r) => r.campom != null) &&
    campomSeries.length > 1;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <div className="flex items-center gap-3 flex-wrap">
            <h1 className="text-3xl font-bold">{latest.name}</h1>
            {latest.archetype?.primary_class && (
              <ClassTooltip
                cls={latest.archetype.primary_class}
                extra={
                  latest.archetype.secondary_class
                    ? `Secondary: ${latest.archetype.secondary_class}`
                    : undefined
                }
              >
                <span
                  className="text-xs font-bold uppercase tracking-wide px-2 py-0.5 rounded border"
                  style={{
                    color: classColor(latest.archetype.primary_class),
                    borderColor: classColor(latest.archetype.primary_class),
                  }}
                >
                  {latest.archetype.primary_class}
                </span>
              </ClassTooltip>
            )}
            {data.trajectory && (() => {
              const t = data.trajectory;
              const tier = campomTier(t.projected_mean);
              const band = `${t.projected_lower.toFixed(1)}–${t.projected_upper.toFixed(1)}`;
              return (
                <span
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border border-dashed ${campomTierColor(tier)}`}
                  title={`Projected next-season CamPom. Mean ${t.projected_mean.toFixed(2)}, 80% band ${band}. Pooled backtest MAE ≈ 2.3 — directional, not a point estimate.`}
                >
                  <span className="text-xs uppercase tracking-wide opacity-70">
                    Proj {seasonLabel(t.target_season)}
                  </span>
                  <span className="font-bold">{t.projected_mean.toFixed(1)}</span>
                  <span className="text-xs opacity-70">{band}</span>
                </span>
              );
            })()}
          </div>
          <div className="text-gray-400 mt-1 text-sm flex gap-2 items-center flex-wrap">
            {latest.position && <span>{latest.position}</span>}
            {latest.class_year && <span>· {latest.class_year}</span>}
            {latest.height_inches != null && <span>· {heightString(latest.height_inches)}</span>}
            {latest.weight_lbs != null && <span>· {latest.weight_lbs} lbs</span>}
            <span>·</span>
            <SeasonLink
              to={`/players/${latest.player_id}?season=${latest.season}`}
              className="text-blue-400 hover:underline"
            >
              ← Back to {seasonLabel(latest.season)} detail
            </SeasonLink>
          </div>
        </div>
        <div className="bg-gray-800 rounded-lg px-4 py-3 text-sm">
          <div className="text-[10px] text-gray-400 uppercase tracking-wide">Seasons</div>
          <div className="font-mono text-gray-200">{data.seasons.length}</div>
        </div>
      </div>

      {/* Time-series chart */}
      {hasCamSeries && (
        <div className="bg-gray-800 rounded-lg p-5">
          <h2 className="text-lg font-bold mb-2">CamPom v3 over time</h2>
          <p className="text-xs text-gray-400 mb-3">
            Site-wide composite per season (Torvik GBPM, usage/minutes/sample/SOS adjusted).
            {data.trajectory && ' Dashed point is the next-season projection from the trajectory model.'}
          </p>
          <ResponsiveContainer width="100%" height={isMobile ? 220 : 280}>
            <LineChart data={campomSeries} margin={{ top: 8, right: 20, left: 0, bottom: 0 }}>
              <CartesianGrid stroke="#334155" strokeDasharray="3 3" />
              <XAxis dataKey="label" stroke="#94a3b8" tick={{ fontSize: 12 }} />
              <YAxis stroke="#94a3b8" domain={[camMin, camMax]} tick={{ fontSize: 12 }} />
              <Tooltip
                contentStyle={{ background: '#0f172a', border: '1px solid #334155', borderRadius: 6 }}
                labelStyle={{ color: '#cbd5e1' }}
                formatter={(value, name) => {
                  const v = typeof value === 'number' ? value : null;
                  if (v == null) return ['—', name];
                  return [v.toFixed(2), name];
                }}
              />
              <ReferenceLine y={0} stroke="#475569" />
              <Line
                type="monotone"
                dataKey="campom"
                name="CamPom v3"
                stroke="#3b82f6"
                strokeWidth={2}
                dot={{ r: 4 }}
                connectNulls
              />
              {data.trajectory && (
                <Line
                  type="monotone"
                  dataKey="projection"
                  name="Projection"
                  stroke="#a78bfa"
                  strokeDasharray="5 4"
                  strokeWidth={2}
                  dot={{ r: 5 }}
                />
              )}
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}

      {/* Stats by season — rows are stats, columns are seasons. Color
          coded by percentile where we have one; otherwise plain. */}
      <div className="bg-gray-800 rounded-lg p-5 overflow-x-auto">
        <h2 className="text-lg font-bold mb-3">Stats by season</h2>
        <table className="text-sm w-full min-w-[600px]">
          <thead className="text-gray-400 text-xs uppercase tracking-wide">
            <tr className="border-b border-gray-700">
              <th className="text-left py-2 pr-4 sticky left-0 bg-gray-800">Stat</th>
              {seasonsAsc.map((s) => (
                <th key={s.season} className="text-right py-2 px-3 whitespace-nowrap">
                  <div>{seasonLabel(s.season)}</div>
                  {s.team_name && (
                    <div className="text-[10px] text-gray-500 normal-case font-normal">
                      {s.team_name}
                    </div>
                  )}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            <StatRowGroup label="Volume" />
            <StatRow label="GP" seasons={seasonsAsc} pick={(s) => s.season_stats?.games_played ?? null} decimals={0} />
            <StatRow label="MPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.minutes_per_game ?? null} pctile={(s) => s.percentiles?.mpg_pct ?? null} />
            <StatRow label="PPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.ppg ?? null} pctile={(s) => s.percentiles?.ppg_pct ?? null} />
            <StatRow label="RPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.rpg ?? null} pctile={(s) => s.percentiles?.rpg_pct ?? null} />
            <StatRow label="APG" seasons={seasonsAsc} pick={(s) => s.season_stats?.apg ?? null} pctile={(s) => s.percentiles?.apg_pct ?? null} />
            <StatRow label="SPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.spg ?? null} pctile={(s) => s.percentiles?.spg_pct ?? null} />
            <StatRow label="BPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.bpg ?? null} pctile={(s) => s.percentiles?.bpg_pct ?? null} />
            <StatRow label="TOPG" seasons={seasonsAsc} pick={(s) => s.season_stats?.topg ?? null} pctile={(s) => s.percentiles?.topg_pct ?? null} />

            <StatRowGroup label="Shooting" />
            <StatRow label="FG%" seasons={seasonsAsc} pick={(s) => s.season_stats?.fg_pct ?? null} fmt={fracPct} />
            <StatRow label="3P%" seasons={seasonsAsc} pick={(s) => s.season_stats?.tp_pct ?? null} fmt={fracPct} />
            <StatRow label="FT%" seasons={seasonsAsc} pick={(s) => s.season_stats?.ft_pct ?? null} fmt={fracPct} />
            <StatRow label="eFG%" seasons={seasonsAsc} pick={(s) => s.season_stats?.effective_fg_pct ?? null} fmt={fracPct} pctile={(s) => s.percentiles?.effective_fg_pct_pct ?? null} />
            <StatRow label="TS%" seasons={seasonsAsc} pick={(s) => s.season_stats?.true_shooting_pct ?? null} fmt={fracPct} pctile={(s) => s.percentiles?.true_shooting_pct_pct ?? null} />

            <StatRowGroup label="Rates" />
            <StatRow label="USG%" seasons={seasonsAsc} pick={(s) => s.season_stats?.usage_rate ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.usage_rate_pct ?? null} />
            <StatRow label="AST%" seasons={seasonsAsc} pick={(s) => s.season_stats?.ast_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.ast_pct_pct ?? null} />
            <StatRow label="TOV%" seasons={seasonsAsc} pick={(s) => s.season_stats?.tov_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.tov_pct_pct ?? null} />
            <StatRow label="OR%" seasons={seasonsAsc} pick={(s) => s.season_stats?.orb_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.orb_pct_pct ?? null} />
            <StatRow label="DR%" seasons={seasonsAsc} pick={(s) => s.season_stats?.drb_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.drb_pct_pct ?? null} />
            <StatRow label="STL%" seasons={seasonsAsc} pick={(s) => s.season_stats?.stl_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.stl_pct_pct ?? null} />
            <StatRow label="BLK%" seasons={seasonsAsc} pick={(s) => s.season_stats?.blk_pct ?? null} fmt={pointPct} pctile={(s) => s.percentiles?.blk_pct_pct ?? null} />
            <StatRow label="FT Rate" seasons={seasonsAsc} pick={(s) => s.season_stats?.ft_rate ?? null} decimals={2} pctile={(s) => s.percentiles?.ft_rate_pct ?? null} />

            <StatRowGroup label="Impact" />
            <StatRow label="ORTG" seasons={seasonsAsc} pick={(s) => s.season_stats?.offensive_rating ?? null} />
            <StatRow label="DRTG" seasons={seasonsAsc} pick={(s) => s.season_stats?.defensive_rating ?? null} />
            <StatRow label="Net" seasons={seasonsAsc} pick={(s) => s.season_stats?.net_rating ?? null} signed />
            <StatRow label="GBPM" seasons={seasonsAsc} pick={(s) => s.torvik_stats?.gbpm ?? null} pctile={(s) => s.torvik_stats?.gbpm_pct ?? null} signed />
            <StatRow label="OGBPM" seasons={seasonsAsc} pick={(s) => s.torvik_stats?.ogbpm ?? null} pctile={(s) => s.torvik_stats?.ogbpm_pct ?? null} signed />
            <StatRow label="DGBPM" seasons={seasonsAsc} pick={(s) => s.torvik_stats?.dgbpm ?? null} pctile={(s) => s.torvik_stats?.dgbpm_pct ?? null} signed />
            <StatRow label="CamPom" seasons={seasonsAsc} pick={(s) => s.torvik_stats?.campom ?? null} pctile={(s) => s.torvik_stats?.campom_pct ?? null} signed />
          </tbody>
        </table>
      </div>

      {/* Per-season cards: radar + shot diet */}
      <div>
        <h2 className="text-lg font-bold mb-3">Profile by season</h2>
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          {seasonsAsc.map((s) => (
            <SeasonCard key={s.season} entry={s} isMobile={isMobile} />
          ))}
        </div>
      </div>
    </div>
  );
}

// ─── Subcomponents ─────────────────────────────────────────────────────

function StatRowGroup({ label }: { label: string }) {
  return (
    <tr className="border-t border-gray-700">
      <td colSpan={99} className="text-xs uppercase tracking-wide text-gray-500 pt-3 pb-1 sticky left-0 bg-gray-800">
        {label}
      </td>
    </tr>
  );
}

function StatRow({
  label,
  seasons,
  pick,
  pctile,
  fmt: fmtFn,
  decimals = 1,
  signed = false,
}: {
  label: string;
  seasons: ProgressionSeason[];
  pick: (s: ProgressionSeason) => number | null;
  pctile?: (s: ProgressionSeason) => number | null;
  fmt?: (v: number | null) => string;
  decimals?: number;
  signed?: boolean;
}) {
  const formatValue = (v: number | null): string => {
    if (v == null) return '—';
    if (fmtFn) return fmtFn(v);
    if (signed) return v >= 0 ? `+${v.toFixed(decimals)}` : v.toFixed(decimals);
    return v.toFixed(decimals);
  };
  return (
    <tr className="hover:bg-gray-700/30">
      <td className="text-left py-1 pr-4 text-gray-400 sticky left-0 bg-gray-800">{label}</td>
      {seasons.map((s) => {
        const value = pick(s);
        const pct = pctile?.(s) ?? null;
        const color = pct != null ? pctileTextColor(pct) : undefined;
        return (
          <td key={s.season} className="text-right py-1 px-3 font-mono tabular-nums" style={color ? { color } : undefined}>
            {formatValue(value)}
          </td>
        );
      })}
    </tr>
  );
}

function SeasonCard({ entry, isMobile }: { entry: ProgressionSeason; isMobile: boolean }) {
  const radarData = useMemo(() => {
    const axes = resolveAxes({
      season_stats: entry.season_stats,
      percentiles: entry.percentiles,
      torvik_stats: entry.torvik_stats,
    });
    return axes.map((a) => ({ stat: a.stat, value: a.value }));
  }, [entry]);
  const hasRadar = radarData.some((a) => a.value > 0);
  const hasShot = entry.torvik_stats != null;
  const tier = entry.torvik_stats?.campom != null ? campomTier(entry.torvik_stats.campom) : null;
  return (
    <div className="bg-gray-800 rounded-lg p-4">
      <div className="flex items-center justify-between gap-2 mb-3 flex-wrap">
        <div className="flex items-center gap-2">
          <span className="font-bold text-gray-200">{seasonLabel(entry.season)}</span>
          {entry.team_name && (
            <span className="text-xs text-gray-400">{entry.team_name}</span>
          )}
        </div>
        <div className="flex items-center gap-2 text-xs">
          {entry.archetype?.primary_class && (
            <span
              className="font-bold uppercase tracking-wide"
              style={{ color: classColor(entry.archetype.primary_class) }}
              title={entry.archetype.secondary_class ?? undefined}
            >
              {entry.archetype.primary_class}
            </span>
          )}
          {entry.torvik_stats?.campom != null && tier && (
            <span className={`px-1.5 rounded border ${campomTierColor(tier)}`}>
              {fmt(entry.torvik_stats.campom)}
            </span>
          )}
        </div>
      </div>
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 items-center">
        {hasRadar ? (
          <div>
            <ResponsiveContainer width="100%" height={isMobile ? 180 : 220}>
              <RadarChart data={radarData}>
                <PolarGrid stroke="#475569" />
                <PolarAngleAxis dataKey="stat" tick={{ fontSize: 10, fill: '#94a3b8' }} />
                <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
                <Radar dataKey="value" stroke="#3b82f6" fill="#3b82f6" fillOpacity={0.3} />
              </RadarChart>
            </ResponsiveContainer>
          </div>
        ) : (
          <div className="text-xs text-gray-500 italic text-center py-12">No radar data</div>
        )}
        {hasShot && entry.torvik_stats ? (
          <div className="flex flex-col items-center gap-3">
            <ShotDietCourt torvik={entry.torvik_stats} />
            <div className="w-full">
              <ShotDistributionBar torvik={entry.torvik_stats} />
            </div>
          </div>
        ) : (
          <div className="text-xs text-gray-500 italic text-center py-12">No shot diet data</div>
        )}
      </div>
    </div>
  );
}
