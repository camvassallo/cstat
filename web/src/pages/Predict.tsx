import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
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
import {
  matchupKey,
  parseSlotSeasonParam,
  slotYears,
  teamLabel,
  toRequest,
  type Matchup,
  type SlotYears,
} from '../lib/predictSlots';
import {
  seasonHref,
  setPageSeasons,
  useAvailableSeasons,
  useSeason,
} from '../components/season';
import ModeToggle from '../components/ModeToggle';
import { conferenceLabel, conferenceSearchText } from '../lib/conferences';
import { usePageTitle } from '../components/usePageTitle';
import { camTier, camTierColor, camTitle } from '../components/cam';
import { classColor, classTitle, provisionalMeta } from '../components/archetypeColors';
import { heightString, shortDate } from '../components/format';
import { RosterWaffle } from '../components/RosterWaffle';
import { TeamShotDiet } from '../components/TeamShotDiet';
import { Link } from 'react-router-dom';

const TEAM_1_COLOR = '#3b82f6'; // blue (matches PlayerCompare PLAYER_COLORS[0])
const TEAM_2_COLOR = '#ef4444'; // red

// ---------------------------------------------------------------------------
// Time Machine (cross-year) mode
// ---------------------------------------------------------------------------
// `season` is the default and in it this page behaves exactly as it did before
// cross-year existed: one site-wide `?season=`, one team list, the same URL
// shape, `as_of_date` available. `year` gives each side its own season and the
// matchup becomes a what-if that never happened.

type PredictMode = 'season' | 'year';

/// Wording is fixed by `ModeToggle`'s cross-year contract so Predict,
/// PlayerCompare and the comparable-players list all read the same. "Any year"
/// over "cross-year": the latter is how we talk about the work, not what the
/// reader is choosing between.
const MODE_OPTIONS = [
  { value: 'year' as const, label: 'Any year', title: 'Give each team its own season' },
  { value: 'season' as const, label: 'Season', title: 'Both teams from one season' },
];

/// The empty override that hides the navbar season picker. A module constant
/// rather than a fresh `[]` per render — `setPageSeasons` de-dupes by value,
/// but there is no reason to hand it the work.
const NO_GLOBAL_SEASONS: readonly number[] = [];

// ---------------------------------------------------------------------------
// Per-season team lists
// ---------------------------------------------------------------------------
// Cross-year needs a team list per slot, not one for the page: D-I membership
// genuinely differs by year, so a 2026 list cannot autocomplete a 2015 team and
// a 2026 rankings row is the wrong baseline for a 2015 four-factor comparison.
//
// Four call sites want a list on any given render (two pickers, two stat
// columns), and in single-season mode all four want the SAME one — hence the
// module-level cache, which turns that into a single request. A rejected fetch
// is evicted rather than kept: caching the failure would leave that season with
// an empty picker for the rest of the session.

const rankingsBySeason = new Map<number, Promise<TeamRanking[]>>();

function loadRankings(season: number): Promise<TeamRanking[]> {
  const cached = rankingsBySeason.get(season);
  if (cached) return cached;
  const pending = fetchTeamRankings(season).then((r) => r.teams);
  pending.catch(() => rankingsBySeason.delete(season));
  rankingsBySeason.set(season, pending);
  return pending;
}

