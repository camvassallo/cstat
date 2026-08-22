import { useEffect, useMemo, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Radar, RadarChart, PolarGrid, PolarAngleAxis, PolarRadiusAxis, ResponsiveContainer, LineChart, Line, XAxis, YAxis, Tooltip, CartesianGrid, ReferenceLine } from 'recharts';
import {
  fetchPlayerDetail,
  fetchPlayerSimilar,
  fetchPlayerPbp,
  fetchPlayerOnOff,
  type PlayerProfile,
  type PlayerPbpProfile,
  type PlayerOnOff,
  type PlayerSeasonStats,
  type Percentiles,
  type GameLogEntry,
  type LeagueAverages,
  type TorkvikStats,
  type PlayerArchetype,
  type PlayerTrajectory,
  type SimilarPlayer,
} from '../api/client';
import { ShotDietCourt, ShotDistributionBar } from '../components/ShotDiet';
import { ArchetypeBadge, SimilarPlayers, type SimilarMode } from '../components/Archetype';
import { camTier, camTierColor, camSplit } from '../components/cam';
import { bandBarClass } from '../components/scale';
import { RAPM_DISPLAY_FLOOR } from '../components/onoff';
import { conferenceLabel } from '../lib/conferences';
import { compareValues, type SortDir } from '../components/tableSort';
import { SortHeader, StickyHeader } from '../components/TableHeaders';
import { SeasonLink } from '../components/SeasonLink';
import { seasonHref, setPageSeasons, useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { useIsMobile } from '../components/useIsMobile';
import { resolveAxes } from '../components/radarAxes';
import { RadarAxisTooltip } from '../components/RadarAxisTooltip';
import { RadarTick } from '../components/RadarTick';
import { useDismissOnOutside } from '../components/useDismissOnOutside';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');
const signedFmt = (v: number | null | undefined, d = 1) =>
  v != null ? `${v > 0 ? '+' : ''}${v.toFixed(d)}` : '—';

function PercentileBar({ label, value, pctile, title }: { label: string; value: string; pctile: number | null; title?: string }) {
  const p = pctile != null ? Math.round(pctile * 100) : null;
  // Shared site scale (red → orange → yellow → blue → green), so these bars,
  // the CAM chips, and the CAMO/CAMD numerals all grade on one vocabulary.
  const color = bandBarClass(pctile);

  return (
    <div className="flex items-center gap-3 py-1" title={title}>
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

// "2026-03-15" → "Mar 15". Falls back to the raw string on parse failure so
// pre-formatted values pass through unchanged.
function shortDate(iso: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
  if (!m) return iso;
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const month = months[Number(m[2]) - 1];
  return `${month} ${Number(m[3])}`;
}

/// How many comparable players the panel asks for. Same count in both search
/// modes — the cross-year query costs the same whatever `k` is, because it
/// scans every archetype row either way.
const SIMILAR_PLAYER_COUNT = 8;

/// Identifies one comparable-players request. Stored alongside the rows it
/// returned so the panel can tell a result for the CURRENT (player, season,
/// mode) from one left over by the previous request. Shared by the writer and
/// the reader on purpose: two copies of this format would drift, and the
/// staleness check would then quietly never match.
function similarRequestKey(
  playerId: string | null,
  season: number,
  mode: SimilarMode,
): string {
  return `${playerId ?? ''}|${season}|${mode}`;
}

export default function PlayerDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const { season } = useSeason();
  const [player, setPlayer] = useState<PlayerProfile | null>(null);
  const [stats, setStats] = useState<PlayerSeasonStats | null>(null);
  const [percentiles, setPercentiles] = useState<Percentiles | null>(null);
  const [gameLog, setGameLog] = useState<GameLogEntry[]>([]);
  const [leagueAvg, setLeagueAvg] = useState<LeagueAverages | null>(null);
  const [torvik, setTorvik] = useState<TorkvikStats | null>(null);
  const [archetype, setArchetype] = useState<PlayerArchetype | null>(null);
  const [trajectory, setTrajectory] = useState<PlayerTrajectory | null>(null);
  // The comparable-players list, tagged with the request that produced it.
  // Carrying the key alongside the rows is what lets the panel tell "the other
  // mode is still loading" from "this mode found nobody" without a
  // `setLoading(true)` — a synchronous set-state in an effect body, which the
  // react-hooks compiler lint rejects.
  const [similar, setSimilar] = useState<{
    key: string;
    players: SimilarPlayer[];
  } | null>(null);
  // Panel-local, so flipping it re-runs only the neighbour search below. Not
  // in the URL: it is one panel's search range, not the page's identity.
  const [similarMode, setSimilarMode] = useState<SimilarMode>('season');
  const [pbp, setPbp] = useState<PlayerPbpProfile | null>(null);
  const [onOff, setOnOff] = useState<PlayerOnOff | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedAxis, setSelectedAxis] = useState<string | null>(null);
  const radarRef = useDismissOnOutside(selectedAxis !== null, () =>
    setSelectedAxis(null),
  );
  const isMobile = useIsMobile();
  usePageTitle(player ? `${player.name} ${player.season}` : null);

  useEffect(() => {
    if (!id) return;
    // No `setLoading(true)` here — see Rankings.tsx for the rationale.
    let cancelled = false;
    fetchPlayerDetail(id, season)
      .then((r) => {
        if (cancelled) return;
        // Publish the player's eligible seasons so the site-wide selector
        // limits the dropdown to years where this player has data. We do
        // this even on the redirect path below — the seasons list is the
        // same on both sides of the canonical-UUID swap.
        setPageSeasons(r.available_seasons);
        // Player UUIDs are season-scoped. The API resolves cross-season via
        // `natstat_id`; if the canonical UUID for this season differs, swap
        // the URL so refresh/share/back lands on the right row. Leave
        // `loading` true through the redirect so the UI doesn't render the
        // "Player not found" empty state in the gap before the next fetch.
        if (r.player.id !== id) {
          navigate(seasonHref(`/players/${r.player.id}`, season), { replace: true });
          return;
        }
        setPlayer(r.player);
        setStats(r.season_stats);
        setPercentiles(r.percentiles);
        setGameLog(r.game_log);
        setLeagueAvg(r.league_averages);
        setTorvik(r.torvik_stats);
        setArchetype(r.archetype);
        setTrajectory(r.trajectory);
        setLoading(false);
      })
      .catch(() => {
        if (cancelled) return;
        // Clear stale state so the "not found" path renders cleanly when
        // the player has no row in the requested season (e.g. switched to
        // a year before they enrolled). Without this reset the previous
        // season's data stays on screen because `player` is still set.
        setPlayer(null);
        setStats(null);
        setPercentiles(null);
        setGameLog([]);
        setLeagueAvg(null);
        setTorvik(null);
        setArchetype(null);
        setTrajectory(null);
        setSimilar(null);
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id, season, navigate]);

  // Comparable players — its OWN request, keyed on the search mode as well as
  // (player, season). Deliberately not folded into the detail fetch above,
  // even though it depends on its result: `similarMode` would then be a
  // dependency of the page's main effect, and flipping the toggle would
  // re-request the whole profile and blank every panel on the page.
  //
  // Keyed on the RESOLVED player id rather than the URL's `id`. They differ for
  // one paint on the canonical-UUID redirect, and a neighbour search against
  // another season's UUID finds no target vector and comes back empty.
  const similarTargetId = player != null && archetype != null ? player.id : null;
  const similarKey = similarRequestKey(similarTargetId, season, similarMode);
  useEffect(() => {
    if (!similarTargetId) return;
    let cancelled = false;
    const key = similarRequestKey(similarTargetId, season, similarMode);
    fetchPlayerSimilar(similarTargetId, SIMILAR_PLAYER_COUNT, season, similarMode === 'year')
      .then((r) => !cancelled && setSimilar({ key, players: r.players }))
      // Stamp the key on the failure too, so a mode that errors settles on
      // "found nobody" instead of spinning forever.
      .catch(() => !cancelled && setSimilar({ key, players: [] }));
    return () => {
      cancelled = true;
    };
  }, [similarTargetId, season, similarMode]);
  const similarPlayers = similar?.key === similarKey ? similar.players : [];
  const similarLoading = similarTargetId != null && similar?.key !== similarKey;

  // PBP season profile — own request, fetched in parallel with the main
  // payload. Keyed on (id, season); the endpoint resolves the cross-season
  // UUID and returns null when the season has no play-by-play, so the panel
  // simply doesn't render for pre-PBP seasons.
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    // No synchronous reset (set-state-in-effect lint); `.then` overwrites and a
    // one-paint stale panel during navigation matches the page convention.
    fetchPlayerPbp(id, season)
      .then((r) => !cancelled && setPbp(r.pbp))
      .catch(() => !cancelled && setPbp(null));
    fetchPlayerOnOff(id, season)
      .then((r) => !cancelled && setOnOff(r.on_off))
      .catch(() => !cancelled && setOnOff(null));
    return () => {
      cancelled = true;
    };
  }, [id, season]);

  // Release the season-selector override on unmount so the dropdown returns
  // to the global list when the user navigates away.
  useEffect(() => {
    return () => setPageSeasons(null);
  }, []);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!player) return <div className="text-red-400">Player not found</div>;

  const resolvedAxes = percentiles
    ? resolveAxes({ season_stats: stats, percentiles, torvik_stats: torvik })
    : [];
  const radarData = resolvedAxes.map((a) => ({ stat: a.stat, value: a.value }));
  const selectedResolved = selectedAxis
    ? resolvedAxes.find((a) => a.stat === selectedAxis)
    : null;

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
              const tier = camTier(torvik.campom);
              const pctStr = torvik.campom_pct != null ? Math.round(torvik.campom_pct * 100) : null;
              const split = camSplit(torvik.campom_o, torvik.campom_d);
              return (
                <span
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border ${camTierColor(tier)}`}
                  title={
                    'CAM: composite player valuation. ' +
                    (split
                      ? 'O/D halves sum to the total; D is positive-good (defensive value added). '
                      : '') +
                    'See methodology in docs/campom_methodology.md.'
                  }
                >
                  <span className="text-xs uppercase tracking-wide opacity-70">CAM</span>
                  <span className="font-bold">{torvik.campom.toFixed(1)}</span>
                  {split && <span className="text-xs opacity-80">{split}</span>}
                  {pctStr != null && <span className="text-xs opacity-80">{pctStr} pct</span>}
                  {tier && <span className="text-xs opacity-80">· {tier}</span>}
                </span>
              );
            })()}
            {trajectory && (() => {
              // growth-model projection. Pooled LOPO MAE ~2.1
              // CAM points; the q=0.1 / q=0.9 band is what users
              // should read for "how confident" — a wide band on a
              // freshman with thin signal is correct, not a flaw. Tier
              // colors track the projected mean so the chip visually
              // reflects projected quality without needing a separate
              // percentile lookup.
              const tier = camTier(trajectory.projected_mean);
              const targetLabel = `${trajectory.target_season - 1}-${(trajectory.target_season % 100).toString().padStart(2, '0')}`;
              const band = `${trajectory.projected_lower.toFixed(1)}–${trajectory.projected_upper.toFixed(1)}`;
              const direction =
                trajectory.prior_campom != null
                  ? trajectory.projected_mean > trajectory.prior_campom + 0.5
                    ? '↑'
                    : trajectory.projected_mean < trajectory.prior_campom - 0.5
                      ? '↓'
                      : '→'
                  : '';
              // Regression-to-the-mean honesty note (ROADMAP §6 Q1):
              // the model systematically under-projects elite-tier
              // returners because (a) historically very few +20+
              // returners sustained +20+, and (b) the +20+ training tail
              // is empty (cohort leaves for the NBA). Append the caveat
              // when current CAM enters the bias zone — ≥10 gets a
              // mild note, ≥15 gets the full "read the ceiling" prompt.
              // Per-bucket MAE / bias lives in
              // `trajectory_model_meta.json::mae_by_current_campom`.
              const regressionNote =
                trajectory.prior_campom != null && trajectory.prior_campom >= 15
                  ? ' Elite players are projected conservatively — read the high end of the band for the optimistic case.'
                  : trajectory.prior_campom != null && trajectory.prior_campom >= 10
                    ? ' Mild regression expected on this tier (projections sit ≈0.3 below current on +10..+15 inputs).'
                    : '';
              // The chip itself links to the cross-season progression
              // page — the projection sits naturally as the right-most
              // point in the time-series there. Hover affordance is the
              // existing dashed border; we don't add an underline so the
              // chip's typography stays clean.
              return (
                <SeasonLink
                  to={`/players/${player.id}/progression`}
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border border-dashed ${camTierColor(tier)} hover:bg-gray-700/40 transition-colors`}
                  title={`Projected next-season CAM. Mean ${trajectory.projected_mean.toFixed(2)}, 80% band ${band}. Pooled backtest MAE ≈ 2.1 — read this as directional, not a point estimate. Wide bands flag thin signal (e.g. freshmen, low-minute returners).${regressionNote} Click for full career progression.`}
                >
                  <span className="text-xs uppercase tracking-wide opacity-70">
                    Proj {targetLabel}
                  </span>
                  <span className="font-bold">{trajectory.projected_mean.toFixed(1)}</span>
                  <span className="text-xs opacity-70">{band}</span>
                  {direction && <span className="text-xs opacity-90">{direction}</span>}
                </SeasonLink>
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
              <SeasonLink to={`/teams/${player.team_id}`} className="text-blue-400 hover:underline">
                {player.team_name}
              </SeasonLink>
            ) : (
              <span>{player.team_name ?? 'Unknown'}</span>
            )}
            {player.conference && (
              <span className="text-gray-500">({conferenceLabel(player.conference)})</span>
            )}
            {stats && <><span>&middot;</span><span>{stats.games_played} GP</span></>}
            {torvik?.hometown && <><span>&middot;</span><span>{torvik.hometown}</span></>}
          </div>
        </div>
        <SeasonLink
          to={`/players/compare?ids=${player.id}`}
          className="text-sm bg-blue-600 hover:bg-blue-700 text-white px-3 py-1.5 rounded font-medium"
        >
          Compare
        </SeasonLink>
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
            <div
              ref={radarRef as React.RefObject<HTMLDivElement>}
              className="bg-gray-800 rounded-lg p-5 relative"
            >
              <h2 className="text-lg font-bold mb-3">Percentile Profile</h2>
              <ResponsiveContainer width="100%" height={isMobile ? 240 : 300}>
                <RadarChart data={radarData}>
                  <PolarGrid stroke="#475569" />
                  <PolarAngleAxis
                    dataKey="stat"
                    tick={(props) => (
                      <RadarTick
                        {...props}
                        selected={selectedAxis === props.payload?.value}
                        onSelect={(s) =>
                          setSelectedAxis((prev) => (prev === s ? null : s))
                        }
                      />
                    )}
                  />
                  <PolarRadiusAxis domain={[0, 100]} tick={false} axisLine={false} />
                  <Radar dataKey="value" stroke="#3b82f6" fill="#3b82f6" fillOpacity={0.3} />
                </RadarChart>
              </ResponsiveContainer>
              {selectedResolved && (
                <RadarAxisTooltip
                  resolutions={[selectedResolved]}
                  onClose={() => setSelectedAxis(null)}
                />
              )}
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

            {/* Scoring context (PBP-derived per-40 rates) — folded in from the old
                Play-by-Play Profile panel; renders only where PBP coverage exists. */}
            {pbp && (pbp.transition_pts_per40 != null || pbp.second_chance_pts_per40 != null) && (
              <>
                <div className="border-t border-gray-700 my-2" />
                {pbp.transition_pts_per40 != null && (
                  <PercentileBar label="Transition /40" value={fmt(pbp.transition_pts_per40)} pctile={pbp.transition_pts_per40_pct} />
                )}
                {pbp.second_chance_pts_per40 != null && (
                  <PercentileBar label="2nd chance /40" value={fmt(pbp.second_chance_pts_per40)} pctile={pbp.second_chance_pts_per40_pct} />
                )}
              </>
            )}

            {(torvik || (onOff?.rapm_net != null && (onOff.rapm_paired_possessions ?? 0) >= RAPM_DISPLAY_FLOOR)) && (
              <>
                <h2 className="text-lg font-bold mt-5 mb-3">Advanced Metrics</h2>
                {torvik && (
                  <>
                    <PercentileBar label="Adj ORTG" value={fmt(torvik.adj_oe)} pctile={torvik.adj_oe_pct} />
                    <PercentileBar label="Adj DRTG" value={fmt(torvik.adj_de)} pctile={torvik.adj_de_pct} />
                  </>
                )}
                {/* RAPM — folded in from the old On/Off panel; the one
                    context-adjusted plus-minus number, then its O and D halves.
                    Labels match the roster's Adv view (RAPM / RAPM-O / RAPM-D)
                    so the same metric reads the same on both pages. Raw on/off
                    lives in the tooltip. */}
                {onOff?.rapm_net != null && (onOff.rapm_paired_possessions ?? 0) >= RAPM_DISPLAY_FLOOR && (
                  <>
                    <PercentileBar
                      label="RAPM"
                      value={signedFmt(onOff.rapm_net)}
                      pctile={onOff.rapm_net_pct ?? null}
                      title={`Adjusted on/off (RAPM), per 100 poss — ridge-regressed with teammates and opponents held constant. Net = O − D. Raw net on/off ${signedFmt(onOff.net_on_off)}. Career-informed (decayed prior-season stints); read alongside CAM, not instead of it.`}
                    />
                    {onOff.rapm_o != null && (
                      <PercentileBar
                        label="RAPM-O"
                        value={signedFmt(onOff.rapm_o)}
                        pctile={onOff.rapm_o_pct ?? null}
                        title="Adjusted on/off, offensive half: points per 100 added on offense with teammates and opponents held constant."
                      />
                    )}
                    {onOff.rapm_d != null && (
                      <PercentileBar
                        label="RAPM-D"
                        value={signedFmt(onOff.rapm_d)}
                        pctile={onOff.rapm_d_pct ?? null}
                        title="Adjusted on/off, defensive half: points per 100 ALLOWED while defending — negative is good, so the percentile bar is inverted (a high bar means good defense)."
                      />
                    )}
                  </>
                )}
              </>
            )}
          </div>

          {/* Shot Diet + lineup-combos button (right column). Flex column so the
              button pins to the bottom and balances the taller stats column. */}
          {(torvik || onOff) && (
            <div className="bg-gray-800 rounded-lg p-5 flex flex-col">
              {torvik && (
                <>
                  <div className="flex items-baseline justify-between mb-3 flex-wrap gap-2">
                    <h2 className="text-lg font-bold">Shot Diet</h2>
                    <span className="text-xs text-gray-500">Volume by zone · FG% by color</span>
                  </div>
                  <div className="flex flex-col items-center">
                    <ShotDietCourt torvik={torvik} />
                  </div>
                  <div className="mt-8">
                    <h2 className="text-lg font-bold mb-3">Shot Distribution</h2>
                    <ShotDistributionBar torvik={torvik} />
                  </div>
                </>
              )}
              {onOff && (
                <div className="mt-auto pt-6">
                  <SeasonLink
                    to={`/lineups?player=${player.id}`}
                    className="inline-flex w-full items-center justify-center gap-1.5 rounded-md border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-blue-400 transition-colors hover:bg-gray-700 hover:text-blue-300"
                  >
                    View {player.name}'s lineup combos (duos / trios / 5-man) →
                  </SeasonLink>
                </div>
              )}
            </div>
          )}
        </div>
      )}


      {/* Similar Players. Gated on the ARCHETYPE, not on the list being
          non-empty: the header now carries the season/any-year toggle, and a
          mode that happens to find nobody must not take the control that gets
          you back out of it off the page. No archetype means no feature vector
          to search from, so there is nothing to toggle either.

          The site-wide season picker deliberately stays visible here, unlike
          the Compare page's cross-year mode. This page is still one player in
          one season — the target vector, and every other panel, comes from
          `?season=`; only this panel's CANDIDATE pool widens. */}
      {archetype && (
        <SimilarPlayers
          players={similarPlayers}
          currentPlayerId={player.id}
          mode={similarMode}
          onModeChange={setSimilarMode}
          loading={similarLoading}
        />
      )}

      {/* Rolling Performance Chart */}
      {rollingData.length > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <h2 className="text-lg font-bold mb-3">Rolling Performance (5-game avg)</h2>
          <ResponsiveContainer width="100%" height={isMobile ? 200 : 250}>
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
        <table className="min-w-full text-sm whitespace-nowrap">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <SortHeader
                label="Date"
                sortKey="game_date"
                current={sort}
                onSort={onSort}
                className="left-0 z-20 border-r border-gray-700"
              />
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
                <tr key={g.game_id} className="group border-b border-gray-800 hover:bg-gray-800">
                  <td className="py-1.5 px-2 text-gray-400 sticky left-0 z-10 bg-gray-900 group-hover:bg-gray-800 border-r border-gray-700">
                    {shortDate(g.game_date)}
                  </td>
                  <td className="py-1.5 px-2">
                    {g.is_home === false && '@ '}
                    {g.opponent_id ? (
                      <SeasonLink to={`/teams/${g.opponent_id}`} className="text-blue-400 hover:underline">
                        {g.opponent_name ?? 'Unknown'}
                      </SeasonLink>
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
