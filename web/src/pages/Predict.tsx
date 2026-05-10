import { useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  fetchPrediction,
  fetchTeamRankings,
  type FeatureContribution,
  type PlayerGameBox,
  type PredictionResult,
  type PriorMeeting,
  type RosterEntry,
  type TeamGameBox,
  type TeamRanking,
  type Venue,
} from '../api/client';
import { useSeason, seasonHref } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { FLAG_FEATURES, homeAdvantageSign } from '../components/featureExplanations';
import { campomTier, campomTierColor } from '../components/campom';
import { classColor, classTitle } from '../components/archetypeColors';
import { shortDate } from '../components/format';
import { Link } from 'react-router-dom';

const TEAM_1_COLOR = '#3b82f6'; // blue (matches PlayerCompare PLAYER_COLORS[0])
const TEAM_2_COLOR = '#ef4444'; // red

/// |feature value| below this counts as "essentially tied" — the underlying
/// stat is too close between the two teams to point at as a concrete
/// advantage. Tied features can't be keys-panel headlines (we'd be
/// announcing an edge with no stat to back it up) and `formatHeadlineGap`
/// uses the same threshold to decide when to print "(teams essentially
/// tied on this stat)" instead of a numeric gap.
const TIED_VALUE_THRESHOLD = 0.005;

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [searchParams] = useSearchParams();
  const urlHome = searchParams.get('home') ?? '';
  const urlAway = searchParams.get('away') ?? '';
  const urlVenue = searchParams.get('venue') as Venue | null;
  const initialVenue: Venue =
    urlVenue === 'home' || urlVenue === 'away' || urlVenue === 'neutral' ? urlVenue : 'home';

  const [team1, setTeam1] = useState(urlHome);
  const [team2, setTeam2] = useState(urlAway);
  const [venue, setVenue] = useState<Venue>(initialVenue);
  const [result, setResult] = useState<PredictionResult | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  // Pull the team list once for autocomplete on both pickers. Rankings is
  // already keyed by season and lightweight (~360 rows for D-I), so client-
  // side filtering is simpler than a per-keystroke server search.
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  useEffect(() => {
    let alive = true;
    fetchTeamRankings(season)
      .then((r) => {
        if (alive) setTeams(r.teams);
      })
      .catch(() => {
        // Picker still works; fallback to free-text typing.
      });
    return () => {
      alive = false;
    };
  }, [season]);

  // When teams arrive via URL params (deep-link from a schedule row, ticker
  // tile, or shared link), kick off the prediction automatically. Re-fires
  // when the URL or season changes so /predict?home=A&away=B remains a
  // first-class destination.
  useEffect(() => {
    if (!urlHome.trim() || !urlAway.trim()) return;
    setTeam1(urlHome);
    setTeam2(urlAway);
    setVenue(initialVenue);
    let alive = true;
    setLoading(true);
    setError('');
    setResult(null);
    fetchPrediction(urlHome.trim(), urlAway.trim(), initialVenue, season)
      .then((r) => {
        if (alive) setResult(r);
      })
      .catch((err) => {
        if (alive) setError(err instanceof Error ? err.message : 'Prediction failed');
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
    // The early-return on empty `urlHome`/`urlAway` short-circuits the first
    // render before pickers have any value. `initialVenue` is intentionally
    // omitted from the deps — it's recomputed each render from `urlVenue`
    // (which is in the deps), so reading its current value inside the effect
    // is correct.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [urlHome, urlAway, urlVenue, season]);

  const handleSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!team1.trim() || !team2.trim()) return;
    setLoading(true);
    setError('');
    setResult(null);
    try {
      const r = await fetchPrediction(team1.trim(), team2.trim(), venue, season);
      setResult(r);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Prediction failed');
    } finally {
      setLoading(false);
    }
  };

  const team1Prob = result ? result.home_win_probability * 100 : 50;

  const venueLabel: Record<Venue, string> = {
    home: team1.trim() ? `${team1.trim()} home` : 'Team 1 home',
    neutral: 'Neutral',
    away: team2.trim() ? `${team2.trim()} home` : 'Team 2 home',
  };

  return (
    <div className="max-w-4xl mx-auto">
      <h1 className="text-2xl font-bold mb-1">Game Prediction</h1>
      <p className="text-xs text-gray-500 mb-5">
        Predicting matchups in the{' '}
        <span className="text-gray-300">
          {season - 1}-{String(season).slice(2)}
        </span>{' '}
        season. Switch the season selector in the nav to back-test historical games.
      </p>

      <form onSubmit={handleSubmit} className="bg-gray-800 rounded-lg p-6 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <TeamPicker
            label="Team 1"
            value={team1}
            onChange={setTeam1}
            teams={teams}
            placeholder="e.g. Duke"
            color={TEAM_1_COLOR}
          />
          <TeamPicker
            label="Team 2"
            value={team2}
            onChange={setTeam2}
            teams={teams}
            placeholder="e.g. Michigan"
            color={TEAM_2_COLOR}
          />
        </div>

        <div>
          <label className="block text-sm text-gray-400 mb-1.5">Venue</label>
          <div
            className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-sm w-full sm:w-auto"
            role="radiogroup"
            aria-label="Game venue"
          >
            {(['home', 'neutral', 'away'] as const).map((v) => (
              <button
                key={v}
                type="button"
                role="radio"
                aria-checked={venue === v}
                onClick={() => setVenue(v)}
                className={`flex-1 sm:flex-none px-3 py-1.5 ${
                  venue === v
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-900 text-gray-300 hover:bg-gray-700'
                }`}
              >
                {venueLabel[v]}
              </button>
            ))}
          </div>
        </div>

        <button
          type="submit"
          disabled={loading || !team1.trim() || !team2.trim()}
          className="w-full bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white font-medium py-2.5 rounded transition-colors"
        >
          {loading ? 'Predicting...' : 'Predict'}
        </button>
      </form>

      {error && (
        <div className="mt-4 bg-red-900/50 border border-red-800 rounded-lg p-4 text-red-300">
          {error}
        </div>
      )}

      {result && (
        <div className="mt-6 space-y-4">
          <ResultHeadline result={result} team1Prob={team1Prob} />
          <RosterCompare result={result} />
          <KeysToGame result={result} />
          <SideBySideStats result={result} teams={teams} />
          <FourFactorsPanel result={result} teams={teams} />
          <PreviousMatchups result={result} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Roster Compare panel — embedded TeamCompare. Shipped before the §5b radial
// plot lands, so for now it's a side-by-side roster table (top 8 by CamPom
// per team) with archetype chips and rate stats. The radial-roster overlay
// from §5b drops into this same component when it ships.
// ---------------------------------------------------------------------------

const ROSTER_PANEL_LIMIT = 8;

function RosterCompare({ result }: { result: PredictionResult }) {
  const homeTop = useMemo(
    () => result.roster_home.slice(0, ROSTER_PANEL_LIMIT),
    [result.roster_home],
  );
  const awayTop = useMemo(
    () => result.roster_away.slice(0, ROSTER_PANEL_LIMIT),
    [result.roster_away],
  );

  if (homeTop.length === 0 && awayTop.length === 0) return null;

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Roster Compare
        </h2>
        <div className="text-[11px] text-gray-500">Top {ROSTER_PANEL_LIMIT} by CamPom</div>
      </div>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-x-6 gap-y-4">
        <RosterColumn
          teamName={result.home_team}
          teamId={result.home_team_id}
          season={result.season}
          color={TEAM_1_COLOR}
          roster={homeTop}
        />
        <RosterColumn
          teamName={result.away_team}
          teamId={result.away_team_id}
          season={result.season}
          color={TEAM_2_COLOR}
          roster={awayTop}
        />
      </div>
    </div>
  );
}

function RosterColumn({
  teamName,
  teamId,
  season,
  color,
  roster,
}: {
  teamName: string;
  teamId: string;
  season: number;
  color: string;
  roster: RosterEntry[];
}) {
  return (
    <div>
      <div className="flex items-baseline justify-between mb-2 pb-2 border-b border-gray-700">
        <Link
          to={seasonHref(`/teams/${teamId}`, season)}
          className="text-base font-semibold hover:underline"
          style={{ color }}
        >
          {teamName}
        </Link>
        <span className="text-[11px] text-gray-500 uppercase tracking-wide">
          {roster.length} {roster.length === 1 ? 'player' : 'players'}
        </span>
      </div>
      <ul className="space-y-1.5">
        {roster.map((p) => (
          <RosterRow key={p.player_id} p={p} season={season} />
        ))}
        {roster.length === 0 && (
          <li className="text-xs text-gray-500">No qualified roster data.</li>
        )}
      </ul>
    </div>
  );
}

function RosterRow({ p, season }: { p: RosterEntry; season: number }) {
  const tier = campomTier(p.campom);
  const tierColor = campomTierColor(tier);
  const mpg = p.minutes_per_game != null ? p.minutes_per_game.toFixed(1) : '—';
  const campomScore = p.campom != null ? p.campom.toFixed(1) : '—';
  return (
    <li className="grid grid-cols-[1fr_auto_auto] items-center gap-2 text-sm">
      <div className="min-w-0 truncate">
        <Link
          to={seasonHref(`/players/${p.player_id}`, season)}
          className="text-gray-100 hover:underline truncate"
        >
          {p.name}
        </Link>
        {p.primary_class && (
          <span
            title={classTitle(p.primary_class)}
            className="ml-1.5 text-[10px] uppercase tracking-wide font-semibold"
            style={{ color: classColor(p.primary_class) }}
          >
            {p.primary_class.slice(0, 3)}
          </span>
        )}
      </div>
      <div className="text-[11px] text-gray-500 font-mono whitespace-nowrap">
        {mpg} mpg · {p.games_played} gp
      </div>
      <div
        className={`text-[11px] font-mono px-1.5 py-0.5 rounded border whitespace-nowrap ${tierColor}`}
        title={tier ? `${tier}` : undefined}
      >
        {campomScore}
      </div>
    </li>
  );
}

// ---------------------------------------------------------------------------
// Previous Matchups — embedded section. When the two teams have already
// played this season, render one card per meeting: headline (final, top
// performer per side) + collapsible full box score.
// ---------------------------------------------------------------------------

function PreviousMatchups({ result }: { result: PredictionResult }) {
  if (result.prior_meetings.length === 0) return null;
  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Previous Matchups
        </h2>
        <div className="text-[11px] text-gray-500">
          {result.prior_meetings.length}{' '}
          {result.prior_meetings.length === 1 ? 'meeting' : 'meetings'} this season
        </div>
      </div>
      <div className="space-y-3">
        {result.prior_meetings.map((m) => (
          <MeetingCard key={m.headline.game_id} meeting={m} result={result} />
        ))}
      </div>
    </div>
  );
}

function MeetingCard({
  meeting,
  result,
}: {
  meeting: PriorMeeting;
  result: PredictionResult;
}) {
  const [expanded, setExpanded] = useState(false);
  const h = meeting.headline;

  // Color sides by which team they correspond to in the *current* prediction
  // (home_team_id vs away_team_id), not by which team hosted the prior game.
  // Keeps the visual frame consistent with the headline / probability bar at
  // the top of the page.
  const headIsResultHome = h.home_team_id === result.home_team_id;
  const homeColor = headIsResultHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  const awayColor = headIsResultHome ? TEAM_2_COLOR : TEAM_1_COLOR;

  const homeWon =
    h.home_score != null && h.away_score != null && h.home_score > h.away_score;
  const awayWon =
    h.home_score != null && h.away_score != null && h.away_score > h.home_score;

  const venueText = h.is_neutral_site
    ? 'Neutral site'
    : `at ${h.home_team_name ?? '—'}`;

  // Top performer per side: highest game_score among players who logged
  // minutes for that team. Falls back to highest points if game_score is
  // unpopulated (legacy rows).
  const topHome = topPerformer(meeting.player_box, h.home_team_id);
  const topAway = topPerformer(meeting.player_box, h.away_team_id);

  return (
    <div className="bg-gray-900 rounded border border-gray-700 overflow-hidden">
      <div className="p-4 space-y-2">
        <div className="flex items-baseline justify-between text-[11px] text-gray-500 uppercase tracking-wide">
          <span>{shortDate(h.game_date)}</span>
          <span>
            {venueText}
            {h.is_postseason && ' · Postseason'}
          </span>
        </div>
        <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3">
          <div className="text-right">
            <div className="font-semibold" style={{ color: awayColor }}>
              {h.away_team_name ?? '—'}
            </div>
            {topAway && (
              <div className="text-[11px] text-gray-400 mt-0.5 truncate">
                {topAway.player_name} · {statLine(topAway)}
              </div>
            )}
          </div>
          <div className="font-mono text-lg whitespace-nowrap">
            <span className={awayWon ? 'text-gray-100 font-bold' : 'text-gray-400'}>
              {h.away_score ?? '—'}
            </span>
            <span className="text-gray-600 mx-1.5">–</span>
            <span className={homeWon ? 'text-gray-100 font-bold' : 'text-gray-400'}>
              {h.home_score ?? '—'}
            </span>
          </div>
          <div className="text-left">
            <div className="font-semibold" style={{ color: homeColor }}>
              {h.home_team_name ?? '—'}
            </div>
            {topHome && (
              <div className="text-[11px] text-gray-400 mt-0.5 truncate">
                {topHome.player_name} · {statLine(topHome)}
              </div>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          className="text-[11px] text-blue-400 hover:text-blue-300 hover:underline"
        >
          {expanded ? 'Hide full box score' : 'Show full box score'}
        </button>
      </div>
      {expanded && <BoxScore meeting={meeting} homeColor={homeColor} awayColor={awayColor} />}
    </div>
  );
}

function topPerformer(
  players: PlayerGameBox[],
  teamId: string | null,
): PlayerGameBox | null {
  if (!teamId) return null;
  const eligible = players.filter((p) => p.team_id === teamId && (p.minutes ?? 0) > 0);
  if (eligible.length === 0) return null;
  // Pick a single key for the whole team so we never compare game_score on
  // one player to points on another (different scales). Use game_score if
  // every player has it (the common case — compute populates it for all
  // rows); otherwise fall back to points uniformly.
  const useGameScore = eligible.every((p) => p.game_score != null);
  const sortKey = (p: PlayerGameBox): number =>
    (useGameScore ? p.game_score : p.points) ?? -Infinity;
  return eligible.reduce((best, p) => (sortKey(p) > sortKey(best) ? p : best));
}

/// Compact "P / R / A" line for the top-performer chip on a Previous Matchup
/// card. Renders `—` for null fields so a row doesn't claim a real "0" stat
/// line when the underlying data is missing.
function statLine(p: PlayerGameBox): string {
  const fmt = (v: number | null) => (v == null ? '—' : v.toString());
  return `${fmt(p.points)}p / ${fmt(p.total_rebounds)}r / ${fmt(p.assists)}a`;
}

function BoxScore({
  meeting,
  homeColor,
  awayColor,
}: {
  meeting: PriorMeeting;
  homeColor: string;
  awayColor: string;
}) {
  const h = meeting.headline;
  const homeId = h.home_team_id;
  const awayId = h.away_team_id;
  const homePlayers = meeting.player_box.filter(
    (p) => p.team_id === homeId && (p.minutes ?? 0) > 0,
  );
  const awayPlayers = meeting.player_box.filter(
    (p) => p.team_id === awayId && (p.minutes ?? 0) > 0,
  );
  const homeTeamBox = meeting.team_box.find((b) => b.team_id === homeId);
  const awayTeamBox = meeting.team_box.find((b) => b.team_id === awayId);

  return (
    <div className="border-t border-gray-700 bg-gray-950/40 p-4 space-y-4">
      <BoxScoreSide
        teamName={h.away_team_name ?? '—'}
        color={awayColor}
        players={awayPlayers}
        teamBox={awayTeamBox}
      />
      <BoxScoreSide
        teamName={h.home_team_name ?? '—'}
        color={homeColor}
        players={homePlayers}
        teamBox={homeTeamBox}
      />
    </div>
  );
}

function BoxScoreSide({
  teamName,
  color,
  players,
  teamBox,
}: {
  teamName: string;
  color: string;
  players: PlayerGameBox[];
  teamBox?: TeamGameBox;
}) {
  return (
    <div>
      <div className="text-sm font-semibold mb-2" style={{ color }}>
        {teamName}
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-xs font-mono">
          <thead>
            <tr className="text-gray-500 border-b border-gray-700">
              <th className="text-left py-1.5 px-2 font-medium">Player</th>
              <th className="text-right py-1.5 px-1 font-medium">MIN</th>
              <th className="text-right py-1.5 px-1 font-medium">PTS</th>
              <th className="text-right py-1.5 px-1 font-medium">FG</th>
              <th className="text-right py-1.5 px-1 font-medium">3P</th>
              <th className="text-right py-1.5 px-1 font-medium">FT</th>
              <th className="text-right py-1.5 px-1 font-medium">REB</th>
              <th className="text-right py-1.5 px-1 font-medium">AST</th>
              <th className="text-right py-1.5 px-1 font-medium">STL</th>
              <th className="text-right py-1.5 px-1 font-medium">BLK</th>
              <th className="text-right py-1.5 px-1 font-medium">TO</th>
            </tr>
          </thead>
          <tbody>
            {players.map((p) => (
              <tr key={p.player_id} className="border-b border-gray-800/60">
                <td className="text-left py-1 px-2 text-gray-200 font-sans">
                  {p.player_name}
                  {p.starter && (
                    <span
                      className="text-gray-500 ml-1"
                      title="Starter"
                      aria-label="Starter"
                    >
                      *
                    </span>
                  )}
                </td>
                <td className="text-right py-1 px-1 text-gray-300">
                  {p.minutes != null ? Math.round(p.minutes) : '—'}
                </td>
                <td className="text-right py-1 px-1 text-gray-100">{p.points ?? '—'}</td>
                <td className="text-right py-1 px-1 text-gray-300">
                  {p.fgm ?? '—'}-{p.fga ?? '—'}
                </td>
                <td className="text-right py-1 px-1 text-gray-300">
                  {p.tpm ?? '—'}-{p.tpa ?? '—'}
                </td>
                <td className="text-right py-1 px-1 text-gray-300">
                  {p.ftm ?? '—'}-{p.fta ?? '—'}
                </td>
                <td className="text-right py-1 px-1 text-gray-300">{p.total_rebounds ?? '—'}</td>
                <td className="text-right py-1 px-1 text-gray-300">{p.assists ?? '—'}</td>
                <td className="text-right py-1 px-1 text-gray-300">{p.steals ?? '—'}</td>
                <td className="text-right py-1 px-1 text-gray-300">{p.blocks ?? '—'}</td>
                <td className="text-right py-1 px-1 text-gray-300">{p.turnovers ?? '—'}</td>
              </tr>
            ))}
            {teamBox && (
              <tr className="bg-gray-900/60 font-semibold">
                <td className="text-left py-1.5 px-2 text-gray-200 uppercase tracking-wide text-[10px] font-sans">
                  Team
                </td>
                <td />
                <td className="text-right py-1.5 px-1 text-gray-100">{teamBox.points ?? '—'}</td>
                <td className="text-right py-1.5 px-1 text-gray-300">
                  {teamBox.fgm ?? '—'}-{teamBox.fga ?? '—'}
                </td>
                <td className="text-right py-1.5 px-1 text-gray-300">
                  {teamBox.tpm ?? '—'}-{teamBox.tpa ?? '—'}
                </td>
                <td className="text-right py-1.5 px-1 text-gray-300">
                  {teamBox.ftm ?? '—'}-{teamBox.fta ?? '—'}
                </td>
                <td className="text-right py-1.5 px-1 text-gray-300">
                  {teamBox.total_rebounds ?? '—'}
                </td>
                <td className="text-right py-1.5 px-1 text-gray-300">{teamBox.assists ?? '—'}</td>
                <td className="text-right py-1.5 px-1 text-gray-300">{teamBox.steals ?? '—'}</td>
                <td className="text-right py-1.5 px-1 text-gray-300">{teamBox.blocks ?? '—'}</td>
                <td className="text-right py-1.5 px-1 text-gray-300">
                  {teamBox.turnovers ?? '—'}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Keys to the Game
// ---------------------------------------------------------------------------

interface Key {
  group: string;
  team: string;
  towardHome: boolean;
  /// Sum of |contribution| across all features in this group — how much
  /// the model leaned on this group of stats overall, ignoring direction.
  /// Used for tier classification and ranking.
  importance: number;
  tier: 'slight' | 'clear' | 'meaningful' | 'decisive';
  headline?: FeatureContribution;
}

const TIER_PHRASE: Record<Key['tier'], string> = {
  slight: 'has a slight nudge',
  clear: 'holds an edge',
  meaningful: 'has a meaningful advantage',
  decisive: 'dominates',
};

const TIER_BADGE: Record<Key['tier'], string> = {
  slight: 'bg-gray-700/60 text-gray-300',
  clear: 'bg-blue-900/40 text-blue-200 ring-1 ring-blue-500/40',
  meaningful: 'bg-amber-900/40 text-amber-200 ring-1 ring-amber-500/50',
  decisive: 'bg-rose-900/50 text-rose-200 ring-1 ring-rose-500/60',
};

/// Per-group tier thresholds [hidden→slight, slight→clear, clear→meaningful,
/// meaningful→decisive]. Base thresholds [0.3, 0.6, 1.2, 2.0] were
/// calibrated against typical |SHAP|-sums of ~0.3–1.0, the regime most
/// groups actually live in.
///
/// **Roster impact** (the GBPM family — w_gbpm, w_ogbpm, w_dgbpm) is
/// special: the model leans on it heavily and its |SHAP|-sums routinely
/// land 5–15. Under base thresholds it would tier `decisive` on
/// virtually every matchup and drown out the rest of the keys. Custom
/// thresholds spread the tier ladder across its actual distribution,
/// reserving `decisive` for true talent blowouts.
///
/// Calibration data: 80-matchup random-pair sample on 2026 (neutral).
/// Roster impact distribution: p25=3.72, median=8.15, p75=13.97,
/// p90=24.05, max=26.6. Custom thresholds map roughly to:
///   `slight` ≈ below-p25 (close matchups where roster impact is small)
///   `clear` ≈ p25–p50 (typical talent gap)
///   `meaningful` ≈ p50–p75 (above-typical, real gap)
///   `decisive` ≈ p75+ (blowout-level talent gap)
const TIER_THRESHOLDS: Record<string, readonly [number, number, number, number]> = {
  default: [0.3, 0.6, 1.2, 2.0],
  'Roster impact': [1.0, 4.0, 8.0, 14.0],
};

function tierFor(group: string, mag: number): Key['tier'] | null {
  const [t1, t2, t3, t4] = TIER_THRESHOLDS[group] ?? TIER_THRESHOLDS.default;
  if (mag < t1) return null;
  if (mag < t2) return 'slight';
  if (mag < t3) return 'clear';
  if (mag < t4) return 'meaningful';
  return 'decisive';
}

/// Coarse "nature" of each feature group for the diversity rerank in
/// `selectTopKeys`. The keys panel reads better when its 4 cards span
/// different angles of the matchup (talent, tactical, situational,
/// synthesis) rather than three roster-flavored cards in a row. Anything
/// not listed here defaults to 'other' and gets no diversity penalty.
type Nature = 'talent' | 'tactical' | 'situational' | 'synthesis' | 'other';
const GROUP_NATURE: Record<string, Nature> = {
  'Roster impact': 'talent',
  'Roster aggregate': 'talent',
  'Star player': 'talent',
  'Four factors (offense)': 'tactical',
  'Four factors (defense)': 'tactical',
  Pace: 'tactical',
  Context: 'situational',
  'Recent form': 'situational',
  'Strength of schedule': 'situational',
  'Adjusted efficiency': 'synthesis',
  'Power ratings': 'synthesis',
};

/// Pick the top `maxKeys` keys with a soft diversity penalty: each pick
/// discounts remaining same-nature candidates by `DIVERSITY_DECAY^seen`.
/// When natures don't conflict (every key is from a different bucket),
/// this reduces to plain importance ranking. When several keys cluster in
/// one nature (typical: 2–3 talent groups in the top of the list), the
/// penalty pulls in a key from an under-represented nature instead.
///
/// 0.5 was tuned by replaying ~10 sample matchups: aggressive enough to
/// swap an under-tier tactical/situational key in over a same-nature
/// duplicate when the magnitudes are within ~2x; gentle enough that a
/// dominant nature (e.g. blowout-tier Roster impact at importance 20+)
/// keeps multiple slots if it genuinely outranks everything else.
const DIVERSITY_DECAY = 0.5;

/// When the largest-|SHAP| candidate in a group disagrees with the
/// group's net direction, look for a concordant alternative whose
/// |SHAP| is at least `(1 - this)` of the max. If found, prefer the
/// concordant one as headline.
///
/// Why: on close groups where many features have similar |SHAP|, the
/// "max wins" rule degenerates into picking a noise-driven headline
/// that often happens to disagree with the group's direction, producing
/// awkward "(gap of X the other way)" phrasings. When the model isn't
/// strongly differentiating among features, breaking ties by direction
/// concordance gives a cleaner narrative without hiding genuine signal:
/// if one feature really dominates the group's |SHAP| (more than ~33%
/// larger than the next), the discordant headline still wins and the
/// "(the other way)" framing fires — flagging that the model's biggest
/// signal in this group bucks the data narrative.
const CONCORDANT_HEADLINE_TOLERANCE = 0.25;

/// Pick the headline feature for a key. Default = max |SHAP|. When that
/// candidate's data direction disagrees with the group's net direction
/// AND another candidate within `CONCORDANT_HEADLINE_TOLERANCE` of its
/// magnitude agrees, prefer the concordant one. Flag features (no
/// directional concept) are eligible as the max-|SHAP| pick but never
/// swap in as concordant alternatives.
function pickHeadline(
  candidates: FeatureContribution[],
  towardHome: boolean,
): FeatureContribution {
  const max = candidates.reduce((best, f) =>
    Math.abs(f.contribution) > Math.abs(best.contribution) ? f : best,
  );
  const maxSign = homeAdvantageSign(max.name, max.value);
  // maxSign === 0 ⇒ flag feature; keep it (formatHeadlineGap special-cases it).
  if (maxSign === 0 || (maxSign > 0) === towardHome) return max;

  const threshold = (1 - CONCORDANT_HEADLINE_TOLERANCE) * Math.abs(max.contribution);
  let bestConcordant: FeatureContribution | null = null;
  for (const f of candidates) {
    if (Math.abs(f.contribution) < threshold) continue;
    const sign = homeAdvantageSign(f.name, f.value);
    if (sign === 0) continue; // flags don't count as concordant alternatives
    if ((sign > 0) !== towardHome) continue;
    if (
      bestConcordant === null ||
      Math.abs(f.contribution) > Math.abs(bestConcordant.contribution)
    ) {
      bestConcordant = f;
    }
  }
  return bestConcordant ?? max;
}

function selectTopKeys(allKeys: Key[], maxKeys = 4): Key[] {
  const remaining = [...allKeys];
  const picked: Key[] = [];
  const natureCount: Partial<Record<Nature, number>> = {};

  while (picked.length < maxKeys && remaining.length > 0) {
    let bestIdx = -1;
    let bestScore = -Infinity;
    for (let i = 0; i < remaining.length; i++) {
      const nature: Nature = GROUP_NATURE[remaining[i].group] ?? 'other';
      const seen = natureCount[nature] ?? 0;
      const score = remaining[i].importance * Math.pow(DIVERSITY_DECAY, seen);
      if (score > bestScore) {
        bestScore = score;
        bestIdx = i;
      }
    }
    if (bestIdx < 0) break;
    const [pick] = remaining.splice(bestIdx, 1);
    picked.push(pick);
    const nature: Nature = GROUP_NATURE[pick.group] ?? 'other';
    natureCount[nature] = (natureCount[nature] ?? 0) + 1;
  }

  // Display order: by raw importance desc — keep the strongest key
  // visually first regardless of selection order.
  return picked.sort((a, b) => b.importance - a.importance);
}

/// Build the keys panel from the per-feature contributions.
///
/// **Direction** (which team has the edge) comes from the **data**: each
/// feature's diff sign mapped through `homeAdvantageSign` to handle
/// "lower is better" stats and 0/1 flags. **Importance** (how much it
/// matters) comes from the **model** as `|SHAP contribution|`. The split
/// keeps the panel a stats-narrative — the named leader is always the
/// team with the better underlying stat, even when the model has learned
/// a non-monotonic interaction that flips the SHAP sign for that feature
/// (e.g. Purdue vs Michigan's opp eFG%, where Michigan's data edge of
/// 0.079 ought to be credited to Michigan even if SHAP attributes that
/// feature toward Purdue). TreeSHAP gives us cleaner additive importances
/// than ablation, but it doesn't replace the data-direction lookup.
function generateKeys(result: PredictionResult): Key[] {
  // Aggregate per-group: data direction × |SHAP| → signed importance for
  // who-leads, |SHAP| → magnitude for how-much-matters.
  const groupAgg = new Map<
    string,
    {
      signedImportance: number;
      importance: number;
      features: FeatureContribution[];
    }
  >();
  for (const f of result.feature_contributions) {
    const advantage = homeAdvantageSign(f.name, f.value);
    const mag = Math.abs(f.contribution);
    const entry = groupAgg.get(f.group) ?? {
      signedImportance: 0,
      importance: 0,
      features: [],
    };
    entry.signedImportance += advantage * mag;
    entry.importance += mag;
    entry.features.push(f);
    groupAgg.set(f.group, entry);
  }

  const keys: Key[] = [];
  for (const [group, agg] of groupAgg.entries()) {
    const tier = tierFor(group, agg.importance);
    if (!tier) continue;
    // Filter headline candidates to non-tied features. If every feature
    // in this group is essentially tied (|value| < TIED_VALUE_THRESHOLD),
    // we have no concrete stat to point at — drop the group rather than
    // confidently announce an edge headlined by a tied feature. Flag
    // features (venue, conference) are always eligible: their narrative
    // phrasing in `formatHeadlineGap` doesn't depend on having a
    // meaningful diff value.
    const candidates = agg.features.filter(
      (f) => FLAG_FEATURES.has(f.name) || Math.abs(f.value) >= TIED_VALUE_THRESHOLD,
    );
    if (candidates.length === 0) continue;
    // signedImportance sign tells which team the group's data leans
    // toward, weighted by how much the model cares about each feature.
    // A pure 0 (no team has any edge) is rare — fall back to the model's
    // signed contribution sum for tie-breaking.
    const towardHome = agg.signedImportance !== 0
      ? agg.signedImportance > 0
      : agg.features.reduce((s, f) => s + f.contribution, 0) > 0;
    const headline = pickHeadline(candidates, towardHome);
    keys.push({
      group,
      team: towardHome ? result.home_team : result.away_team,
      towardHome,
      importance: agg.importance,
      tier,
      headline,
    });
  }
  return selectTopKeys(keys, 4);
}

function KeysToGame({ result }: { result: PredictionResult }) {
  const keys = useMemo(() => generateKeys(result), [result]);

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Keys to the Game
        </h2>
        <div className="text-[11px] text-gray-500">Synthesized from the model</div>
      </div>
      {keys.length === 0 ? (
        <p className="text-sm text-gray-400">
          The model sees this as an even matchup with no clear advantages on either side.
        </p>
      ) : (
        <ul className="space-y-3">
          {keys.map((k) => (
            <KeyItem key={k.group} k={k} />
          ))}
        </ul>
      )}
    </div>
  );
}

function KeyItem({ k }: { k: Key }) {
  const teamColor = k.towardHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  // Phrase the headline gap from the leading team's perspective, using
  // the data direction (`homeAdvantageSign`) so the named team is always
  // the side with the better underlying stat — never the side the model
  // happened to credit a feature toward (those can disagree on
  // non-monotonic features).
  const headlineGapText = k.headline
    ? formatHeadlineGap(k.headline, k.team, k.towardHome)
    : null;

  return (
    <li className="flex items-start gap-3">
      <div className="flex-shrink-0 w-1 self-stretch rounded-full" style={{ backgroundColor: teamColor }} />
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline justify-between gap-2 flex-wrap">
          <div className="text-sm">
            <span className="text-gray-200 font-medium">{k.group}</span>
            <span className="text-gray-500">: </span>
            <span style={{ color: teamColor }} className="font-medium">
              {k.team}
            </span>{' '}
            <span className="text-gray-300">{TIER_PHRASE[k.tier]}</span>
          </div>
          <span
            className={`text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded ${TIER_BADGE[k.tier]}`}
          >
            {k.tier}
          </span>
        </div>
        {k.headline && (
          <div className="text-xs text-gray-400 mt-1 leading-snug">
            Headlined by <span className="text-gray-300">{k.headline.label}</span>
            {headlineGapText && <> {headlineGapText}</>}
          </div>
        )}
      </div>
    </li>
  );
}

/// Render the parenthetical that follows the headline feature label
/// (e.g. "Headlined by Adj efficiency margin <here>").
///
/// Returns one of:
///   - "(home court factor)" / "(neutral site)" / "(conference matchup)" /
///     "(non-conference matchup)" for the two flag features
///   - "(teams essentially tied on this stat)" when the rounded value is
///     near zero (unreachable in practice — `generateKeys` filters tied
///     candidates upstream — kept as a defensive fallback)
///   - "(gap of 7.40 in Duke's favor)" — the common case
///   - "(gap of 0.10 the other way — outweighed by other Duke edges in
///     this group)" when the headline feature's *data direction*
///     (`homeAdvantageSign`) disagrees with the group's net direction.
///     Rare under the current selection rule: `pickHeadline` already
///     prefers a concordant alternative when one is within
///     `CONCORDANT_HEADLINE_TOLERANCE` of the max |SHAP|, so this branch
///     only fires when one feature genuinely dominates the group's
///     |SHAP| but bucks the data narrative (e.g. Purdue vs Michigan
///     opp eFG%).
function formatHeadlineGap(
  headline: FeatureContribution,
  team: string,
  towardHome: boolean,
): string | null {
  // True flag features (venue, is_conference_game) get their own phrasing.
  if (FLAG_FEATURES.has(headline.name)) {
    if (headline.name === 'venue') {
      return headline.value > 0 ? '(home court factor)' : '(neutral site)';
    }
    if (headline.name === 'is_conference_game') {
      return headline.value > 0 ? '(conference matchup)' : '(non-conference matchup)';
    }
    return null;
  }

  // Continuous feature with a value at or near zero — teams are essentially
  // tied, so don't pretend there's a gap. (Should be unreachable now that
  // `generateKeys` filters tied features out as headline candidates, but
  // keep the fallback so the function stays self-contained.)
  if (Math.abs(headline.value) < TIED_VALUE_THRESHOLD) {
    return '(teams essentially tied on this stat)';
  }

  // Show absolute gap; the team name above already encodes the direction.
  const absVal = Math.abs(headline.value);
  const gap =
    absVal >= 10 ? absVal.toFixed(1) : absVal >= 1 ? absVal.toFixed(2) : absVal.toFixed(3);
  // Direction comes from the data via `homeAdvantageSign`. If that
  // disagrees with the group's net direction (one feature fighting its
  // group), soften the phrasing.
  const advantageSign = homeAdvantageSign(headline.name, headline.value);
  const matches = (advantageSign > 0) === towardHome;
  if (matches) {
    return `(gap of ${gap} in ${team}'s favor)`;
  }
  return `(gap of ${gap} the other way — outweighed by other ${team} edges in this group)`;
}

// ---------------------------------------------------------------------------
// Side-by-side stats — uses the rankings data we already fetched for the picker
// ---------------------------------------------------------------------------

function lookupTeam(name: string, teams: TeamRanking[]): TeamRanking | undefined {
  return teams.find((t) => t.name === name);
}

interface StatRow {
  label: string;
  home: number | string | null;
  away: number | string | null;
  /// `'high'` means higher is better, `'low'` lower is better, `'neither'` no
  /// directional bias (e.g. tempo), and `'record'` is a special case for W-L
  /// where we compare win pct.
  better: 'high' | 'low' | 'neither' | 'record';
  /// Used when comparing W-L records (we sort by win pct).
  homeNum?: number;
  awayNum?: number;
  format?: (v: number) => string;
}

function SideBySideStats({
  result,
  teams,
}: {
  result: PredictionResult;
  teams: TeamRanking[];
}) {
  const home = lookupTeam(result.home_team, teams);
  const away = lookupTeam(result.away_team, teams);
  if (!home || !away) return null;

  const fmt1 = (v: number) => (v > 0 ? '+' : '') + v.toFixed(1);
  const fmt0 = (v: number) => Math.round(v).toString();

  const winPct = (t: TeamRanking) => {
    const total = t.wins + t.losses;
    return total > 0 ? t.wins / total : 0.5;
  };

  const rows: StatRow[] = [
    {
      label: 'Record',
      home: `${home.wins}-${home.losses}`,
      away: `${away.wins}-${away.losses}`,
      homeNum: winPct(home),
      awayNum: winPct(away),
      better: 'record',
    },
    {
      label: 'AdjEM',
      home: home.adj_efficiency_margin,
      away: away.adj_efficiency_margin,
      better: 'high',
      format: fmt1,
    },
    {
      label: 'AdjO',
      home: home.adj_offense,
      away: away.adj_offense,
      better: 'high',
      format: (v) => v.toFixed(1),
    },
    {
      label: 'AdjD',
      home: home.adj_defense,
      away: away.adj_defense,
      better: 'low',
      format: (v) => v.toFixed(1),
    },
    {
      label: 'Tempo',
      home: home.adj_tempo,
      away: away.adj_tempo,
      better: 'neither',
      format: (v) => v.toFixed(1),
    },
    {
      label: 'SOS',
      home: home.sos,
      away: away.sos,
      better: 'high',
      format: fmt1,
    },
    {
      label: 'ELO',
      home: home.elo_rating,
      away: away.elo_rating,
      better: 'high',
      format: fmt0,
    },
  ];

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Side by Side
        </h2>
      </div>
      <div className="space-y-1.5">
        {/* Header */}
        <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 pb-2 border-b border-gray-700">
          <div className="text-right text-sm font-medium" style={{ color: TEAM_1_COLOR }}>
            {home.name}
          </div>
          <div className="w-20 text-center text-[11px] uppercase tracking-wide text-gray-500">
            stat
          </div>
          <div className="text-left text-sm font-medium" style={{ color: TEAM_2_COLOR }}>
            {away.name}
          </div>
        </div>
        {rows.map((r) => (
          <StatComparisonRow key={r.label} row={r} />
        ))}
      </div>
    </div>
  );
}

function StatComparisonRow({ row }: { row: StatRow }) {
  const homeBetter = computeWinner(row) === 'home';
  const awayBetter = computeWinner(row) === 'away';
  const fmt = row.format ?? ((v: number) => v.toFixed(1));

  const renderValue = (v: number | string | null, better: boolean, color: string) => {
    if (v == null) return <span className="text-gray-500">—</span>;
    const text = typeof v === 'number' ? fmt(v) : v;
    return (
      <span className={better ? 'font-semibold' : 'text-gray-400'} style={better ? { color } : {}}>
        {text}
      </span>
    );
  };

  return (
    <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 text-sm">
      <div className="text-right">
        {renderValue(row.home, homeBetter, TEAM_1_COLOR)}
      </div>
      <div className="w-20 text-center text-[11px] text-gray-500 uppercase tracking-wide">
        {row.label}
      </div>
      <div className="text-left">{renderValue(row.away, awayBetter, TEAM_2_COLOR)}</div>
    </div>
  );
}

function computeWinner(row: StatRow): 'home' | 'away' | null {
  const h = row.better === 'record' ? row.homeNum : (row.home as number | null);
  const a = row.better === 'record' ? row.awayNum : (row.away as number | null);
  if (h == null || a == null || row.better === 'neither') return null;
  if (h === a) return null;
  if (row.better === 'low') return h < a ? 'home' : 'away';
  return h > a ? 'home' : 'away';
}

// ---------------------------------------------------------------------------
// Four factors — split into two panels, one per "side of the ball". Each row
// is a single tug-of-war bar showing the matchup-specific advantage between
// the offensive team and the defensive team for that factor.
// ---------------------------------------------------------------------------

/// Approximate D-I averages — used to decompose each side's strength
/// (deviation from league average) so per-matchup advantages compose
/// cleanly. Tuned by inspection of recent seasons; precision matters less
/// than relative ordering for the visual.
const LEAGUE_AVG = {
  EFG: 0.50,
  TOV: 0.17,
  ORB: 0.30, // offensive rebound rate
  DRB: 0.70, // defensive rebound rate (1 - opponent ORB)
  FT_RATE: 0.30,
} as const;

interface MatchupRow {
  label: string;
  /// Offensive team's value for this factor (e.g. team1's eFG% on offense).
  offValue: number | null;
  /// Defensive team's allowed/forced value (e.g. team2's opp_eFG% allowed).
  defValue: number | null;
  /// Net advantage in factor units. Positive = offense wins, negative =
  /// defense wins. Computed from off_strength − def_strength so each side's
  /// deviation from league average composes correctly.
  advantage: number | null;
  /// Display formatter for raw values (e.g. "55.0%" for percentages).
  formatRaw: (v: number) => string;
  /// Display formatter for the advantage chip (e.g. "+5.2pp").
  formatAdvantage: (v: number) => string;
  /// Bar cap (in advantage units) at which the tug-of-war fills one half.
  /// Tuned per factor so a decisive matchup edge fills the bar.
  barCap: number;
}

function FourFactorsPanel({
  result,
  teams,
}: {
  result: PredictionResult;
  teams: TeamRanking[];
}) {
  const home = lookupTeam(result.home_team, teams);
  const away = lookupTeam(result.away_team, teams);
  if (!home || !away) return null;

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Four Factors
        </h2>
        <div className="text-[11px] text-gray-500">Per-matchup advantage</div>
      </div>
      <div className="space-y-6">
        <MatchupSubpanel
          offTeam={home}
          defTeam={away}
          offColor={TEAM_1_COLOR}
          defColor={TEAM_2_COLOR}
        />
        <MatchupSubpanel
          offTeam={away}
          defTeam={home}
          offColor={TEAM_2_COLOR}
          defColor={TEAM_1_COLOR}
        />
      </div>
    </div>
  );
}

function MatchupSubpanel({
  offTeam,
  defTeam,
  offColor,
  defColor,
}: {
  offTeam: TeamRanking;
  defTeam: TeamRanking;
  offColor: string;
  defColor: string;
}) {
  const fmtPp = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(1)}pp`;
  const fmtRatio = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(3)}`;
  const pctStr = (v: number) => `${(v * 100).toFixed(1)}%`;
  const ratioStr = (v: number) => v.toFixed(3);

  // Each factor's advantage formula is:
  //   off_strength = (off_value − league_avg) signed in offense's favor
  //   def_strength = (def_value − league_avg) signed in defense's favor
  //   advantage = off_strength − def_strength
  // Signs are baked into the formulas below so each one resolves to a
  // single signed scalar where positive = offense wins.
  const rows: MatchupRow[] = [
    {
      label: 'eFG%',
      offValue: offTeam.effective_fg_pct,
      defValue: defTeam.opp_effective_fg_pct,
      advantage:
        offTeam.effective_fg_pct == null || defTeam.opp_effective_fg_pct == null
          ? null
          : (offTeam.effective_fg_pct + defTeam.opp_effective_fg_pct - 2 * LEAGUE_AVG.EFG) * 100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 8,
    },
    {
      label: 'TOV%',
      offValue: offTeam.turnover_pct,
      defValue: defTeam.opp_turnover_pct,
      advantage:
        offTeam.turnover_pct == null || defTeam.opp_turnover_pct == null
          ? null
          : (2 * LEAGUE_AVG.TOV - offTeam.turnover_pct - defTeam.opp_turnover_pct) * 100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 5,
    },
    {
      label: 'Rebounding',
      offValue: offTeam.off_rebound_pct,
      defValue: defTeam.def_rebound_pct,
      advantage:
        offTeam.off_rebound_pct == null || defTeam.def_rebound_pct == null
          ? null
          : (offTeam.off_rebound_pct - LEAGUE_AVG.ORB - (defTeam.def_rebound_pct - LEAGUE_AVG.DRB)) *
            100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 8,
    },
    {
      label: 'FT Rate',
      offValue: offTeam.ft_rate,
      defValue: defTeam.opp_ft_rate,
      advantage:
        offTeam.ft_rate == null || defTeam.opp_ft_rate == null
          ? null
          : offTeam.ft_rate + defTeam.opp_ft_rate - 2 * LEAGUE_AVG.FT_RATE,
      formatRaw: ratioStr,
      formatAdvantage: fmtRatio,
      barCap: 0.06,
    },
  ];

  return (
    <div>
      <div className="text-xs uppercase tracking-wide text-gray-500 mb-3">
        When <span style={{ color: offColor }}>{offTeam.name}</span> has the ball
      </div>
      <div className="space-y-3">
        {rows.map((r) => (
          <MatchupRowView
            key={r.label}
            row={r}
            offTeam={offTeam.name}
            defTeam={defTeam.name}
            offColor={offColor}
            defColor={defColor}
          />
        ))}
      </div>
    </div>
  );
}

function MatchupRowView({
  row,
  offTeam,
  defTeam,
  offColor,
  defColor,
}: {
  row: MatchupRow;
  offTeam: string;
  defTeam: string;
  offColor: string;
  defColor: string;
}) {
  if (row.advantage == null || row.offValue == null || row.defValue == null) {
    return <div className="text-xs text-gray-500">{row.label}: —</div>;
  }

  const offWins = row.advantage > 0;
  const winnerColor = offWins ? offColor : defColor;
  const winnerName = offWins ? offTeam : defTeam;
  // Cap at 50 because the bar represents one half of the container (the
  // half-width on either side of the centerline). `Math.min(50, …)` keeps
  // the bar from extending past its container edge on extreme matchups.
  const barPct = Math.min(50, (Math.abs(row.advantage) / row.barCap) * 50);

  return (
    <div>
      {/* Factor label */}
      <div className="text-sm mb-1.5">
        <span className="text-gray-200 font-medium">{row.label}</span>
      </div>

      {/* Centered winner chip above the bar */}
      <div className="grid grid-cols-[80px_1fr_80px] gap-3 mb-0.5">
        <div />
        <div className="text-center text-xs">
          <span style={{ color: winnerColor }} className="font-semibold">
            {winnerName} {row.formatAdvantage(Math.abs(row.advantage))}
          </span>
        </div>
        <div />
      </div>

      {/* Raw offense / defense values flanking the tug-of-war bar */}
      <div className="grid grid-cols-[80px_1fr_80px] items-center gap-3 text-xs">
        <div className="text-right">
          <span style={{ color: offColor }}>{row.formatRaw(row.offValue)}</span>
        </div>
        <div className="relative h-3 bg-gray-900 rounded">
          <div className="absolute inset-y-0 left-1/2 w-px bg-gray-700" />
          <div
            className="absolute inset-y-0 rounded"
            style={
              offWins
                ? { left: `${50 - barPct}%`, width: `${barPct}%`, backgroundColor: offColor }
                : { left: '50%', width: `${barPct}%`, backgroundColor: defColor }
            }
          />
        </div>
        <div className="text-left">
          <span style={{ color: defColor }}>{row.formatRaw(row.defValue)}</span>
        </div>
      </div>

      {/* Tiny under-line clarifying which side is which */}
      <div className="grid grid-cols-2 gap-3 mt-0.5 text-[10px] text-gray-500">
        <div className="text-right">offense</div>
        <div className="text-left">defense</div>
      </div>
    </div>
  );
}

function ResultHeadline({
  result,
  team1Prob,
}: {
  result: PredictionResult;
  team1Prob: number;
}) {
  const margin = result.predicted_margin;
  const winnerIsHome = margin > 0;
  const winnerColor = winnerIsHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  // Display the spread from the *winner's* perspective, KenPom-style:
  // "Duke -3.5" reads naturally regardless of which team was passed first.
  const winnerSpread = -Math.abs(margin);

  const venueText =
    result.venue === 'neutral'
      ? 'Neutral site'
      : result.venue === 'home'
        ? `at ${result.home_team}`
        : `at ${result.away_team}`;

  return (
    <div className="bg-gray-800 rounded-lg p-6 space-y-5">
      <div className="text-center">
        <div className="text-xs text-gray-500 uppercase tracking-wide mb-2">{venueText}</div>
        <div className="text-3xl font-bold" style={{ color: winnerColor }}>
          {result.predicted_winner}{' '}
          <span className="text-2xl text-gray-300 font-semibold">
            {winnerSpread.toFixed(1)}
          </span>
        </div>
        <div className="text-sm text-gray-400 mt-1">
          {(Math.max(result.home_win_probability, 1 - result.home_win_probability) * 100).toFixed(0)}
          % win probability
        </div>
      </div>

      {/* Probability bar */}
      <div>
        <div className="flex justify-between text-sm mb-1">
          <span className={winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'}>
            {result.home_team}
          </span>
          <span className={!winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'}>
            {result.away_team}
          </span>
        </div>
        <div className="flex h-7 rounded-full overflow-hidden ring-1 ring-gray-700">
          <div
            className="flex items-center justify-center text-xs font-medium text-white transition-[width]"
            style={{ width: `${team1Prob}%`, backgroundColor: TEAM_1_COLOR }}
          >
            {team1Prob >= 12 ? `${team1Prob.toFixed(0)}%` : ''}
          </div>
          <div
            className="flex items-center justify-center text-xs font-medium text-white transition-[width]"
            style={{ width: `${100 - team1Prob}%`, backgroundColor: TEAM_2_COLOR }}
          >
            {100 - team1Prob >= 12 ? `${(100 - team1Prob).toFixed(0)}%` : ''}
          </div>
        </div>
      </div>
    </div>
  );
}

function TeamPicker({
  label,
  value,
  onChange,
  teams,
  placeholder,
  color,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  teams: TeamRanking[];
  placeholder: string;
  color: string;
}) {
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const filtered = useMemo(() => {
    const q = value.trim().toLowerCase();
    if (q.length === 0) return [];
    return teams
      .filter(
        (t) =>
          t.name.toLowerCase().includes(q) ||
          (t.conference?.toLowerCase().includes(q) ?? false),
      )
      .slice(0, 10);
  }, [teams, value]);

  return (
    <div className="relative">
      <label className="block text-sm text-gray-400 mb-1">
        <span style={{ color }} className="font-medium">
          ●
        </span>{' '}
        {label}
      </label>
      <input
        ref={inputRef}
        type="text"
        value={value}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        placeholder={placeholder}
        className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
        autoComplete="off"
      />
      {open && filtered.length > 0 && (
        <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-700 rounded shadow-lg">
          {filtered.map((t) => (
            <button
              key={t.team_id}
              type="button"
              onMouseDown={(e) => {
                e.preventDefault();
                onChange(t.name);
                setOpen(false);
                inputRef.current?.blur();
              }}
              className="w-full text-left px-3 py-2 hover:bg-gray-800 text-sm flex items-center justify-between gap-3"
            >
              <span className="truncate">{t.name}</span>
              <span className="text-xs text-gray-500 truncate">
                {t.conference ?? '—'}
                {t.adj_efficiency_margin != null && (
                  <>
                    {' · '}
                    {t.adj_efficiency_margin > 0 ? '+' : ''}
                    {t.adj_efficiency_margin.toFixed(1)}
                  </>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
