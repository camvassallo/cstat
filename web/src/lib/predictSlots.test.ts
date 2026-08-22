import { describe, expect, it } from 'vitest';
import {
  matchupKey,
  parseSlotSeasonParam,
  slotYears,
  teamLabel,
  toRequest,
  type Matchup,
} from './predictSlots';

const base: Matchup = {
  home: 'Duke Blue Devils',
  away: 'Kentucky Wildcats',
  venue: 'home',
  homeSeason: 2026,
  awaySeason: 2026,
  asOfDate: '',
  crossYear: false,
};

describe('parseSlotSeasonParam', () => {
  it('reads a plausible year', () => {
    expect(parseSlotSeasonParam('2015')).toBe(2015);
  });

  it('reads an absent param as unpinned', () => {
    expect(parseSlotSeasonParam(null)).toBeNull();
    expect(parseSlotSeasonParam('')).toBeNull();
  });

  it('rejects out-of-range and non-integer years rather than passing them on', () => {
    // The backend builds a chrono date out of `home_season`; anything wild is
    // better dropped here than argued about there.
    expect(parseSlotSeasonParam('1999')).toBeNull();
    expect(parseSlotSeasonParam('300000')).toBeNull();
    expect(parseSlotSeasonParam('2015.5')).toBeNull();
    expect(parseSlotSeasonParam('twenty fifteen')).toBeNull();
  });

  it('accepts a year the static fallback list predates', () => {
    // A shared link must survive a cold render, before /api/seasons answers.
    expect(parseSlotSeasonParam('2012')).toBe(2012);
  });
});

describe('toRequest', () => {
  it('sends the legacy single-season shape when the mode is off', () => {
    expect(toRequest({ ...base, asOfDate: '2026-01-15' })).toEqual({
      home: 'Duke Blue Devils',
      away: 'Kentucky Wildcats',
      venue: 'home',
      season: 2026,
      asOfDate: '2026-01-15',
    });
  });

  it('omits an empty as-of date rather than sending a blank param', () => {
    expect(toRequest(base).asOfDate).toBeUndefined();
  });

  it('names both years cross-year, and no shared season', () => {
    expect(
      toRequest({ ...base, crossYear: true, homeSeason: 2015, awaySeason: 2026 }),
    ).toEqual({
      home: 'Duke Blue Devils',
      away: 'Kentucky Wildcats',
      venue: 'home',
      homeSeason: 2015,
      awaySeason: 2026,
    });
  });

  it('drops as_of_date cross-year — the backend 400s on the combination', () => {
    const r = toRequest({
      ...base,
      crossYear: true,
      homeSeason: 2015,
      awaySeason: 2026,
      asOfDate: '2026-01-15',
    });
    expect(r.asOfDate).toBeUndefined();
    expect(r.season).toBeUndefined();
  });

  it('still sends both years when a cross-year matchup lands on one season', () => {
    // 2015 Duke vs 2015 Duke is a legal thing to sit on while editing the
    // years; the request must not collapse back to the single-season shape and
    // change what the page is asking for.
    const r = toRequest({ ...base, crossYear: true });
    expect(r).toMatchObject({ homeSeason: 2026, awaySeason: 2026 });
    expect(r.season).toBeUndefined();
  });
});

describe('matchupKey', () => {
  it('separates matchups that differ in any sent field', () => {
    const keys = new Set([
      matchupKey(base),
      matchupKey({ ...base, home: 'Duke' }),
      matchupKey({ ...base, away: 'Duke' }),
      matchupKey({ ...base, venue: 'neutral' }),
      matchupKey({ ...base, asOfDate: '2026-01-15' }),
      matchupKey({ ...base, crossYear: true, homeSeason: 2015 }),
    ]);
    expect(keys.size).toBe(6);
  });

  it('ignores as-of date cross-year, where it is never sent', () => {
    const m: Matchup = { ...base, crossYear: true, homeSeason: 2015 };
    expect(matchupKey({ ...m, asOfDate: '2026-01-15' })).toBe(matchupKey(m));
  });

  it('separates the two modes even on one season', () => {
    // Same two years, but different requests on the wire. Their responses do
    // coincide today, so a shared key would be a dedupe that is right only by
    // accident — and would have to be re-argued on any change to either side.
    expect(matchupKey({ ...base, crossYear: true })).not.toBe(matchupKey(base));
  });
});

describe('slotYears / teamLabel', () => {
  it('prints no year while the slots agree', () => {
    const y = slotYears(base);
    expect(y.show).toBe(false);
    expect(teamLabel('Duke Blue Devils', y.home, y.show)).toBe('Duke Blue Devils');
  });

  it('prints the year on both sides as soon as they differ', () => {
    const y = slotYears({ ...base, crossYear: true, homeSeason: 2015 });
    expect(y).toEqual({ home: 2015, away: 2026, show: true });
    expect(teamLabel('Duke Blue Devils', y.home, y.show)).toBe('2015 Duke Blue Devils');
    expect(teamLabel('Duke Blue Devils', y.away, y.show)).toBe('2026 Duke Blue Devils');
  });

  it('keeps Duke vs Duke distinguishable', () => {
    const y = slotYears({
      ...base,
      away: base.home,
      crossYear: true,
      homeSeason: 2015,
    });
    expect(teamLabel(base.home, y.home, y.show)).not.toBe(
      teamLabel(base.home, y.away, y.show),
    );
  });

  it('prints no year for a cross-year matchup that sits on one season', () => {
    // The years are editable but identical: suffixing both with the same year
    // would be noise, and the header would read as a typo the other way round.
    expect(slotYears({ ...base, crossYear: true }).show).toBe(false);
  });
});
