import { describe, expect, it } from 'vitest';
import { availabilityOf, type TransferAvailabilityFields } from './transferAvailability';

const row = (over: Partial<TransferAvailabilityFields> = {}): TransferAvailabilityFields => ({
  status: 'Entered',
  next_team: null,
  ...over,
});

describe('availabilityOf', () => {
  it('treats a plain portal entry with no destination as available', () => {
    expect(availabilityOf(row())).toBe('available');
  });

  it('treats a committed row as committed', () => {
    expect(availabilityOf(row({ status: 'Committed', next_team: 'Kansas' }))).toBe('committed');
  });

  // The two reconciliation cases are the whole reason this is an OR rather than
  // a single field. Both shapes exist in the live data.
  it('counts a destination as committed even when 247 still says Entered', () => {
    // 2026: 5 rows like this. Filing them under "Available" would caption a
    // visible destination as "still on the board".
    expect(availabilityOf(row({ status: 'Entered', next_team: 'Duke' }))).toBe('committed');
  });

  it('counts a Committed row as committed even with no destination yet', () => {
    // 2025: 3 rows like this. The "Next" column shows TBD, but 247 has called
    // it, so the chip should not offer him as available.
    expect(availabilityOf(row({ status: 'Committed', next_team: null }))).toBe('committed');
  });

  it('partitions any row set exactly, so chip counts sum to the total', () => {
    const rows = [
      row(),
      row({ status: 'Committed', next_team: 'Kansas' }),
      row({ status: 'Entered', next_team: 'Duke' }),
      row({ status: 'Committed', next_team: null }),
      row({ status: 'Entered' }),
    ];
    const committed = rows.filter((r) => availabilityOf(r) === 'committed').length;
    const available = rows.filter((r) => availabilityOf(r) === 'available').length;
    expect(committed + available).toBe(rows.length);
    expect(committed).toBe(3);
    expect(available).toBe(2);
  });
});
