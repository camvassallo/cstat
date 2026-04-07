import { useState } from 'react';
import { fetchPrediction, type PredictionResult } from '../api/client';

export default function Predict() {
  const [home, setHome] = useState('');
  const [away, setAway] = useState('');
  const [neutral, setNeutral] = useState(false);
  const [result, setResult] = useState<PredictionResult | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!home.trim() || !away.trim()) return;
    setLoading(true);
    setError('');
    setResult(null);
    try {
      const r = await fetchPrediction(home.trim(), away.trim(), neutral);
      setResult(r);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Prediction failed');
    } finally {
      setLoading(false);
    }
  };

  const homeProb = result ? result.home_win_probability * 100 : 50;

  return (
    <div className="max-w-2xl mx-auto">
      <h1 className="text-2xl font-bold mb-6">Game Prediction</h1>

      <form onSubmit={handleSubmit} className="bg-gray-800 rounded-lg p-6 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Home Team</label>
            <input
              type="text"
              value={home}
              onChange={(e) => setHome(e.target.value)}
              placeholder="e.g. Duke Blue Devils"
              className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
            />
          </div>
          <div>
            <label className="block text-sm text-gray-400 mb-1">Away Team</label>
            <input
              type="text"
              value={away}
              onChange={(e) => setAway(e.target.value)}
              placeholder="e.g. North Carolina Tar Heels"
              className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
            />
          </div>
        </div>

        <label className="flex items-center gap-2 text-sm text-gray-300">
          <input
            type="checkbox"
            checked={neutral}
            onChange={(e) => setNeutral(e.target.checked)}
            className="rounded border-gray-600"
          />
          Neutral site
        </label>

        <button
          type="submit"
          disabled={loading || !home.trim() || !away.trim()}
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
                style={{ width: `${homeProb}%` }}
              >
                {homeProb.toFixed(0)}%
              </div>
              <div
                className="bg-red-600 flex items-center justify-center text-xs font-medium text-white"
                style={{ width: `${100 - homeProb}%` }}
              >
                {(100 - homeProb).toFixed(0)}%
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
            </div>
            <div className="bg-gray-900 rounded p-3">
              <div className="text-xs text-gray-400 uppercase">Win Probability</div>
              <div className="text-xl font-bold mt-1">
                {(result.home_win_probability * 100).toFixed(1)}%
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