function useTeamRankings(season: number): TeamRanking[] {
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  useEffect(() => {
    let alive = true;
    loadRankings(season)
      .then((t) => {
        if (alive) setTeams(t);
      })
      .catch(() => {
        // Pickers still work as free text, and the panels that need a row
        // render nothing rather than a wrong-era one.
      });
    return () => {
      alive = false;
    };
  }, [season]);
  return teams;
}

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [searchParams, setSearchParams] = useSearchParams();
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
  // Per-slot years. Their PRESENCE is what marks a link as cross-year — there
  // is no separate mode param, the same way PlayerCompare reads its mode off
  // the `@year` tokens inside `ids`. A single-season link carries neither and
  // keeps the shape every ticker and schedule row already builds.
  const urlHomeSeason = parseSlotSeasonParam(searchParams.get('home_season'));
  const urlAwaySeason = parseSlotSeasonParam(searchParams.get('away_season'));
  const urlIsCrossYear = urlHomeSeason != null || urlAwaySeason != null;

  const [mode, setMode] = useState<PredictMode>(urlIsCrossYear ? 'year' : 'season');
  // One-way, like PlayerCompare's: a cross-year link pasted into an already-
  // mounted page turns the mode ON. It cannot turn it off, because leaving the
  // mode is itself what strips the params.
  useEffect(() => {
    if (urlIsCrossYear) setMode('year');
  }, [urlIsCrossYear]);
  const crossYear = mode === 'year';

  // Cross-year gives each slot its own year, so one site-wide season means
  // nothing — publishing an empty list hides the navbar picker. The unmount
  // cleanup is what stops it staying hidden on whatever the user opens next:
  // the override is module state, not page state.
  useEffect(() => {
    setPageSeasons(crossYear ? NO_GLOBAL_SEASONS : null);
    return () => setPageSeasons(null);
  }, [crossYear]);

  const { seasons: allSeasons } = useAvailableSeasons();

  const [team1, setTeam1] = useState(urlHome);
  const [team2, setTeam2] = useState(urlAway);
  const [team1Season, setTeam1Season] = useState(urlHomeSeason ?? season);
  const [team2Season, setTeam2Season] = useState(urlAwaySeason ?? season);
  const [venue, setVenue] = useState<Venue>(initialVenue);
  const [asOfDate, setAsOfDate] = useState(urlAsOfDate);
  // The result travels with the years that were REQUESTED, not with the form's
  // current ones: the response echoes a single `season` (the home side's), and
  // editing a picker afterwards must not relabel a prediction that is already
  // on screen.
  const [loaded, setLoaded] = useState<{
    result: PredictionResult;
    years: SlotYears;
  } | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  // Single-season mode ignores the per-slot state entirely and runs on the
  // site-wide season, so `?season=` keeps driving the page exactly as before.
  const homeSeason = crossYear ? team1Season : season;
  const awaySeason = crossYear ? team2Season : season;
  const formShowYears = homeSeason !== awaySeason;

  // A team list per slot. Same season on both sides — every single-season
  // render — is one shared request, see `loadRankings`.
  const team1Teams = useTeamRankings(homeSeason);
  const team2Teams = useTeamRankings(awaySeason);

  // One prediction path for both entry points (the form, and a URL the page was
  // opened on) so the two cannot drift. `seq` supersedes rather than cancels:
  // two fetches are easily in flight at once — submit while a deep-link is
  // still loading — and the older answer must not be the one that lands.
  const seqRef = useRef(0);
  const lastKeyRef = useRef<string | null>(null);

  const runPrediction = useCallback(async (m: Matchup) => {
    const seq = ++seqRef.current;
    lastKeyRef.current = matchupKey(m);
    setLoading(true);
    setError('');
    setLoaded(null);
    try {
      const r = await fetchPrediction(toRequest(m));
      if (seqRef.current !== seq) return;
      setLoaded({ result: r, years: slotYears(m) });
    } catch (err) {
      if (seqRef.current !== seq) return;
      setError(err instanceof Error ? err.message : 'Prediction failed');
    } finally {
      if (seqRef.current === seq) setLoading(false);
    }
  }, []);

  // When teams arrive via URL params (deep-link from a schedule row, ticker
  // tile, or shared link), kick off the prediction automatically. Re-fires
  // when the URL or season changes so /predict?home=A&away=B — and its
  // cross-year form, /predict?home=A&home_season=2015&away=B&away_season=2026 —
  // remain first-class destinations.
  useEffect(() => {
    if (!urlHome.trim() || !urlAway.trim()) return;
    const m: Matchup = {
      home: urlHome.trim(),
      away: urlAway.trim(),
      venue: initialVenue,
      homeSeason: urlIsCrossYear ? (urlHomeSeason ?? season) : season,
      awaySeason: urlIsCrossYear ? (urlAwaySeason ?? season) : season,
      asOfDate: urlAsOfDate,
      crossYear: urlIsCrossYear,
    };
    // Mirror the link into the form, so the pickers show what is on screen.
    setTeam1(m.home);
    setTeam2(m.away);
    setTeam1Season(m.homeSeason);
    setTeam2Season(m.awaySeason);
    setVenue(m.venue);
    setAsOfDate(m.asOfDate);
    // Cross-year submits write these same params back to the URL; without the
    // guard that write returns here as a second, identical request.
    if (matchupKey(m) === lastKeyRef.current) return;
    void runPrediction(m);
    // The early-return on empty `urlHome`/`urlAway` short-circuits the first
    // render before pickers have any value. `initialVenue` is intentionally
    // omitted from the deps — it's recomputed each render from `urlVenue`
    // (which is in the deps), so reading its current value inside the effect
    // is correct.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    urlHome,
    urlAway,
    urlVenue,
    urlAsOfDate,
    urlHomeSeason,
    urlAwaySeason,
    urlIsCrossYear,
    season,
    runPrediction,
  ]);

  const changeMode = (next: PredictMode) => {
    if (next === mode) return;
    if (next === 'year') {
      // Pin both slots to the season already on screen, so the toggle makes the
      // years editable rather than different.
      setTeam1Season(season);
      setTeam2Season(season);
    } else if (urlIsCrossYear) {
      // Strip the per-slot params, or the sync effect above reads them straight
      // back and flips the mode on again.
      setSearchParams(
        (prev) => {
          const p = new URLSearchParams(prev);
          p.delete('home_season');
          p.delete('away_season');
          return p;
        },
        { replace: true },
      );
    }
    setMode(next);
  };

  const handleSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!team1.trim() || !team2.trim()) return;
    const m: Matchup = {
      home: team1.trim(),
      away: team2.trim(),
      venue,
      homeSeason,
      awaySeason,
      asOfDate: asOfDate.trim(),
      crossYear,
    };
    if (crossYear) {
      // Cross-year state lives nowhere but the URL: there is no site-wide
      // `?season=` to fall back on (the picker is hidden), and the mode itself
      // is read back off these two params — so writing them on submit is what
      // makes a Time Machine matchup shareable at all. `replace` keeps a run of
      // experiments out of the back button. Single-season deliberately does NOT
      // do this: that URL shape is what the ticker and schedule rows build, and
      // it stays exactly as it was.
      setSearchParams(
        (prev) => {
          const p = new URLSearchParams(prev);
          p.set('home', m.home);
          p.set('away', m.away);
          p.set('venue', m.venue);
          p.set('home_season', String(m.homeSeason));
          p.set('away_season', String(m.awaySeason));
          // Can't apply cross-year, and a stale one in a shared link is noise.
          p.delete('as_of_date');
          return p;
        },
        { replace: true },
      );
    }
    await runPrediction(m);
  };

  const team1Prob = loaded ? loaded.result.home_win_probability * 100 : 50;

  const venueLabel: Record<Venue, string> = {
    home: team1.trim()
      ? `${teamLabel(team1.trim(), homeSeason, formShowYears)} home`
      : 'Team 1 home',
    neutral: 'Neutral',
    away: team2.trim()
      ? `${teamLabel(team2.trim(), awaySeason, formShowYears)} home`
      : 'Team 2 home',
  };

  return (
    <div className="max-w-4xl mx-auto">
      <div className="flex flex-wrap items-start justify-between gap-3 mb-5">
        <div>
          <h1 className="text-2xl font-bold mb-1">Game Prediction</h1>
          {crossYear ? (
            /* Keyed on whether the YEARS differ, not on the mode. The backend
               draws the same line — `cross_era` is `home_season != away_season`
               — so a cross-year matchup sitting on one year is served as an
               ordinary forecast, prior meetings and all. Asserting "these two
               never met" there would be false, and would sit directly above a
               Previous Matchups panel listing the times they did. */
            <p className="text-xs text-gray-500 max-w-2xl">
              {formShowYears ? (
                <>
                  These two never met and never could — the eras played
                  different games, different pace and different rules — so take
                  the result as a fun what-if rather than a line. The one bias
                  we have measured is small and in a known direction: about a
                  point in the more recent team's favor.
                </>
              ) : (
                <>
                  Each side carries its own year. Point them at different ones
                  and the matchup becomes a what-if: the eras played different
                  games, and the further apart the years, the more of the gap is
                  the era rather than the teams. On a single year this is the
                  ordinary forecast, prior meetings and all.
                </>
              )}
            </p>
          ) : (
            <p className="text-xs text-gray-500">
              Predicting{' '}
              <span className="text-gray-300">
                {season - 1}-{String(season).slice(2)}
              </span>{' '}
              matchups.
            </p>
          )}
        </div>
        <ModeToggle
          options={MODE_OPTIONS}
          value={mode}
          onChange={changeMode}
          ariaLabel="Prediction mode"
          className="self-start shrink-0"
        />
      </div>

      <form onSubmit={handleSubmit} className="bg-gray-800 rounded-lg p-6 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <TeamPicker
            label="Team 1"
            value={team1}
            onChange={setTeam1}
            teams={team1Teams}
            placeholder="e.g. Duke"
            color={TEAM_1_COLOR}
            season={crossYear ? homeSeason : null}
            seasons={allSeasons}
            onSeasonChange={setTeam1Season}
          />
          <TeamPicker
            label="Team 2"
            value={team2}
            onChange={setTeam2}
            teams={team2Teams}
            placeholder="e.g. Michigan"
            color={TEAM_2_COLOR}
            season={crossYear ? awaySeason : null}
            seasons={allSeasons}
            onSeasonChange={setTeam2Season}
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

        {/* Point-in-time is a within-season idea — the cohort behind it is built
            for exactly one year — so cross-year has no honest version of it and
            the backend rejects the combination outright. Hiding the field is
            what keeps the mode from offering a guaranteed error. */}
        <div className={crossYear ? 'hidden' : undefined}>
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
              Point-in-time projection: player value rebuilt from game-by-game
              data up to {asOfDate}. Team-level features remain season
              aggregates.
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

      {loaded && (
        <div className="mt-6 space-y-4">
          {/* Order matches the user's mental flow: answer first,
              quantitative breakdown second, personnel context third,
              historical evidence last (most useful after you've
              internalised the projection's reasoning).
              PreviousMatchups returns null when the teams haven't
              played, so absent-history matchups still flow cleanly. */}
          <ResultHeadline
            result={loaded.result}
            years={loaded.years}
            team1Prob={team1Prob}
          />
          <SideBySideStats result={loaded.result} years={loaded.years} />
          <ArchetypeRow result={loaded.result} years={loaded.years} />
          <ShotDietRow result={loaded.result} years={loaded.years} />
          <RosterCompare result={loaded.result} years={loaded.years} />
          <PreviousMatchups result={loaded.result} />
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

function ArchetypeRow({
  result,
  years,
}: {
  result: PredictionResult;
  years: SlotYears;
}) {
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
          teamName={teamLabel(result.home_team, years.home, years.show)}
          teamId={result.home_team_id}
          season={years.home}
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
          teamName={teamLabel(result.away_team, years.away, years.show)}
          teamId={result.away_team_id}
          season={years.away}
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

function ShotDietRow({
  result,
  years,
}: {
  result: PredictionResult;
  years: SlotYears;
}) {
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
          teamName={teamLabel(result.home_team, years.home, years.show)}
          teamId={result.home_team_id}
          season={years.home}
          color={TEAM_1_COLOR}
        >
          {hasHome ? (
            <TeamShotDiet roster={result.roster_home} />
          ) : (
            <EmptyNote>No shot data</EmptyNote>
          )}
        </TeamPanel>
        <TeamPanel
          teamName={teamLabel(result.away_team, years.away, years.show)}
          teamId={result.away_team_id}
          season={years.away}
          color={TEAM_2_COLOR}
        >
          {hasAway ? (
            <TeamShotDiet roster={result.roster_away} />
          ) : (
            <EmptyNote>No shot data</EmptyNote>
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
// Roster Compare panel — side-by-side roster table (the 8 players who log
// the most minutes per team) with archetype chips and rate stats. The
// Archetype + Shot Diet rows above already render the visual identity per
// team; this panel drills into the specific players carrying it.
//
// Minutes, not CAM, and the distinction matters because this panel SLICES.
// `get_team_roster` already returns minutes-desc (CAM breaks ties) and the
// roster query has no minutes or games floor, so re-sorting by CAM here would
// change *which* players appear, not just their order — a six-game bench
// player with a noisy CAM would displace a thirty-minute starter. For a
// matchup preview the rotation is the thing worth showing, and each row still
// prints its CAM. TeamDetail does sort its roster by CAM, but it renders the
// whole list rather than a top-N, so there the sort is presentation only.
// ---------------------------------------------------------------------------

const ROSTER_PANEL_LIMIT = 8;

function RosterCompare({
  result,
  years,
}: {
  result: PredictionResult;
  years: SlotYears;
}) {
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
        <div className="text-[11px] text-gray-500">
          Top {ROSTER_PANEL_LIMIT} by minutes
        </div>
      </div>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-x-6 gap-y-4">
        <RosterColumn
          teamName={teamLabel(result.home_team, years.home, years.show)}
          teamId={result.home_team_id}
          season={years.home}
          color={TEAM_1_COLOR}
          roster={homeTop}
        />
        <RosterColumn
          teamName={teamLabel(result.away_team, years.away, years.show)}
          teamId={result.away_team_id}
          season={years.away}
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
  const tier = camTier(p.campom);
  const tierColor = camTierColor(tier);
  const mpg = p.minutes_per_game != null ? p.minutes_per_game.toFixed(1) : '—';
  const campomScore = p.campom != null ? p.campom.toFixed(1) : '—';
  // Height rather than games played: this list is already ordered by minutes
  // and the row already prints minutes, so games played was a third reading of
  // "how much did they play" and told you nothing the other two didn't. Height
  // is the one thing on the row the archetype chip and CAM don't cover, and
  // it's the dimension a matchup is actually argued over — more so across
  // eras, where the two rosters were built to different templates. Populated
  // for 97-100% of players in every ingested season; the rare miss drops the
  // field rather than printing a placeholder.
  const height = heightString(p.height_inches);
  return (
    <li className="grid grid-cols-[1fr_auto_auto] items-center gap-2 text-sm">
      <div className="min-w-0 truncate">
        <Link
          to={seasonHref(`/players/${p.player_id}`, season)}
          className="text-gray-100 hover:underline truncate"
        >
          {p.name}
        </Link>
        {p.primary_class &&
          (() => {
            const prov = provisionalMeta(p);
            return (
              <span
                title={classTitle(p.primary_class) + (prov.note ? ` · ${prov.note}` : '')}
                className={`ml-1.5 text-[10px] uppercase tracking-wide font-semibold ${
                  prov.provisional ? 'opacity-70' : ''
                }`}
                style={{ color: classColor(p.primary_class) }}
              >
                {p.primary_class.slice(0, 3)}
                {prov.shortYear && (
                  <span className="ml-0.5 text-gray-500 lowercase">{prov.shortYear}</span>
                )}
              </span>
            );
          })()}
      </div>
      <div className="text-[11px] text-gray-500 font-mono whitespace-nowrap">
        {mpg} mpg{height && ` · ${height}`}
      </div>
      <div
        className={`text-[11px] font-mono px-1.5 py-0.5 rounded border whitespace-nowrap ${tierColor}`}
        title={camTitle(p.campom, p.campom_o, p.campom_d) || undefined}
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
  // Cross-year needs no guard of its own: two teams from different seasons
  // never played, so the backend skips the query outright rather than running
  // one whose only non-empty answer would be wrong, and this list arrives
  // empty. The "this season" wording below is safe for the same reason.
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

/// Find a team's rankings row.
///
/// Keyed on the season-scoped UUID rather than the name, because cross-year
/// hands this two different eras' lists and those lists overlap by name almost
/// entirely: matching "Duke" against whichever list happens to be loaded would
/// quietly print 2026 Duke's four factors under a header reading 2015 Duke. The
/// id can only match the right era, so a list that is still loading (or that
/// genuinely has no row for this team) renders nothing instead — which is what
/// the name lookup did for a missing team anyway.
function lookupTeam(teamId: string, teams: TeamRanking[]): TeamRanking | undefined {
  return teams.find((t) => t.team_id === teamId);
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
  years,
}: {
  result: PredictionResult;
  years: SlotYears;
}) {
  // One list per side. Same year on both — every single-season render — is one
  // shared request; different years fetch the two eras separately, which is the
  // point: a 2026 rankings row is the wrong baseline for a 2015 team.
  const homeTeams = useTeamRankings(years.home);
  const awayTeams = useTeamRankings(years.away);
  const home = lookupTeam(result.home_team_id, homeTeams);
  const away = lookupTeam(result.away_team_id, awayTeams);
  // Compute season-aware league averages from the rankings list we already
  // fetched, so the possession panels' league-baseline highlighting tracks
  // the actual era's stats instead of frozen 2008-vintage Dean Oliver
  // figures. One baseline per side rather than one per page: across eras the
  // two teams are average against different leagues, and measuring each
  // team's deviation against its own league is what makes "who wins this
  // row" an era-relative question instead of a league-drift question.
  // Same-season matchups feed both sides the identical list, so this is a
  // no-op there. `useMemo` because each list is stable across renders.
  const homeLeague = useMemo(() => computeLeagueAverages(homeTeams), [homeTeams]);
  const awayLeague = useMemo(() => computeLeagueAverages(awayTeams), [awayTeams]);
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
      home: home.adj_tempo == null ? null : home.adj_tempo - homeLeague.TEMPO,
      away: away.adj_tempo == null ? null : away.adj_tempo - awayLeague.TEMPO,
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
        <div className="text-[11px] text-gray-500">
          {years.show ? 'Season averages · highlights are era-relative' : 'Season averages'}
        </div>
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
              {teamLabel(home.name, years.home, years.show)}
            </div>
            <div className="w-20 text-center text-[11px] uppercase tracking-wide text-gray-500">
              stat
            </div>
            <div className="text-left text-sm font-medium" style={{ color: TEAM_2_COLOR }}>
              {teamLabel(away.name, years.away, years.show)}
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
          offName={teamLabel(home.name, years.home, years.show)}
          offColor={TEAM_1_COLOR}
          defColor={TEAM_2_COLOR}
          offLeagueAvg={homeLeague}
          defLeagueAvg={awayLeague}
        />

        {/* Column 3: when away team has the ball */}
        <PossessionPanel
          offTeam={away}
          defTeam={home}
          offName={teamLabel(away.name, years.away, years.show)}
          offColor={TEAM_2_COLOR}
          defColor={TEAM_1_COLOR}
          offLeagueAvg={awayLeague}
          defLeagueAvg={homeLeague}
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
  /// League-average baselines in the same units as off/def, one per side.
  /// The highlight goes to whichever side's deviation from ITS OWN baseline
  /// is larger (signed in their favor — see `PossessionRow`). Two values
  /// rather than one because a cross-era row compares teams that were average
  /// against different leagues; same-season rows pass the identical number
  /// twice and the arithmetic collapses back to what it always was.
  offLeagueAvg: number;
  defLeagueAvg: number;
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
  offName,
  offColor,
  defColor,
  offLeagueAvg,
  defLeagueAvg,
}: {
  offTeam: TeamRanking;
  defTeam: TeamRanking;
  /// Display name for the offensive side, already year-suffixed when the two
  /// slots disagree. Passed in rather than read off `offTeam.name` so the
  /// panel header can't be the one place on the page that drops the year.
  offName: string;
  offColor: string;
  defColor: string;
  offLeagueAvg: PossessionLeagueAvg;
  defLeagueAvg: PossessionLeagueAvg;
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
      offLeagueAvg: offLeagueAvg.EFF,
      defLeagueAvg: defLeagueAvg.EFF,
      better: 'high',
      format: fmtEff,
    },
    {
      label: 'eFG%',
      offValue: offTeam.effective_fg_pct,
      defValue: defTeam.opp_effective_fg_pct,
      offLeagueAvg: offLeagueAvg.eFG,
      defLeagueAvg: defLeagueAvg.eFG,
      better: 'high',
      format: fmtPct,
    },
    {
      label: 'TOV%',
      offValue: offTeam.turnover_pct,
      defValue: defTeam.opp_turnover_pct,
      offLeagueAvg: offLeagueAvg.TOV,
      defLeagueAvg: defLeagueAvg.TOV,
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
      offLeagueAvg: offLeagueAvg.ORB,
      defLeagueAvg: defLeagueAvg.ORB,
      better: 'high',
      format: fmtPct,
    },
    {
      label: 'FT Rate',
      offValue: offTeam.ft_rate,
      defValue: defTeam.opp_ft_rate,
      offLeagueAvg: offLeagueAvg.FT,
      defLeagueAvg: defLeagueAvg.FT,
      better: 'high',
      format: fmtRatio,
    },
  ];

  return (
    <div>
      <div className="text-xs uppercase tracking-wide text-gray-500 mb-3 text-center">
        When <span style={{ color: offColor }}>{offName}</span> has the ball
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
  //
  // Each side is measured against its OWN league, which only differs cross-era
  // but matters a lot there: 2015 D-I turned it over ~2.7pp more often than
  // 2026 D-I, so a shared baseline would hand the modern side a turnover edge
  // it did not earn. Deviation-from-own-era is the era-relative reading.
  let offBetter = false;
  let defBetter = false;
  if (row.offValue != null && row.defValue != null) {
    const offStrength =
      row.better === 'high'
        ? row.offValue - row.offLeagueAvg
        : row.offLeagueAvg - row.offValue;
    const defStrength =
      row.better === 'high'
        ? row.defLeagueAvg - row.defValue
        : row.defValue - row.defLeagueAvg;
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
  years,
  team1Prob,
}: {
  result: PredictionResult;
  years: SlotYears;
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

  const winnerSeason = winnerIsHome ? years.home : years.away;
  const loserSeason = winnerIsHome ? years.away : years.home;
  const winnerName = teamLabel(
    winnerIsHome ? result.home_team : result.away_team,
    winnerSeason,
    years.show,
  );
  const loserName = teamLabel(
    winnerIsHome ? result.away_team : result.home_team,
    loserSeason,
    years.show,
  );
  const winnerId = winnerIsHome ? result.home_team_id : result.away_team_id;
  const loserId = winnerIsHome ? result.away_team_id : result.home_team_id;
  const winnerScore = winnerIsHome
    ? result.predicted_home_score
    : result.predicted_away_score;
  const loserScore = winnerIsHome
    ? result.predicted_away_score
    : result.predicted_home_score;

  const homeName = teamLabel(result.home_team, years.home, years.show);
  const awayName = teamLabel(result.away_team, years.away, years.show);
  const venueText =
    result.venue === 'neutral'
      ? 'Neutral site'
      : result.venue === 'home'
        ? `at ${homeName}`
        : `at ${awayName}`;

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
      title: `Point-in-time CAM as of ${result.as_of_date}. Team-level features (AdjEM, SOS, four factors) still reflect end-of-season state.`,
    },
    // The other four labels all describe how much of a season the number saw.
    // This one varies on a different axis entirely — the game never happened —
    // so it gets its own wording rather than borrowing "leaky", which would
    // read as an accuracy warning on a surface whose whole premise is that the
    // matchup is hypothetical.
    cross_era: {
      label: 'What-if',
      cls: 'bg-violet-900/60 text-violet-300',
      title:
        'These two teams are from different seasons, so they never met. Each side is its whole season as it actually played, put on one court — a fun what-if, not a line. The eras differ in pace, rules and shot selection, and the tilt we can measure is about a point toward the more recent team.',
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
            to={seasonHref(`/teams/${winnerId}`, winnerSeason)}
            style={{ color: winnerColor }}
            className="hover:underline"
          >
            {winnerName} {winnerScore}
          </Link>
          <span className="text-gray-500 mx-3">—</span>
          <Link
            to={seasonHref(`/teams/${loserId}`, loserSeason)}
            style={{ color: loserColor }}
            className="hover:underline"
          >
            {loserName} {loserScore}
          </Link>
        </div>
        <div className="text-sm text-gray-400 mt-2">
          <span style={{ color: winnerColor }} className="font-semibold">
            {winnerName} {winnerSpread.toFixed(1)}
          </span>
          <span className="mx-2 text-gray-600">·</span>
          <span>{winPct}% win probability</span>
        </div>
      </div>

      {/* Probability bar */}
      <div>
        <div className="flex justify-between text-sm mb-1">
          <Link
            to={seasonHref(`/teams/${result.home_team_id}`, years.home)}
            className={`${
              winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'
            } hover:underline`}
          >
            {homeName}
          </Link>
          <Link
            to={seasonHref(`/teams/${result.away_team_id}`, years.away)}
            className={`${
              !winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'
            } hover:underline`}
          >
            {awayName}
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
  season,
  seasons,
  onSeasonChange,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  teams: TeamRanking[];
  placeholder: string;
  color: string;
  /// This slot's year, or null in single-season mode where the site-wide
  /// picker owns it and no per-slot control is rendered.
  season: number | null;
  seasons: readonly number[];
  onSeasonChange: (next: number) => void;
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
          conferenceSearchText(t.conference).includes(q),
      )
      .slice(0, 10);
  }, [teams, value]);

  return (
    <div className="relative">
      {/* The year sits on the label row rather than under the input: a
          cross-year pair of pickers is already two stacked blocks on a phone,
          and giving each one a third row pushes the Predict button off the
          first screen. */}
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="text-sm text-gray-400">
          <span style={{ color }} className="font-medium">
            ●
          </span>{' '}
          {label}
        </label>
        {season != null && (
          <SlotSeasonSelect
            season={season}
            seasons={seasons}
            color={color}
            label={label}
            onChange={onSeasonChange}
          />
        )}
      </div>
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
                {t.conference ? conferenceLabel(t.conference) : '—'}
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

/// The per-slot year control, rendered only in cross-year mode.
///
/// Offers every season the site has, not just the ones this team existed in:
/// unlike PlayerCompare's version there is no per-entity `available_seasons` to
/// narrow it — the team is a free-text name that may not resolve at all yet. A
/// year the program was not Division I in is therefore reachable, and lands on
/// the backend's per-side 404, which names both the side and the year for
/// exactly this reason.
function SlotSeasonSelect({
  season,
  seasons,
  color,
  label,
  onChange,
}: {
  season: number;
  seasons: readonly number[];
  color: string;
  label: string;
  onChange: (next: number) => void;
}) {
  // A slot pointed at a year outside the list (a pasted link, or a season the
  // fallback list predates) still has to show the year it is on, or the menu
  // would silently claim otherwise.
  const options = seasons.includes(season) ? seasons : [season, ...seasons];
  return (
    <select
      value={season}
      onChange={(e) => onChange(Number(e.target.value))}
      aria-label={`Season for ${label}`}
      className="bg-gray-900 border rounded px-1.5 py-0.5 text-xs text-gray-200 focus:outline-none focus:border-blue-500"
      style={{ borderColor: color }}
    >
      {options
        .slice()
        .sort((a, b) => b - a)
        .map((y) => (
          <option key={y} value={y}>
            {y}
          </option>
        ))}
    </select>
  );
}
