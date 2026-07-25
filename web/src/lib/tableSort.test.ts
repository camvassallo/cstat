import { describe, it, expect } from 'vitest';
import { agNullsBottom } from '../components/tableSort';

// Regression guard for issue #196: AG Grid negates a comparator's result on a
// descending sort, so a naive nulls-last comparator (+1 for null) floats blank
// (—) rows to the TOP exactly when sorting best-to-worst. `agNullsBottom` takes
// AG Grid's isDescending flag and pre-inverts so blanks stay at the BOTTOM in
// both directions. These tests assert the post-negation order the user sees.
describe('agNullsBottom', () => {
  // AG Grid applies the comparator, then negates the sign on descending.
  const sortWith = (values: (number | null)[], isDescending: boolean) =>
    [...values].sort((a, b) => {
      const r = agNullsBottom(a, b, undefined, undefined, isDescending);
      return isDescending ? -r : r;
    });

  it('keeps nulls at the bottom on ascending sort', () => {
    expect(sortWith([3, null, 1, null, 2], false)).toEqual([1, 2, 3, null, null]);
  });

  it('keeps nulls at the bottom on descending sort (the #196 case)', () => {
    // Naive nulls-last would surface the nulls first here; agNullsBottom must not.
    expect(sortWith([3, null, 1, null, 2], true)).toEqual([3, 2, 1, null, null]);
  });

  it('orders real values correctly and handles negatives (Δ247 sleepers/favorites)', () => {
    expect(sortWith([-4, 7, null, 0], false)).toEqual([-4, 0, 7, null]);
    expect(sortWith([-4, 7, null, 0], true)).toEqual([7, 0, -4, null]);
  });

  it('treats two nulls as equal in both directions', () => {
    expect(agNullsBottom(null, null, undefined, undefined, false)).toBe(0);
    expect(agNullsBottom(null, null, undefined, undefined, true)).toBe(0);
  });
});
