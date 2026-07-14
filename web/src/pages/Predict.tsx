import { useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  fetchPrediction,
  fetchTeamRankings,
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
import { campomTier, campomTierColor, campomTitle } from '../components/campom';
import { classColor, classTitle } from '../components/archetypeColors';
import { shortDate } from '../components/format';
import { RosterWaffle } from '../components/RosterWaffle';
import { TeamShotDiet } from '../components/TeamShotDiet';
import { Link } from 'react-router-dom';

const TEAM_1_COLOR = '#3b82f6'; // blue (matches PlayerCompare PLAYER_COLORS[0])
const TEAM_2_COLOR = '#ef4444'; // red

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [searchParams] = useSearchParams();
  const urlHome = searchParams.get('home') ?? '';
  const urlAway = searchParams.get('away') ?? '';
  const urlVenue = searchParams.get('venue') as Venue | null;
  const initialVenue: Venue =
    urlVenue === 'home' || urlVenue === 'away' || urlVenue === 'neutral' ? urlVenue : 'home';
  // Point-in-time cutoff. When present (`YYYY-MM-DD`), the prediction is
  // routed through the pit model bundle so the displayed forecast
  // reflects only data available up to and including that date — the
  // honest counterfactual for a historical matchup. Empty → live
  // end-of-season state.
  const urlAsOfDate = searchParams.get('as_of_date') ?? '';

  const [team1, setTeam1] = useState(urlHome);
  const [team2, setTeam2] = useState(urlAway);
  const [venue, setVenue] = useState<Venue>(initialVenue);
  const [asOfDate, setAsOfDate] = useState(urlAsOfDate);
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
    setAsOfDate(urlAsOfDate);
    let alive = true;
    setLoading(true);
    setError('');
    setResult(null);
    fetchPrediction(
      urlHome.trim(),
      urlAway.trim(),
      initialVenue,
      season,
      urlAsOfDate || undefined,
    )
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
  }, [urlHome, urlAway, urlVenue, urlAsOfDate, season]);

  const handleSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!team1.trim() || !team2.trim()) return;
    setLoading(true);
    setError('');
    setResult(null);
    try {
      const r = await fetchPrediction(
        team1.trim(),
        team2.trim(),
        venue,
        season,
        asOfDate.trim() || undefined,
      );
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
        Predicting{' '}
        <span className="text-gray-300">
          {season - 1}-{String(season).slice(2)}
        </span>{' '}
        matchups. Switch seasons via the nav for back-tests.
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

        <div>
          <label htmlFor="as-of-date" className="block text-sm text-gray-400 mb-1.5">
            As of <span className="text-gray-600">(optional, for historical projections)</span>
          </label>
          <input
            id="as-of-date"
            type="date"
            value={asOfDate}
            onChange={(e) => setAsOfDate(e.target.value)}
            className="bg-gray-900 border border-gray-700 text-gray-200 rounded px-3 py-1.5 text-sm focus:outline-none focus:border-blue-500"
          />
          {asOfDate && (
            <p className="mt-1 text-xs text-amber-400">
              Point-in-time projection: CamPom rebuilt from game-by-game Torvik data
              up to {asOfDate}. Team-level features (AdjEM, SOS, four factors)
              remain end-of-season aggregates — see roadmap §4b for the residual
              leak budget.
            </p>
          )}
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
          {/* Order matches the user's mental flow: answer first,
              quantitative breakdown second, personnel context third,
              historical evidence last (most useful after you've
              internalised the projection's reasoning).
              PreviousMatchups returns null when the teams haven't
              played, so absent-history matchups still flow cleanly. */}
          <ResultHeadline result={result} team1Prob={team1Prob} />
          <SideBySideStats result={result} teams={teams} />
          <ArchetypeRow result={result} />
          <ShotDietRow result={result} />
          <RosterCompare result={result} />
          <PreviousMatchups result={result} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Archetype + Shot Diet rows. Mirror the TeamDetail panels, rendered side
// by side per team so the matchup reads as two roster identities you can
// compare without leaving the page. The waffle's canonical CLASS_ORDER
// means each archetype lands in the same waffle position on both sides,
// so the eye compares blocks-in-the-same-region rather than hunting.
// ---------------------------------------------------------------------------

function ArchetypeRow({ result }: { result: PredictionResult }) {
  const hasHome = result.archetype_distribution_home?.some((a) => a.team_share > 0);
  const hasAway = result.archetype_distribution_away?.some((a) => a.team_share > 0);
  if (!hasHome && !hasAway) return null;
  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Roster Archetypes
        </h2>
        <div className="text-[11px] text-gray-500">
          1% of team minutes per square
        </div>
      </div>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <TeamPanel
          teamName={result.home_team}
          teamId={result.home_team_id}
          season={result.season}
          color={TEAM_1_COLOR}
        >
          {hasHome ? (
            <div className="flex justify-center">
              <RosterWaffle archetypeDist={result.archetype_distribution_home} />
            </div>
          ) : (
            <EmptyNote>No archetype data</EmptyNote>
          )}
        </TeamPanel>
        <TeamPanel
          teamName={result.away_team}
          teamId={result.away_team_id}
          season={result.season}
          color={TEAM_2_COLOR}
        >
          {hasAway ? (
            <div className="flex justify-center">
              <RosterWaffle archetypeDist={result.archetype_distribution_away} />
            </div>
          ) : (
            <EmptyNote>No archetype data</EmptyNote>
          )}
        </TeamPanel>
      </div>
    </div>
  );
}

