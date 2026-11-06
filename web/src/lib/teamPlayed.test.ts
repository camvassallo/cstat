import { describe, expect, it } from 'vitest';
import { hasResult, teamHasPlayed } from './teamPlayed';

const final = { team_score: 97, opponent_score: 66 };
const upcoming = { team_score: null, opponent_score: null };

describe('hasResult', () => {
  it('needs both scores', () => {
    expect(hasResult(final)).toBe(true);
    expect(hasResult(upcoming)).toBe(false);
    expect(hasResult({ team_score: 97, opponent_score: null })).toBe(false);
    expect(hasResult({ team_score: null, opponent_score: 66 })).toBe(false);
  });

  it('counts a shutout score of zero as a score', () => {
    expect(hasResult({ team_score: 0, opponent_score: 66 })).toBe(true);
  });
});

describe('teamHasPlayed', () => {
  it('is true once any game on the schedule is final', () => {
    expect(teamHasPlayed([upcoming, final, upcoming])).toBe(true);
  });

  it('is false for a schedule of unplayed games, or none', () => {
    expect(teamHasPlayed([upcoming, upcoming])).toBe(false);
    expect(teamHasPlayed([])).toBe(false);
  });
});
