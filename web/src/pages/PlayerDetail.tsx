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
import { ArchetypeBadge, SimilarPlayers } from '../components/Archetype';
import { campomTier, campomTierColor, campomSplit } from '../components/campom';
import { RAPM_DISPLAY_FLOOR } from '../components/onoff';
import { compareValues, type SortDir } from '../components/tableSort';
import { SortHeader, StickyHeader } from '../components/TableHeaders';
import { SeasonLink } from '../components/SeasonLink';
import { seasonHref, setPageSeasons, useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { useIsMobile } from '../components/useIsMobile';
import { resolveAxes } from '../components/radarAxes';
import { pctileTextColor } from '../components/pctile';
import { RadarAxisTooltip } from '../components/RadarAxisTooltip';
import { RadarTick } from '../components/RadarTick';
import { useDismissOnOutside } from '../components/useDismissOnOutside';

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

/// Play-by-play season profile: paint/perimeter shot mix, scoring-context
/// points, fouls drawn, and on-floor +/- — all derived from the PBP columns on
/// player_game_stats.
function PbpProfilePanel({ pbp }: { pbp: PlayerPbpProfile }) {
  const totalFga = pbp.paint_fga + pbp.perimeter_fga;
  const paintShare = totalFga > 0 ? pbp.paint_fga / totalFga : 0;
  const paintFg = pbp.paint_fga > 0 ? pbp.paint_fgm / pbp.paint_fga : null;
  const perimFg = pbp.perimeter_fga > 0 ? pbp.perimeter_fgm / pbp.perimeter_fga : null;

  const ord = (p: number) => {
    const n = Math.round(p * 100);
    const s = ['th', 'st', 'nd', 'rd'];
    const v = n % 100;
    return n + (s[(v - 20) % 10] || s[v] || s[0]);
  };
  // A FG% colored by its within-season percentile (rim finishing / jumper
  // efficiency are the non-redundant-with-shot-diet signal).
  const fgWithPct = (fg: number | null, p: number | null) =>
    fg == null ? null : (
      <span style={{ color: pctileTextColor(p) }}>
        {pct(fg)} FG{p != null && <span className="text-gray-500"> ({ord(p)})</span>}
      </span>
    );

  // Rate tile: the per-40 rate as the headline (colored by its percentile so
  // it's comparable across players), label, then "Nth pct · M total" so the raw
  // count is still there. Falls back to the raw count when there's no rate
  // (player below the percentile gate).
  const rateTile = (
    label: string,
    rate: number | null,
    p: number | null,
    raw: number,
  ) => (
    <div className="bg-gray-900 rounded p-3 text-center">
      <div
        className="text-lg font-bold tabular-nums"
        style={{ color: rate != null ? pctileTextColor(p) : undefined }}
      >
        {rate != null ? rate.toFixed(1) : raw}
      </div>
      <div className="text-xs text-gray-400 mt-0.5">
        {label}
        {rate != null && <span className="text-gray-600"> /40</span>}
      </div>
      <div className="text-[10px] text-gray-600 mt-0.5 tabular-nums">
        {p != null ? `${ord(p)} · ${raw} tot` : `${raw} total`}
      </div>
    </div>
  );

  return (
    <div className="bg-gray-800 rounded-lg p-5 mt-6">
      <div className="flex items-baseline justify-between flex-wrap gap-2 mb-3">
        <h2 className="text-lg font-bold">Play-by-Play Profile</h2>
        <span className="text-xs text-gray-500">{pbp.games} games with play-by-play</span>
      </div>

      {/* Shot mix: paint vs perimeter share of attempts; FG% colored by percentile */}
      {totalFga > 0 && (
        <div className="mb-4">
          <div className="flex justify-between text-xs text-gray-400 mb-1">
            <span>
              <span style={{ color: pctileTextColor(pbp.paint_rate_pct) }}>
                Paint {Math.round(paintShare * 100)}%
              </span>
              {paintFg != null && <span> · {fgWithPct(paintFg, pbp.paint_fg_pct_pct)}</span>}
            </span>
            <span>
              {perimFg != null && <span>{fgWithPct(perimFg, pbp.perimeter_fg_pct_pct)} · </span>}
              Perimeter {Math.round((1 - paintShare) * 100)}%
            </span>
          </div>
          <div className="flex h-3 rounded-full overflow-hidden bg-gray-700">
            <div className="bg-blue-500" style={{ width: `${paintShare * 100}%` }} />
            <div className="bg-purple-500" style={{ width: `${(1 - paintShare) * 100}%` }} />
          </div>
          <div className="text-xs text-gray-500 mt-1">
            {pbp.paint_fga} paint / {pbp.perimeter_fga} perimeter field-goal attempts
            {pbp.paint_rate_pct != null && (
              <span> · paint rate {ord(pbp.paint_rate_pct)} percentile</span>
            )}
          </div>
        </div>
      )}

      {/* Scoring context (per-40 rates, percentile-colored) + on-floor impact */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-2">
        {rateTile('Transition', pbp.transition_pts_per40, pbp.transition_pts_per40_pct, pbp.transition_pts)}
        {rateTile('2nd-chance', pbp.second_chance_pts_per40, pbp.second_chance_pts_per40_pct, pbp.second_chance_pts)}
        {rateTile('Pts off TO', pbp.points_off_turnovers_per40, pbp.points_off_turnovers_per40_pct, pbp.points_off_turnovers)}
        {rateTile('Fouls drawn', pbp.fouls_drawn_per40, pbp.fouls_drawn_per40_pct, pbp.fouls_drawn)}
        <div className="bg-gray-900 rounded p-3 text-center">
          <div className="text-lg font-bold tabular-nums">
            {pbp.plus_minus_pbp == null
              ? '—'
              : `${pbp.plus_minus_pbp > 0 ? '+' : ''}${pbp.plus_minus_pbp}`}
          </div>
          <div className="text-xs text-gray-400 mt-0.5">On-floor +/-</div>
        </div>
      </div>
      <p className="text-[10px] text-gray-600 mt-2">
        Per-40-minute rates; color and percentile rank vs all qualified players this season.
      </p>
    </div>
  );
}

/// On/off splits: team offense/defense per 100 possessions with vs without the
/// player on the floor (PBP-derived). The headline is the on−off net swing — the
/// classic player-value number. Off-court possessions can be thin for heavy-
/// minute starters, so we surface the off sample and caveat replay-sourced data.
function OnOffPanel({ onOff }: { onOff: PlayerOnOff }) {
  const signed = (v: number | null, d = 1) =>
    v == null ? '—' : `${v > 0 ? '+' : ''}${v.toFixed(d)}`;
  const netColor = (v: number | null) =>
    v == null
      ? 'text-gray-400'
      : v > 0
        ? 'text-green-400'
        : v < 0
          ? 'text-red-400'
          : 'text-gray-300';

  // Off-court possessions can be thin for a player who rarely sits → flag it.
  const offPoss = onOff.off_possessions_for + onOff.off_possessions_against;
  const thinOff = offPoss < 100;

  // Adj on/off (RAPM) companion line — display floor on the fit sample so a
  // garbage-time player's near-prior coefficient never headlines. (Can't use
  // adjOnOff() here — the panel payload names the field rapm_paired_possessions,
  // not the grids' rapm_paired_poss — but the floor constant is shared.)
  const showRapm =
    onOff.rapm_net != null &&
    (onOff.rapm_paired_possessions ?? 0) >= RAPM_DISPLAY_FLOOR;

  const row = (
    label: string,
    sub: string,
    ortg: number | null,
    drtg: number | null,
    net: number | null,
  ) => (
    <div className="grid grid-cols-4 gap-2 items-center py-2">
      <div>
        <div className="text-sm font-semibold">{label}</div>
        <div className="text-xs text-gray-500">{sub}</div>
      </div>
      <div className="text-right tabular-nums text-sm">{fmt(ortg)}</div>
      <div className="text-right tabular-nums text-sm">{fmt(drtg)}</div>
      <div className={`text-right tabular-nums text-sm font-medium ${netColor(net)}`}>
        {signed(net)}
      </div>
    </div>
  );

  return (
    <div className="bg-gray-800 rounded-lg p-5 mt-6">
      <div className="flex items-baseline justify-between flex-wrap gap-2 mb-1">
        <h2 className="text-lg font-bold">On / Off Splits</h2>
        <span className="text-xs text-gray-500">{onOff.games} games</span>
      </div>
      <p className="text-xs text-gray-500 mb-3">
        Team rating per 100 possessions with the player on the floor vs on the bench (same games).
      </p>

      {/* Headline: net on/off swing */}
      <div className="flex items-baseline gap-2 mb-4">
        <span className={`text-3xl font-bold tabular-nums ${netColor(onOff.net_on_off)}`}>
          {signed(onOff.net_on_off)}
        </span>
        <span className="text-sm text-gray-400">net on/off (per 100 poss)</span>
      </div>

      <div className="grid grid-cols-4 gap-2 text-xs text-gray-400 border-b border-gray-700 pb-1">
        <div />
        <div className="text-right">ORtg</div>
        <div className="text-right">DRtg</div>
        <div className="text-right">Net</div>
      </div>
      <div className="divide-y divide-gray-700/50">
        {row('On floor', `${onOff.on_minutes.toFixed(0)} min`, onOff.on_ortg, onOff.on_drtg, onOff.on_net_rtg)}
        {row('Off floor', `${onOff.off_minutes.toFixed(0)} min`, onOff.off_ortg, onOff.off_drtg, onOff.off_net_rtg)}
      </div>

      {showRapm && (
        <div className="mt-4 pt-3 border-t border-gray-700">
          <div className="flex items-baseline gap-2">
            <span className={`text-2xl font-bold tabular-nums ${netColor(onOff.rapm_net)}`}>
              {signed(onOff.rapm_net)}
            </span>
            <span className="text-sm text-gray-400">
              adj on/off (RAPM)
            </span>
            <span className="text-xs text-gray-500 tabular-nums">
              O {signed(onOff.rapm_o)} / D {signed(onOff.rapm_d)}
            </span>
          </div>
          <p className="text-xs text-gray-600 mt-1">
            The same per-100 swing with teammates and opponents held constant (ridge-regressed
            adjusted +/- over every stint) — removes the deep-team garbage-time bias raw on/off
            carries. Stabilized with the player's prior-season stints at decayed weight, so it
            reads career-informed, not season-pure. Negative D is good (points allowed below
            average).
          </p>
        </div>
      )}

      {thinOff && (
        <p className="text-xs text-amber-500/80 mt-3">
          Small off-court sample ({Math.round(offPoss)} poss) — the split is noisy for a player
          who rarely leaves the floor.
        </p>
      )}
      {onOff.source === 'replay' && (
        <p className="text-xs text-gray-500 mt-2">
          Lineups reconstructed from substitution play-by-play (~86% accurate); on/off is approximate.
        </p>
      )}
      <p className="text-xs text-gray-600 mt-2">
        On/off is a team-result measure — it reflects whoever else is on the floor, so a strong
        player can read negative on a deep team (the bench may feast in garbage time).
        {showRapm ? ' The adjusted line above controls for that;' : ''} read it alongside CamPom,
        not instead of it.
      </p>
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
  const [similar, setSimilar] = useState<SimilarPlayer[]>([]);
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
        if (r.archetype) {
          fetchPlayerSimilar(r.player.id, 8, season)
            .then((s) => {
              if (!cancelled) setSimilar(s.players);
            })
            .catch(() => {
              if (!cancelled) setSimilar([]);
            });
        } else {
          setSimilar([]);
        }
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
        setSimilar([]);
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id, season, navigate]);

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
              const tier = campomTier(torvik.campom);
              const pctStr = torvik.campom_pct != null ? Math.round(torvik.campom_pct * 100) : null;
              const split = campomSplit(torvik.campom_o, torvik.campom_d);
              return (
                <span
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border ${campomTierColor(tier)}`}
                  title={
                    'CamPom: composite player valuation. ' +
                    (split
                      ? 'O/D halves sum to the total; D is positive-good (defensive value added). '
                      : '') +
                    'See methodology in docs/campom_methodology.md.'
                  }
                >
                  <span className="text-xs uppercase tracking-wide opacity-70">CamPom</span>
                  <span className="font-bold">{torvik.campom.toFixed(1)}</span>
                  {split && <span className="text-xs opacity-80">{split}</span>}
                  {pctStr != null && <span className="text-xs opacity-80">{pctStr} pct</span>}
                  {tier && <span className="text-xs opacity-80">· {tier}</span>}
                </span>
              );
            })()}
            {trajectory && (() => {
              // Phase 5c growth-model projection. Pooled LOPO MAE ~2.1
              // CamPom points; the q=0.1 / q=0.9 band is what users
              // should read for "how confident" — a wide band on a
              // freshman with thin signal is correct, not a flaw. Tier
              // colors track the projected mean so the chip visually
              // reflects projected quality without needing a separate
              // percentile lookup.
              const tier = campomTier(trajectory.projected_mean);
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
              // when current CamPom enters the bias zone — ≥10 gets a
              // mild note, ≥15 gets the full "read the ceiling" prompt.
              // Per-bucket MAE / bias lives in
              // `trajectory_model_meta.json::mae_by_current_campom`.
              const regressionNote =
                trajectory.prior_campom != null && trajectory.prior_campom >= 15
                  ? ' Regression-to-the-mean: elite inputs project ≈2 CamPom below current — mostly real regression (residual bias vs actual ≈−0.7 on ≥+15; +20+ inputs are extrapolation beyond training). Read the q90 ceiling for the optimistic case.'
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
                  className={`inline-flex items-baseline gap-2 px-2.5 py-0.5 rounded border border-dashed ${campomTierColor(tier)} hover:bg-gray-700/40 transition-colors`}
                  title={`Projected next-season CamPom. Mean ${trajectory.projected_mean.toFixed(2)}, 80% band ${band}. Pooled backtest MAE ≈ 2.1 — read this as directional, not a point estimate. Wide bands flag thin signal (e.g. freshmen, low-minute returners).${regressionNote} Click for full career progression.`}
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
            {player.conference && <span className="text-gray-500">({player.conference})</span>}
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

            {torvik && (
              <>
                <h2 className="text-lg font-bold mt-5 mb-3">Advanced Metrics</h2>
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

      {/* Play-by-Play Profile (shot location, scoring context, on-floor +/-) */}
      {pbp && <PbpProfilePanel pbp={pbp} />}

      {/* On/off splits (team rating with vs without the player) */}
      {onOff && <OnOffPanel onOff={onOff} />}

      {/* Similar Players */}
      {similar.length > 0 && (
        <SimilarPlayers players={similar} currentPlayerId={player.id} />
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
              <StickyHeader align="right" className="hidden sm:table-cell">FG</StickyHeader>
              <StickyHeader align="right" className="hidden sm:table-cell">3P</StickyHeader>
              <SortHeader label="REB" sortKey="total_rebounds" current={sort} onSort={onSort} align="right" />
              <SortHeader label="AST" sortKey="assists" current={sort} onSort={onSort} align="right" />
              <SortHeader label="STL" sortKey="steals" current={sort} onSort={onSort} align="right" className="hidden sm:table-cell" />
              <SortHeader label="BLK" sortKey="blocks" current={sort} onSort={onSort} align="right" className="hidden sm:table-cell" />
              <SortHeader label="TO" sortKey="turnovers" current={sort} onSort={onSort} align="right" className="hidden sm:table-cell" />
              <SortHeader label="GmSc" sortKey="game_score" current={sort} onSort={onSort} align="right" className="hidden sm:table-cell" />
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
                  <td className="hidden sm:table-cell py-1.5 px-2 text-right">{g.fgm != null ? `${g.fgm}-${g.fga}` : '—'}</td>
                  <td className="hidden sm:table-cell py-1.5 px-2 text-right">{g.tpm != null ? `${g.tpm}-${g.tpa}` : '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.total_rebounds ?? '—'}</td>
                  <td className="py-1.5 px-2 text-right">{g.assists ?? '—'}</td>
                  <td className="hidden sm:table-cell py-1.5 px-2 text-right">{g.steals ?? '—'}</td>
                  <td className="hidden sm:table-cell py-1.5 px-2 text-right">{g.blocks ?? '—'}</td>
                  <td className="hidden sm:table-cell py-1.5 px-2 text-right">{g.turnovers ?? '—'}</td>
                  <td className={`hidden sm:table-cell py-1.5 px-2 text-right ${gmscHot ? 'text-green-400' : ''}`}>
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
