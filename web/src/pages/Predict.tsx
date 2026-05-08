import { useState } from 'react';
import { fetchPrediction, type PredictionResult, type Venue } from '../api/client';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [team1, setTeam1] = useState('');
  const [team2, setTeam2] = useState('');
  const [venue, setVenue] = useState<Venue>('home');
  const [result, setResult] = useState<PredictionResult | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!team1.trim() || !team2.trim()) return;
    setLoading(true);
    setError('');
    setResult(null);
    try {
      // The API takes (home, away) as team identifiers and a separate `venue`
      // saying who's hosting. We always send team1 as `home` and team2 as
      // `away` so the result perspective stays consistent — `venue=away`
      // tells the backend that team2 is the actual host.
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
    <div className="max-w-2xl mx-auto">
      <h1 className="text-2xl font-bold mb-1">Game Prediction</h1>
      <p className="text-xs text-gray-500 mb-5">
        Predicting matchups in the <span className="text-gray-300">{season - 1}-{String(season).slice(2)}</span> season.
        Switch the season selector in the nav to back-test historical games.
      </p>

      <form onSubmit={handleSubmit} className="bg-gray-800 rounded-lg p-6 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Team 1</label>
            <input
              type="text"
              value={team1}
              onChange={(e) => setTeam1(e.target.value)}
              placeholder="e.g. Duke"
              className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
            />
          </div>
          <div>
            <label className="block text-sm text-gray-400 mb-1">Team 2</label>
            <input
              type="text"
              value={team2}
              onChange={(e) => setTeam2(e.target.value)}
              placeholder="e.g. North Carolina"
              className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
            />
          </div>
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
        <div className="mt-4 bg-red-900/50 border border-red-800 rounded-lg p-4 text-red-300">{error}</div>
      )}

      {result && (
        <div className="mt-6 bg-gray-800 rounded-lg p-6 space-y-4">
          <div className="text-center">
            <div className="text-sm text-gray-400 mb-1">Predicted Winner</div>
            <div className="text-2xl font-bold text-blue-400">{result.predicted_winner}</div>
            <div className="text-xs text-gray-500 mt-1">
              {result.venue === 'neutral'
                ? 'Neutral site'
                : `at ${result.venue === 'home' ? result.home_team : result.away_team}`}
            </div>
          </div>

          {/* Probability Bar */}
          <div>
            <div className="flex justify-between text-sm mb-1">
              <span className="text-gray-300">{result.home_team}</span>
              <span className="text-gray-300">{result.away_team}</span>
            </div>
            <div className="flex h-6 rounded-full overflow-hidden">
              <div
                className="bg-blue-600 flex items-center justify-center text-xs font-medium text-white"
                style={{ width: `${team1Prob}%` }}
              >
                {team1Prob.toFixed(0)}%
              </div>
              <div
                className="bg-red-600 flex items-center justify-center text-xs font-medium text-white"
                style={{ width: `${100 - team1Prob}%` }}
              >
                {(100 - team1Prob).toFixed(0)}%
              </div>
            </div>
          </div>

          {/* Details */}
          <div className="grid grid-cols-2 gap-4 text-center">
            <div className="bg-gray-900 rounded p-3">
              <div className="text-xs text-gray-400 uppercase">Predicted Margin</div>
              <div className="text-xl font-bold mt-1">
                {result.predicted_margin > 0 ? '+' : ''}{result.predicted_margin.toFixed(1)}
              </div>
              <div className="text-[11px] text-gray-500 mt-0.5">{result.home_team} perspective</div>
            </div>
            <div className="bg-gray-900 rounded p-3">
              <div className="text-xs text-gray-400 uppercase">Win Probability</div>
              <div className="text-xl font-bold mt-1">
                {(result.home_win_probability * 100).toFixed(1)}%
              </div>
              <div className="text-[11px] text-gray-500 mt-0.5">{result.home_team} wins</div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
