import { describe, expect, it } from 'vitest';
import { CLASS_ORDER } from '../components/archetypeColors';
import {
  QUIZ,
  buildQuizShare,
  isComplete,
  quizResult,
  scoreQuiz,
} from './whichClass';

describe('QUIZ data', () => {
  it('every question has 4 options and only weights known classes', () => {
    const known = new Set(CLASS_ORDER);
    for (const q of QUIZ) {
      expect(q.options.length).toBe(4);
      for (const opt of q.options) {
        for (const cls of Object.keys(opt.weights)) {
          expect(known.has(cls)).toBe(true);
        }
      }
    }
  });

  it('every class is reachable (appears in at least one option)', () => {
    const seen = new Set<string>();
    for (const q of QUIZ) {
      for (const opt of q.options) {
        for (const cls of Object.keys(opt.weights)) seen.add(cls);
      }
    }
    for (const cls of CLASS_ORDER) expect(seen.has(cls)).toBe(true);
  });
});

describe('scoreQuiz / quizResult', () => {
  const unanswered = QUIZ.map(() => null);

  it('sums weights and sorts high→low', () => {
    // Answer only Q0 option 0 (Sorcerer +2, Wizard +1).
    const answers = QUIZ.map((_, i) => (i === 0 ? 0 : null));
    const ranking = scoreQuiz(answers);
    expect(ranking[0]).toEqual({ className: 'Sorcerer', score: 2 });
    expect(ranking.find((r) => r.className === 'Wizard')?.score).toBe(1);
    // Everything else 0.
    const nonzero = ranking.filter((r) => r.score > 0).map((r) => r.className).sort();
    expect(nonzero).toEqual(['Sorcerer', 'Wizard']);
  });

  it('ties broken by CLASS_ORDER (deterministic)', () => {
    const ranking = scoreQuiz(unanswered); // all zero
    expect(ranking.map((r) => r.className)).toEqual([...CLASS_ORDER]);
  });

  it('quizResult returns the top two', () => {
    const answers = QUIZ.map((_, i) => (i === 0 ? 0 : null));
    const { primary, secondary } = quizResult(answers);
    expect(primary).toBe('Sorcerer');
    expect(secondary).toBe('Wizard');
  });

  it('a full run of "assist/pass" answers lands on a creator class', () => {
    // Pick the playmaking-leaning option in each question where present.
    const answers = QUIZ.map((q) => {
      const idx = q.options.findIndex((o) => o.weights.Bard || o.weights.Wizard);
      return idx >= 0 ? idx : 0;
    });
    const { primary } = quizResult(answers);
    expect(['Bard', 'Wizard']).toContain(primary);
  });
});

describe('isComplete', () => {
  it('false until every question answered', () => {
    expect(isComplete(QUIZ.map(() => null))).toBe(false);
    const partial = QUIZ.map((_, i) => (i === 0 ? 0 : null));
    expect(isComplete(partial)).toBe(false);
    expect(isComplete(QUIZ.map(() => 0))).toBe(true);
  });
});

describe('buildQuizShare', () => {
  it('includes both classes, the brand, and the url', () => {
    const out = buildQuizShare(
      { primary: 'Wizard', secondary: 'Bard', ranking: [] },
      'https://x/which-class',
    );
    expect(out).toContain('Camalytics · Which Archetype Are You?');
    expect(out).toContain("I'm a Wizard / Bard.");
    expect(out).toContain('https://x/which-class');
  });
});