function ShotDietRow({ result }: { result: PredictionResult }) {
  const hasHome = result.roster_home.some((p) => (p.rim_attempted ?? 0) + (p.mid_attempted ?? 0) + (p.tpa ?? 0) > 0);
  const hasAway = result.roster_away.some((p) => (p.rim_attempted ?? 0) + (p.mid_attempted ?? 0) + (p.tpa ?? 0) > 0);
  if (!hasHome && !hasAway) return null;
  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Shot Diet
        </h2>
        <div className="text-[11px] text-gray-500">
          Hover a zone for top contributors
        </div>
      </div>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <TeamPanel
          teamName={result.home_team}
          teamId={result.home_team_id}
          season={result.season}
          color={TEAM_1_COLOR}
        >
          {hasHome ? (
            <TeamShotDiet roster={result.roster_home} />
          ) : (
            <EmptyNote>No Torvik shot data</EmptyNote>
          )}
        </TeamPanel>
        <TeamPanel
          teamName={result.away_team}
          teamId={result.away_team_id}
          season={result.season}
          color={TEAM_2_COLOR}
        >
          {hasAway ? (
            <TeamShotDiet roster={result.roster_away} />
          ) : (
            <EmptyNote>No Torvik shot data</EmptyNote>
          )}
        </TeamPanel>
      </div>
    </div>
  );
}

/// Light wrapper that prints the team name + a color-coded link to
/// the team page above each side of the two-team comparison rows.
/// Keeps the per-team header treatment consistent between the
/// archetype and shot-diet sections.
function TeamPanel({
  teamName,
  teamId,
  season,
  color,
  children,
}: {
  teamName: string;
  teamId: string;
  season: number;
  color: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-3 flex items-baseline gap-2">
        <span
          className="inline-block w-2 h-2 rounded-full"
          style={{ background: color }}
        />
        <Link
          to={seasonHref(`/teams/${teamId}`, season)}
          className="text-sm font-semibold hover:underline truncate"
          style={{ color }}
        >
          {teamName}
        </Link>
      </div>
      {children}
    </div>
  );
}

