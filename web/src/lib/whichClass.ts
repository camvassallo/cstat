// "Which Class Are You?" — a personality quiz that maps answers onto the 12
// D&D archetype classes. Pure data + scoring so it's unit-testable; the page
// (pages/WhichClass.tsx) owns rendering, exemplar fetch, and share.
//
// Each option carries weights toward one or more classes; the result is the
// argmax (primary) + runner-up (secondary), tie-broken by CLASS_ORDER for
// determinism. Weights are tuned so every class is reachable.

import { CLASS_ORDER } from '../components/archetypeColors';

export interface QuizOption {
  label: string;
  /** Class-name → points. Classes omitted score 0 for this option. */
  weights: Record<string, number>;
}

export interface QuizQuestion {
  id: string;
  prompt: string;
  options: QuizOption[];
}

export const QUIZ: QuizQuestion[] = [
  {
    id: 'clutch',
    prompt: 'Shot clock winding down, your team needs a bucket. You…',
    options: [
      { label: 'Create off the dribble and rise into my spot', weights: { Sorcerer: 2, Wizard: 1 } },
      { label: 'Relocate behind the arc and let it fly', weights: { Warlock: 2, Ranger: 1 } },
      { label: 'Attack the rim — contact be damned', weights: { Barbarian: 2, Cleric: 1 } },
      { label: 'Draw two defenders and find the open teammate', weights: { Bard: 2, Wizard: 1 } },
    ],
  },
  {
    id: 'hype',
    prompt: 'What gets you most hyped?',
    options: [
      { label: 'A deep pull-up three in someone’s eye', weights: { Sorcerer: 1, Warlock: 2 } },
      { label: 'A chase-down block off the backboard', weights: { Paladin: 2, Rogue: 1 } },
      { label: 'A no-look dime in transition', weights: { Bard: 2, Wizard: 1 } },
      { label: 'A two-handed putback slam', weights: { Barbarian: 2, Druid: 1 } },
    ],
  },
  {
    id: 'role',
    prompt: 'Your role on the floor is best described as…',
    options: [
      { label: 'The engine — the ball is in my hands', weights: { Wizard: 2, Sorcerer: 1 } },
      { label: 'The floor-spacer who keeps defenses honest', weights: { Ranger: 2, Warlock: 1 } },
      { label: 'The anchor who owns the paint', weights: { Paladin: 2, Druid: 1 } },
      { label: 'The glue guy who does a little of everything', weights: { Fighter: 2, Cleric: 1 } },
    ],
  },
  {
    id: 'defense',
    prompt: 'Pick your defensive identity:',
    options: [
      { label: 'Ball-hawk, living in the passing lanes', weights: { Rogue: 2, Ranger: 1 } },
      { label: 'Rim protector — nothing at the basket', weights: { Paladin: 2, Druid: 1 } },
      { label: 'Switchable, I’ll guard 1 through 5', weights: { Monk: 2, Rogue: 1 } },
      { label: 'Positionally sound, no gambles', weights: { Fighter: 2, Cleric: 1 } },
    ],
  },
  {
    id: 'vibe',
    prompt: 'Off the court, your vibe is…',
    options: [
      { label: 'Vocal leader — I run the show', weights: { Wizard: 1, Bard: 1 } },
      { label: 'Quiet assassin, let the game talk', weights: { Monk: 2, Sorcerer: 1 } },
      { label: 'Bruiser who does the dirty work', weights: { Barbarian: 2, Fighter: 1 } },
      { label: 'Steady and dependable', weights: { Cleric: 2, Fighter: 1 } },
    ],
  },
  {
    id: 'diet',
    prompt: 'Your ideal shot diet is…',
    options: [
      { label: 'Threes, threes, and more threes', weights: { Warlock: 2, Ranger: 1 } },
      { label: 'Rim runs and free throws', weights: { Barbarian: 2, Druid: 1 } },
      { label: 'A balanced, versatile mix', weights: { Monk: 2, Fighter: 1 } },
      { label: 'Whatever I create off the bounce', weights: { Sorcerer: 2, Wizard: 1 } },
    ],
  },
  {
    id: 'statline',
    prompt: 'The stat you’d lead your team in:',
    options: [
      { label: 'Assists', weights: { Bard: 2, Wizard: 1 } },
      { label: 'Points', weights: { Sorcerer: 2, Monk: 1 } },
      { label: 'Blocks', weights: { Paladin: 2, Druid: 1 } },
      { label: 'Steals', weights: { Rogue: 2, Ranger: 1 } },
    ],
  },
];

export interface ClassScore {
  className: string;
  score: number;
}

/** Sum the selected options' weights into a per-class score, returned sorted
 *  high→low. `answers[i]` is the chosen option index for question i, or null if
 *  unanswered. Ties broken by CLASS_ORDER so the result is deterministic. */
export function scoreQuiz(answers: Array<number | null>): ClassScore[] {
  const totals: Record<string, number> = {};
  for (const c of CLASS_ORDER) totals[c] = 0;
  QUIZ.forEach((q, i) => {
    const sel = answers[i];
    if (sel == null) return;
    const opt = q.options[sel];
    if (!opt) return;
    for (const [cls, w] of Object.entries(opt.weights)) {
      totals[cls] = (totals[cls] ?? 0) + w;
    }
  });
  return CLASS_ORDER.map((c) => ({ className: c, score: totals[c] })).sort(
    (a, b) =>
      b.score - a.score ||
      CLASS_ORDER.indexOf(a.className) - CLASS_ORDER.indexOf(b.className),
  );
}

export function isComplete(answers: Array<number | null>): boolean {
  return answers.length === QUIZ.length && answers.every((a) => a != null);
}

export interface QuizResult {
  primary: string;
  secondary: string;
  ranking: ClassScore[];
}

export function quizResult(answers: Array<number | null>): QuizResult {
  const ranking = scoreQuiz(answers);
  return { primary: ranking[0].className, secondary: ranking[1].className, ranking };
}

/** Shareable one-liner (no spoilers to spoil — it's the user's own result). */
export function buildQuizShare(result: QuizResult, url?: string): string {
  const line = `CamPom · Which Archetype Are You? — I'm a ${result.primary} / ${result.secondary}.`;
  return url ? `${line}\n${url}` : line;
}
