import { describe, it, expect } from 'vitest';
import { recruitTooltipLine } from './recruitDisplay';

describe('recruitTooltipLine', () => {
  it('formats a ranked, starred recruit', () => {
    expect(
      recruitTooltipLine({ name: 'Cameron Boozer', composite_rank: 3, star_rating: 5 }),
    ).toBe('#3 Cameron Boozer (5★)');
  });

  it('omits the rank when unranked and shows ? for a missing star rating', () => {
    expect(
      recruitTooltipLine({ name: 'Walk On', composite_rank: null, star_rating: null }),
    ).toBe('Walk On (?★)');
  });

  it('appends the redshirt marker only when did_not_play is true', () => {
    expect(
      recruitTooltipLine({
        name: 'Sebastian Wilkins',
        composite_rank: 35,
        star_rating: 4,
        did_not_play: true,
      }),
    ).toBe('#35 Sebastian Wilkins (4★) — redshirt (did not play)');
  });

  it('has no marker when did_not_play is false or absent (the live-projection case)', () => {
    expect(
      recruitTooltipLine({
        name: 'Dame Sarr',
        composite_rank: 32,
        star_rating: 4,
        did_not_play: false,
      }),
    ).toBe('#32 Dame Sarr (4★)');
    expect(
      recruitTooltipLine({ name: 'Dame Sarr', composite_rank: 32, star_rating: 4 }),
    ).toBe('#32 Dame Sarr (4★)');
  });
});