function EmptyNote({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-xs text-gray-500 italic text-center py-8">
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Roster Compare panel — side-by-side roster table (top 8 by CamPom per
// team) with archetype chips and rate stats. The Archetype + Shot Diet
// rows above already render the visual identity per team; this panel
// drills into the specific players carrying it.
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
        title={campomTitle(p.campom, p.campom_o, p.campom_d) || undefined}
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
  // Compute season-aware league averages from the rankings list we already
  // fetched, so the possession panels' league-baseline highlighting tracks
  // the actual era's stats instead of frozen 2008-vintage Dean Oliver
  // figures. `useMemo` because `teams` is stable across renders.
  const leagueAvg = useMemo(() => computeLeagueAverages(teams), [teams]);
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
    // AdjO/AdjD intentionally omitted here — they live as the headline
    // row of each possession panel below, where they pair naturally with
    // the four factors that decompose them.
    {
      // Tempo isn't directionally good or bad — fast teams aren't better
      // teams. Showing each team's pace as a signed delta from league
      // average gives users immediate context: `+2.4 / −0.4` reads as
      // "Duke fast, Illinois slow", `+5 / +5` reads "track meet",
      // `−3 / −3` reads "grinder". Raw numbers (66.4 / 65.4) carry the
      // same info but only if you've memorised the baseline. Label
      // includes Δ so the values aren't mistaken for raw possessions.
      label: 'Tempo Δ',
      home: home.adj_tempo == null ? null : home.adj_tempo - leagueAvg.TEMPO,
      away: away.adj_tempo == null ? null : away.adj_tempo - leagueAvg.TEMPO,
      better: 'neither',
      format: fmt1,
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
          Team Stats
        </h2>
        <div className="text-[11px] text-gray-500">Season averages</div>
      </div>
      {/* Three-column layout on desktop: general team stats | offense
          when team1 has the ball | offense when team2 has the ball.
          Each column has 5 rows (Record/AdjEM/Tempo/SOS/ELO on the left,
          Pts/100 + four factors on the right two) so heights line up.
          `lg:` (≥1024px) is the right breakpoint here — at `md:` (768px)
          the page's `max-w-4xl` cap leaves ~240px per column and team
          names like "Northern Illinois" wrap. Tablets stack to a single
          column gracefully. */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Column 1: general stats */}
        <div className="space-y-1.5">
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

        {/* Column 2: when home team has the ball */}
        <PossessionPanel
          offTeam={home}
          defTeam={away}
          offColor={TEAM_1_COLOR}
          defColor={TEAM_2_COLOR}
          leagueAvg={leagueAvg}
        />

        {/* Column 3: when away team has the ball */}
        <PossessionPanel
          offTeam={away}
          defTeam={home}
          offColor={TEAM_2_COLOR}
          defColor={TEAM_1_COLOR}
          leagueAvg={leagueAvg}
        />
      </div>
    </div>
  );
}

interface PossessionLeagueAvg {
  /// Adjusted-efficiency league avg in pts/100 possessions. AdjO and
  /// AdjD share one baseline — every team's `adj_offense` mean equals
  /// every team's `adj_defense` mean by construction (every point
  /// scored is a point allowed somewhere).
  EFF: number;
  eFG: number;
  TOV: number;
  /// ORB% league avg (≈ DRB% complement; both panels' ORB% rows compare
  /// in ORB% units after converting DRB% → 1 − DRB%).
  ORB: number;
  FT: number;
  /// Adjusted-tempo league avg in possessions/40min. Used by the Tempo
  /// row in the general column to render each team's pace as a signed
  /// delta from average — gives users immediate context for "fast vs
  /// slow" without forcing them to memorise a baseline.
  TEMPO: number;
}

/// Conservative D-I averages used as a fallback when the rankings list
/// isn't loaded. The live league averages are computed per-season from
/// the actual rankings via `computeLeagueAverages`; these constants only
/// fire on the empty-list edge case so the UI doesn't divide-by-zero.
const POSSESSION_LEAGUE_AVG_FALLBACK: PossessionLeagueAvg = {
  EFF: 105,
  eFG: 0.5,
  TOV: 0.17,
  ORB: 0.3,
  FT: 0.3,
  TEMPO: 67,
};

/// Compute simple (per-team) means of the four-factor stats from the
/// season's full rankings list. Each TeamRanking percentage is already
/// per-possession-normalized, so per-team mean is a reasonable league
/// baseline — possession-weighting would shift the answer by a hair but
/// requires possession totals we don't have here. Drives the highlighting
/// in `PossessionPanel` so the comparison reflects the era you're viewing
/// (modern D-I ORB% is ~28%, not the 30% Dean Oliver coined in 2008).
function computeLeagueAverages(teams: TeamRanking[]): PossessionLeagueAvg {
  if (teams.length === 0) return POSSESSION_LEAGUE_AVG_FALLBACK;

  const mean = (extract: (t: TeamRanking) => number | null | undefined): number => {
    let sum = 0;
    let count = 0;
    for (const t of teams) {
      const v = extract(t);
      if (v != null && Number.isFinite(v)) {
        sum += v;
        count += 1;
      }
    }
    return count > 0 ? sum / count : 0;
  };

  return {
    EFF: mean((t) => t.adj_offense),
    eFG: mean((t) => t.effective_fg_pct),
    TOV: mean((t) => t.turnover_pct),
    ORB: mean((t) => t.off_rebound_pct),
    FT: mean((t) => t.ft_rate),
    TEMPO: mean((t) => t.adj_tempo),
  };
}

interface PossessionRowSpec {
  label: string;
  /// Offensive team's stat (e.g. their eFG%).
  offValue: number | null | undefined;
  /// Defensive team's allowed/forced stat in the same units as `offValue`,
  /// so the two are directly comparable. For rebounding the caller passes
  /// `1 − DRB%` so both sides read as ORB% (raw DRB% would be the
  /// complement-by-definition and just show the same stat twice).
  defValue: number | null | undefined;
  /// League-average baseline in the same units as off/def. The highlight
  /// goes to whichever side's deviation from this average is larger
  /// (signed in their favor — see `PossessionRow`).
  leagueAvg: number;
  /// `'high'` = higher is better for offense (eFG%, ORB%, FT Rate);
  /// `'low'` = lower is better for offense (TOV%). Direction flips for
  /// the defense — lower-allowed eFG% is good for defense; higher-forced
  /// TOV% is good for defense.
  better: 'high' | 'low';
  format: (v: number) => string;
}

function PossessionPanel({
  offTeam,
  defTeam,
  offColor,
  defColor,
  leagueAvg,
}: {
  offTeam: TeamRanking;
  defTeam: TeamRanking;
  offColor: string;
  defColor: string;
  leagueAvg: PossessionLeagueAvg;
}) {
  const fmtPct = (v: number) => `${(v * 100).toFixed(1)}%`;
  const fmtRatio = (v: number) => v.toFixed(3);
  const fmtEff = (v: number) => v.toFixed(1);

  const rows: PossessionRowSpec[] = [
    // Headline row: AdjO (offense's pts/100) vs AdjD (defense's pts
    // allowed/100). Higher AdjO is better for offense; lower AdjD is
    // better for defense — the `better: 'high'` decomposition handles
    // both directions correctly via league-baseline math (off_strength
    // = off − league; def_strength = league − def). The four factors
    // below decompose what's driving this number.
    {
      label: 'Pts/100',
      offValue: offTeam.adj_offense,
      defValue: defTeam.adj_defense,
      leagueAvg: leagueAvg.EFF,
      better: 'high',
      format: fmtEff,
    },
    {
      label: 'eFG%',
      offValue: offTeam.effective_fg_pct,
      defValue: defTeam.opp_effective_fg_pct,
      leagueAvg: leagueAvg.eFG,
      better: 'high',
      format: fmtPct,
    },
    {
      label: 'TOV%',
      offValue: offTeam.turnover_pct,
      defValue: defTeam.opp_turnover_pct,
      leagueAvg: leagueAvg.TOV,
      better: 'low',
      format: fmtPct,
    },
    {
      label: 'ORB%',
      offValue: offTeam.off_rebound_pct,
      // Convert DRB% to "ORB% allowed" so both sides are in the same
      // direction (offensive rebound rate) — pairing raw ORB% with raw
      // DRB% is a complement-by-definition trap that exaggerates the
      // gap visually (33% vs 72% reads as huge but is just two views
      // of the same coin).
      defValue: defTeam.def_rebound_pct == null ? null : 1 - defTeam.def_rebound_pct,
      leagueAvg: leagueAvg.ORB,
      better: 'high',
      format: fmtPct,
    },
    {
      label: 'FT Rate',
      offValue: offTeam.ft_rate,
      defValue: defTeam.opp_ft_rate,
      leagueAvg: leagueAvg.FT,
      better: 'high',
      format: fmtRatio,
    },
  ];

  return (
    <div>
      <div className="text-xs uppercase tracking-wide text-gray-500 mb-3 text-center">
        When <span style={{ color: offColor }}>{offTeam.name}</span> has the ball
      </div>
      <div className="space-y-1.5">
        {rows.map((r) => (
          <PossessionRow key={r.label} row={r} offColor={offColor} defColor={defColor} />
        ))}
      </div>
    </div>
  );
}

function PossessionRow({
  row,
  offColor,
  defColor,
}: {
  row: PossessionRowSpec;
  offColor: string;
  defColor: string;
}) {
  // Decompose each side's strength as deviation from league average, signed
  // in their favor. For TOV% specifically: low offensive TOV% is GOOD for
  // the offense (off_strength = league - off_value); low defensive forced
  // TOV% is BAD for the defense (def_strength = def_value - league). So
  // Duke 13.4% TOV vs Illinois 11.7% opp-TOV — both well below the ~17%
  // league average — is a Duke offensive edge: Duke is strong at the
  // thing Illinois is weak at, even though 13.4 > 11.7 in raw terms.
  let offBetter = false;
  let defBetter = false;
  if (row.offValue != null && row.defValue != null) {
    const offStrength =
      row.better === 'high' ? row.offValue - row.leagueAvg : row.leagueAvg - row.offValue;
    const defStrength =
      row.better === 'high' ? row.leagueAvg - row.defValue : row.defValue - row.leagueAvg;
    if (offStrength > defStrength) offBetter = true;
    else if (defStrength > offStrength) defBetter = true;
  }

  const renderValue = (v: number | null | undefined, better: boolean, color: string) => {
    if (v == null) return <span className="text-gray-500">—</span>;
    return (
      <span className={better ? 'font-semibold' : 'text-gray-400'} style={better ? { color } : {}}>
        {row.format(v)}
      </span>
    );
  };

  return (
    <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 text-sm">
      <div className="text-right">{renderValue(row.offValue, offBetter, offColor)}</div>
      <div className="w-20 text-center text-[11px] text-gray-500 uppercase tracking-wide">
        {row.label}
      </div>
      <div className="text-left">{renderValue(row.defValue, defBetter, defColor)}</div>
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
  const loserColor = winnerIsHome ? TEAM_2_COLOR : TEAM_1_COLOR;
  // Display the spread from the *winner's* perspective, KenPom-style:
  // "Duke -3.5" reads naturally regardless of which team was passed first.
  const winnerSpread = -Math.abs(margin);
  const winPct = (
    Math.max(result.home_win_probability, 1 - result.home_win_probability) * 100
  ).toFixed(0);

  const winnerName = winnerIsHome ? result.home_team : result.away_team;
  const loserName = winnerIsHome ? result.away_team : result.home_team;
  const winnerId = winnerIsHome ? result.home_team_id : result.away_team_id;
  const loserId = winnerIsHome ? result.away_team_id : result.home_team_id;
  const winnerScore = winnerIsHome
    ? result.predicted_home_score
    : result.predicted_away_score;
  const loserScore = winnerIsHome
    ? result.predicted_away_score
    : result.predicted_home_score;

  const venueText =
    result.venue === 'neutral'
      ? 'Neutral site'
      : result.venue === 'home'
        ? `at ${result.home_team}`
        : `at ${result.away_team}`;

  // Server-confirmed regime label. Reads `result.prediction_basis`
  // (set in routes/predict.rs) so a request that drops as_of_date in
  // transit — proxy rewrite, stale cache, future memoization keyed
  // only on home/away/venue — paints the response with what was
  // actually served, not what the page meant to ask for.
  //
  // Early-season honest predictions blend the preseason roster
  // projection (r=0.88) with point-in-time form, decaying preseason out
  // from Nov 1 to ~mid-December (ROADMAP §6, calibrated) — the chip tells
  // the user which regime produced the number. The preseason leg peaks at
  // 0.70 weight at tip-off (never pure), so even "Preseason" is a 70/30 mix.
  const basisMeta: Record<
    string,
    { label: string; cls: string; title: string } | undefined
  > = {
    preseason: {
      label: 'Preseason',
      cls: 'bg-sky-900/60 text-sky-300',
      title: `Preseason-weighted blend as of ${result.as_of_date ?? 'today'}. This early, in-game data is thin, so the forecast leans on the preseason roster projection (r≈0.88) — ~70/30 preseason/${result.as_of_date ? 'point-in-time form' : 'current form'} at tip-off, decaying as games accrue.`,
    },
    blended: {
      label: 'Blended',
      cls: 'bg-teal-900/60 text-teal-300',
      title: `Blend of the preseason roster projection and ${result.as_of_date ? 'point-in-time form' : 'current form'} as of ${result.as_of_date ?? 'today'}. Preseason weight decays from Nov 1 (peak 0.70) to zero by ~mid-December as in-season data accumulates.`,
    },
    pit: {
      label: 'Point-in-time',
      cls: 'bg-amber-900/60 text-amber-300',
      title: `Point-in-time CamPom v3 as of ${result.as_of_date}. Team-level features (AdjEM, SOS, four factors) still reflect end-of-season state.`,
    },
  };
  // Keyed on the server-confirmed basis alone (not as_of_date): the live
  // early-season path blends with no as_of_date on the request, and the
  // chip must still tell the user the number is preseason-anchored. The
  // "leaky" basis has no entry, so ordinary live requests show no chip.
  const meta = basisMeta[result.prediction_basis];
  const basisChip = meta ? (
    <span
      className={`ml-2 inline-flex items-center text-[10px] font-medium uppercase tracking-wide ${meta.cls} px-1.5 py-0.5 rounded`}
      title={meta.title}
    >
      {meta.label}
    </span>
  ) : null;

  return (
    <div className="bg-gray-800 rounded-lg p-6 space-y-5">
      <div className="text-center">
        <div className="text-xs text-gray-500 uppercase tracking-wide mb-2">
          {venueText}
          {basisChip}
        </div>
        {/* Projected final score, winner first. KenPom-style approximation
            (totals model backtest MAE ~13.6 vs margin ~8.2). Team names
            link to detail pages so the headline acts as a navigation
            entry point — matches the affordance in Roster Compare and
            Previous Matchups. */}
        <div className="text-3xl font-bold leading-tight">
          <Link
            to={seasonHref(`/teams/${winnerId}`, result.season)}
            style={{ color: winnerColor }}
            className="hover:underline"
          >
            {winnerName} {winnerScore}
          </Link>
          <span className="text-gray-500 mx-3">—</span>
          <Link
            to={seasonHref(`/teams/${loserId}`, result.season)}
            style={{ color: loserColor }}
            className="hover:underline"
          >
            {loserName} {loserScore}
          </Link>
        </div>
        <div className="text-sm text-gray-400 mt-2">
          <span style={{ color: winnerColor }} className="font-semibold">
            {result.predicted_winner} {winnerSpread.toFixed(1)}
          </span>
          <span className="mx-2 text-gray-600">·</span>
          <span>{winPct}% win probability</span>
        </div>
      </div>

      {/* Probability bar */}
      <div>
        <div className="flex justify-between text-sm mb-1">
          <Link
            to={seasonHref(`/teams/${result.home_team_id}`, result.season)}
            className={`${
              winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'
            } hover:underline`}
          >
            {result.home_team}
          </Link>
          <Link
            to={seasonHref(`/teams/${result.away_team_id}`, result.season)}
            className={`${
              !winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'
            } hover:underline`}
          >
            {result.away_team}
          </Link>
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
