import { describe, expect, it } from 'vitest';
import {
  clearSlotSeasons,
  idsHaveSlotSeasons,
  parseCompareIds,
  parseSlotToken,
  pinSlotSeasons,
  setSlotSeason,
  slotToken,
} from './compareSlots';

const A = '11111111-1111-1111-1111-111111111111';
const B = '22222222-2222-2222-2222-222222222222';

describe('parseSlotToken', () => {
  it('reads a bare uuid as unpinned', () => {
    expect(parseSlotToken(A)).toEqual({ raw: A, id: A, season: null });
  });

  it('reads a pinned slot', () => {
    expect(parseSlotToken(`${A}@2015`)).toEqual({ raw: `${A}@2015`, id: A, season: 2015 });
  });

  it('tolerates whitespace around both halves', () => {
    expect(parseSlotToken(` ${A} @ 2015 `)).toEqual({
      raw: `${A} @ 2015`,
      id: A,
      season: 2015,
    });
  });

  // A suffix the server would 400 on is treated as unpinned rather than
  // trusted — the page renders the slot in the request season instead of
  // asking for `@banana`.
  it('treats an unparseable season as unpinned', () => {
    expect(parseSlotToken(`${A}@banana`).season).toBeNull();
    expect(parseSlotToken(`${A}@1200`).season).toBeNull();
  });
});

describe('parseCompareIds', () => {
  it('drops empty entries', () => {
    expect(parseCompareIds(` ${A} , ,${B}`)).toEqual([A, B]);
  });

  it('returns nothing for an empty param', () => {
    expect(parseCompareIds('')).toEqual([]);
  });
});

describe('idsHaveSlotSeasons', () => {
  // This is how cross-year mode round-trips: no second query param, just the
  // shape of the tokens themselves.
  it('is false for a single-season link', () => {
    expect(idsHaveSlotSeasons([A, B])).toBe(false);
  });

  it('is true as soon as one slot is pinned', () => {
    expect(idsHaveSlotSeasons([A, `${B}@2015`])).toBe(true);
  });
});

describe('pinSlotSeasons', () => {
  it('pins the unpinned and leaves the rest alone', () => {
    expect(pinSlotSeasons([A, `${B}@2015`], 2026)).toEqual([`${A}@2026`, `${B}@2015`]);
  });

  it('is a no-op on an already-pinned list', () => {
    const pinned = [`${A}@2024`, `${B}@2015`];
    expect(pinSlotSeasons(pinned, 2026)).toEqual(pinned);
  });
});

describe('clearSlotSeasons', () => {
  it('strips every year', () => {
    expect(clearSlotSeasons([`${A}@2024`, `${B}@2015`])).toEqual([A, B]);
  });

  // Flagg 2025 vs Flagg 2026 is a first-class cross-year comparison, and it
  // has no meaning at all once there is one season — so it must collapse to a
  // single column rather than ask the server for the same player twice.
  it('collapses the same player in two years onto one slot', () => {
    expect(clearSlotSeasons([`${A}@2025`, `${A}@2026`, `${B}@2025`])).toEqual([A, B]);
  });
});

describe('setSlotSeason', () => {
  it('repoints one slot and leaves its neighbours untouched', () => {
    expect(setSlotSeason([`${A}@2026`, `${B}@2026`], 1, 2015)).toEqual([
      `${A}@2026`,
      `${B}@2015`,
    ]);
  });

  it('pins a slot that had no year', () => {
    expect(setSlotSeason([A, B], 0, 2019)).toEqual([`${A}@2019`, B]);
  });
});

describe('slotToken', () => {
  it('omits the suffix when unpinned', () => {
    expect(slotToken(A, null)).toBe(A);
    expect(slotToken(A, 2015)).toBe(`${A}@2015`);
  });
});
