import { useEffect, useMemo, useRef, useState } from 'react';
import {
  fetchPrediction,
  fetchTeamRankings,
  type FeatureContribution,
  type GroupContribution,
  type PredictionResult,
  type TeamRanking,
  type Venue,
} from '../api/client';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

const TEAM_1_COLOR = '#3b82f6'; // blue (matches PlayerCompare PLAYER_COLORS[0])
const TEAM_2_COLOR = '#ef4444'; // red

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [team1, setTeam1] = useState('');
  const [team2, setTeam2] = useState('');
  const [venue, setVenue] = useState<Venue>('home');
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
    <div className="max-w-3xl mx-auto">
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
          <ContributionsPanel
            contributions={result.top_contributors}
            homeTeam={result.home_team}
            awayTeam={result.away_team}
          />
          <GroupedContributionsPanel
            groups={result.contributions_by_group}
            homeTeam={result.home_team}
            awayTeam={result.away_team}
          />
        </div>
      )}
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

function ContributionsPanel({
  contributions,
  homeTeam,
  awayTeam,
}: {
  contributions: FeatureContribution[];
  homeTeam: string;
  awayTeam: string;
}) {
  if (contributions.length === 0) return null;
  const maxAbs = Math.max(...contributions.map((c) => Math.abs(c.contribution)), 0.01);

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-3">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Top Contributors
        </h2>
        <div className="text-[11px] text-gray-500">
          Toward <span style={{ color: TEAM_1_COLOR }}>{homeTeam}</span>
          {' / '}
          <span style={{ color: TEAM_2_COLOR }}>{awayTeam}</span>
        </div>
      </div>
      <div className="space-y-2">
        {contributions.map((c) => (
          <ContributionBar key={c.name} contribution={c} maxAbs={maxAbs} />
        ))}
      </div>
      <p className="text-[11px] text-gray-500 mt-4 leading-relaxed">
        Each bar is the model&apos;s sensitivity to that feature: how much the predicted margin would
        move toward zero if both teams were equal on that dimension. Sums don&apos;t exactly equal
        the headline because tree models have feature interactions.
      </p>
    </div>
  );
}

function ContributionBar({
  contribution,
  maxAbs,
}: {
  contribution: FeatureContribution;
  maxAbs: number;
}) {
  const pct = Math.min(100, (Math.abs(contribution.contribution) / maxAbs) * 50);
  const towardHome = contribution.contribution > 0;
  const color = towardHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  const fmtVal = (v: number) => (Math.abs(v) >= 10 ? v.toFixed(0) : v.toFixed(1));
  const sign = contribution.contribution > 0 ? '+' : '';
  return (
    <div className="grid grid-cols-[140px_1fr_60px] sm:grid-cols-[180px_1fr_70px] items-center gap-3 text-xs">
      <div className="truncate" title={`${contribution.label} (${contribution.group})`}>
        <span className="text-gray-200">{contribution.label}</span>
        <span className="text-gray-500 ml-1.5">{fmtVal(contribution.value)}</span>
      </div>
      {/* Centred bar — left half = "toward home/team1", right half = "toward away/team2". */}
      <div className="relative h-4 bg-gray-900 rounded">
        <div className="absolute inset-y-0 left-1/2 w-px bg-gray-700" />
        <div
          className="absolute inset-y-0 rounded"
          style={
            towardHome
              ? { left: `${50 - pct}%`, width: `${pct}%`, backgroundColor: color }
              : { left: '50%', width: `${pct}%`, backgroundColor: color }
          }
        />
      </div>
      <div
        className={`text-right font-mono ${towardHome ? 'text-blue-300' : 'text-red-300'}`}
        style={{ color }}
      >
        {sign}
        {contribution.contribution.toFixed(1)}
      </div>
    </div>
  );
}

function GroupedContributionsPanel({
  groups,
  homeTeam,
  awayTeam,
}: {
  groups: GroupContribution[];
  homeTeam: string;
  awayTeam: string;
}) {
  // Filter near-zero groups so the panel stays readable.
  const nonTrivial = groups.filter((g) => Math.abs(g.contribution) >= 0.05);
  if (nonTrivial.length === 0) return null;
  const maxAbs = Math.max(...nonTrivial.map((g) => Math.abs(g.contribution)), 0.01);

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-3">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          By Category
        </h2>
        <div className="text-[11px] text-gray-500">
          Sum across{' '}
          {nonTrivial.reduce((acc, g) => acc + g.feature_count, 0)} features
        </div>
      </div>
      <div className="space-y-2">
        {nonTrivial.map((g) => {
          const pct = Math.min(100, (Math.abs(g.contribution) / maxAbs) * 50);
          const towardHome = g.contribution > 0;
          const color = towardHome ? TEAM_1_COLOR : TEAM_2_COLOR;
          const sign = g.contribution > 0 ? '+' : '';
          return (
            <div
              key={g.group}
              className="grid grid-cols-[140px_1fr_60px] sm:grid-cols-[180px_1fr_70px] items-center gap-3 text-xs"
            >
              <div className="truncate text-gray-200" title={g.group}>
                {g.group}
              </div>
              <div className="relative h-4 bg-gray-900 rounded">
                <div className="absolute inset-y-0 left-1/2 w-px bg-gray-700" />
                <div
                  className="absolute inset-y-0 rounded"
                  style={
                    towardHome
                      ? { left: `${50 - pct}%`, width: `${pct}%`, backgroundColor: color }
                      : { left: '50%', width: `${pct}%`, backgroundColor: color }
                  }
                />
              </div>
              <div className="text-right font-mono" style={{ color }}>
                {sign}
                {g.contribution.toFixed(1)}
              </div>
            </div>
          );
        })}
      </div>
      <p className="text-[11px] text-gray-500 mt-3">
        Positive = toward {homeTeam}; negative = toward {awayTeam}.
      </p>
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
        <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-700 rounded shadow-lg max-h-72 overflow-y-auto">
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
