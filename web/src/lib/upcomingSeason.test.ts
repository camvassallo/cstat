import { describe, expect, it } from 'vitest';
import { resolveUpcomingSeason } from './upcomingSeason';

// The Future tab's target season (#386). Before this, the frontend computed
// it as `AVAILABLE_SEASONS_FALLBACK[0] + 1` — arithmetic on a hand-edited
// constant, which asserts a season exists rather than checking, and which
// needs editing every November. `/api/seasons` now serves it.
describe('resolveUpcomingSeason', () => {
  it('prefers what the API actually found projected', () => {
    // The normal case: 2027 has projections, nothing has been played in it.
    expect(resolveUpcomingSeason(2027, 2026, 2026)).toBe(2027);
  });

  it('trusts the API over the hardcoded array even when they disagree', () => {
    // The array is stale by a year (nobody bumped it in November); the API is
    // the one that knows. Getting this backwards is the drift being removed.
    expect(resolveUpcomingSeason(2028, 2027, 2026)).toBe(2028);
  });

  it('falls back to the API default when nothing is projected ahead', () => {
    // Legitimately null: the current season is under way and the nightly has
    // not yet written next season's projections. The season being played is
    // the newest one with a projection.
    expect(resolveUpcomingSeason(null, 2027, 2026)).toBe(2027);
  });

  it('never invents a season with no projection rows', () => {
    // The regression this replaced: `default + 1` named 2028 the night 2027
    // tipped off, and every picker that prepends `upcoming` offered an empty
    // 2028 whose projection endpoint 500'd.
    expect(resolveUpcomingSeason(null, 2027, 2026)).not.toBe(2028);
  });

  it('uses the hardcoded array only when the API has said nothing', () => {
    // First paint, and the API-unreachable case.
    expect(resolveUpcomingSeason(null, null, 2026)).toBe(2027);
    expect(resolveUpcomingSeason(undefined, undefined, 2026)).toBe(2027);
  });
});
