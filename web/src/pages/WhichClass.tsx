import { useEffect, useMemo, useState } from 'react';
import { fetchPlayers, type PlayerRow } from '../api/client';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { SeasonLink } from '../components/SeasonLink';
import { classColor, classTagline } from '../components/archetypeColors';
import {
  QUIZ,
  buildQuizShare,
  isComplete,
  quizResult,
  type QuizResult,
} from '../lib/whichClass';

export default function WhichClass() {
  usePageTitle('Which Class Are You?');
  const { season } = useSeason();

  const [answers, setAnswers] = useState<Array<number | null>>(() =>
    QUIZ.map(() => null),
  );
  // Candidate players sharing the result's PRIMARY class in either slot
  // (primary or secondary). Dual-class matches are filtered out of this in the
  // result card. Fetched only once the quiz completes and the primary is known.
  const [matchPool, setMatchPool] = useState<PlayerRow[] | null>(null);
  const [copied, setCopied] = useState(false);

  const answered = answers.filter((a) => a != null).length;
  const complete = isComplete(answers);
  const result: QuizResult | null = useMemo(
    () => (complete ? quizResult(answers) : null),
    [complete, answers],
  );

  const primary = result?.primary ?? null;

  useEffect(() => {
    if (!primary) return;
    let cancelled = false;
    // Fetch the whole class pool (not a top-N) — dual-class matches are often
    // low-CamPom role players, so a small limit would truncate them and force a
    // wrong fallback to primary-only. It's one fetch on quiz completion.
    fetchPlayers({ archetype: primary, includeSecondaryArchetype: true, season, limit: 5000 })
      .then((r) => {
        if (!cancelled) setMatchPool(r.players);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [primary, season]);

  const pick = (qIdx: number, optIdx: number) => {
    setAnswers((prev) => {
      const next = prev.slice();
      next[qIdx] = optIdx;
      return next;
    });
    setCopied(false);
  };

  const retake = () => {
    setAnswers(QUIZ.map(() => null));
    setCopied(false);
  };

  const handleShare = () => {
    if (!result) return;
    const text = buildQuizShare(result, `${window.location.origin}/which-class`);
    navigator.clipboard
      ?.writeText(text)
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
  };

  return (
    <div className="space-y-5">
      <div>
        <h1 className="text-2xl font-bold text-gray-100">Which Class Are You?</h1>
        <p className="text-sm text-gray-400">
          Answer {QUIZ.length} questions to find your basketball archetype — one of the 12
          D&amp;D-inspired{' '}
          <SeasonLink to="/archetypes" className="text-blue-400 hover:underline">
            player classes
          </SeasonLink>
          .
        </p>
      </div>

      {/* Progress */}
      <div className="flex items-center gap-3">
        <div className="h-2 flex-1 overflow-hidden rounded-full bg-gray-800">
          <div
            className="h-full bg-blue-500 transition-all"
            style={{ width: `${(answered / QUIZ.length) * 100}%` }}
          />
        </div>
        <span className="text-xs text-gray-500">
          {answered}/{QUIZ.length}
        </span>
      </div>

      {/* Questions */}
      <div className="space-y-4">
        {QUIZ.map((q, qIdx) => (
          <div key={q.id} className="rounded-lg bg-gray-800 p-4">
            <div className="mb-3 text-sm font-semibold text-gray-100">
              {qIdx + 1}. {q.prompt}
            </div>
            <div className="grid gap-2 sm:grid-cols-2">
              {q.options.map((opt, optIdx) => {
                const selected = answers[qIdx] === optIdx;
                return (
                  <button
                    key={optIdx}
                    type="button"
                    onClick={() => pick(qIdx, optIdx)}
                    className={`rounded border px-3 py-2 text-left text-sm transition-colors ${
                      selected
                        ? 'border-blue-500 bg-blue-600/20 text-gray-100'
                        : 'border-gray-700 bg-gray-900 text-gray-300 hover:border-gray-600 hover:bg-gray-800'
                    }`}
                  >
                    {opt.label}
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </div>

      {/* Result */}
      {result ? (
        <ResultCard
          result={result}
          matchPool={matchPool}
          copied={copied}
          onShare={handleShare}
          onRetake={retake}
        />
      ) : (
        <div className="rounded-lg border border-gray-700 bg-gray-800/60 p-4 text-sm text-gray-400">
          Answer every question to reveal your class.
        </div>
      )}
    </div>
  );
}

function ClassBadge({ name, size }: { name: string; size: 'lg' | 'sm' }) {
  const color = classColor(name);
  return (
    <span
      className={`inline-flex items-center gap-2 rounded-full border px-3 font-bold ${
        size === 'lg' ? 'py-1 text-lg' : 'py-0.5 text-sm'
      }`}
      style={{ color, borderColor: color, backgroundColor: `${color}22` }}
    >
      {name}
    </span>
  );
}

function ResultCard({
  result,
  matchPool,
  copied,
  onShare,
  onRetake,
}: {
  result: QuizResult;
  matchPool: PlayerRow[] | null;
  copied: boolean;
  onShare: () => void;
  onRetake: () => void;
}) {
  const color = classColor(result.primary);

  // Prefer players who share BOTH classes (either slot order); fall back to
  // those whose primary class matches. Pool is CamPom-sorted from the API.
  const pool = matchPool ?? [];
  const wanted = new Set([result.primary, result.secondary]);
  const dual = pool.filter(
    (p) =>
      p.primary_class != null &&
      p.secondary_class != null &&
      wanted.has(p.primary_class) &&
      wanted.has(p.secondary_class) &&
      p.primary_class !== p.secondary_class,
  );
  const primaryOnly = pool.filter((p) => p.primary_class === result.primary);
  const bothMatch = dual.length > 0;
  const matches = bothMatch ? dual : primaryOnly;
  const exemplars = matches.slice(0, 3);
  // Deep-link to the Players page with the same filter. AND-mode over both
  // classes reproduces the dual set exactly; the fallback filters on primary.
  const showMoreTo = bothMatch
    ? `/players?archetypes=${result.primary},${result.secondary}&match=all`
    : `/players?archetypes=${result.primary}`;

  return (
    <div
      className="rounded-lg border-2 bg-gray-800 p-5"
      style={{ borderColor: color }}
    >
      <div className="text-xs uppercase tracking-wide text-gray-500">You are a</div>
      <div className="mt-1 flex flex-wrap items-center gap-3">
        <ClassBadge name={result.primary} size="lg" />
        <span className="text-sm text-gray-400">
          with a secondary of <ClassBadge name={result.secondary} size="sm" />
        </span>
      </div>
      <p className="mt-3 text-sm text-gray-300">
        <span className="font-semibold text-gray-100">{result.primary}:</span>{' '}
        {classTagline(result.primary)}
      </p>

      {exemplars.length > 0 && (
        <div className="mt-4">
          <div className="mb-2 text-xs uppercase tracking-wide text-gray-500">
            {bothMatch ? 'Players who share both your classes' : 'Players who share your class'}
          </div>
          <div className="flex flex-wrap gap-2">
            {exemplars.map((ex) => (
              <SeasonLink
                key={ex.player_id}
                to={`/players/${ex.player_id}`}
                className="rounded border border-gray-700 bg-gray-900 px-3 py-1.5 text-sm text-gray-200 hover:border-gray-600 hover:text-white"
              >
                {ex.name}
                {ex.team_name ? (
                  <span className="ml-1 text-xs text-gray-500">{ex.team_name}</span>
                ) : null}
              </SeasonLink>
            ))}
          </div>
          {matches.length > exemplars.length && (
            <SeasonLink
              to={showMoreTo}
              className="mt-2 inline-block text-sm text-blue-400 hover:underline"
            >
              Show all {matches.length} →
            </SeasonLink>
          )}
        </div>
      )}

      <div className="mt-5 flex items-center gap-2">
        <button
          type="button"
          onClick={onShare}
          className="rounded bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-500"
        >
          {copied ? 'Copied!' : 'Share'}
        </button>
        <button
          type="button"
          onClick={onRetake}
          className="rounded border border-gray-600 bg-gray-800 px-3 py-1.5 text-sm text-gray-200 hover:bg-gray-700"
        >
          Retake
        </button>
        <SeasonLink
          to="/archetypes"
          className="ml-auto text-sm text-blue-400 hover:underline"
        >
          Explore all 12 classes →
        </SeasonLink>
      </div>
    </div>
  );
}
