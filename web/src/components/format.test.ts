import { describe, expect, it } from 'vitest';
import { heightString } from './format';

describe('heightString', () => {
  it('formats whole inches as feet and inches', () => {
    expect(heightString(83)).toBe("6'11\"");
    expect(heightString(73)).toBe("6'1\"");
  });

  it('prints a clean foot boundary as 0 inches, not a bare foot mark', () => {
    expect(heightString(72)).toBe("6'0\"");
  });

  it('returns null for a missing height so callers can drop the field', () => {
    // Not an em dash or an empty string: the roster row omits the whole
    // ` · height` segment rather than printing a placeholder next to a real
    // minutes figure.
    expect(heightString(null)).toBeNull();
    expect(heightString(undefined)).toBeNull();
  });

  it('returns null rather than "NaN\'NaN" for a non-finite input', () => {
    expect(heightString(NaN)).toBeNull();
    expect(heightString(Infinity)).toBeNull();
  });

  it('carries a fractional height into the next foot instead of printing 5\'12"', () => {
    // Rounding the REMAINDER (what `portle`'s copy does) gives 5'12" here,
    // because the floor and the round disagree about which foot 71.6 is in.
    expect(heightString(71.6)).toBe("6'0\"");
    expect(heightString(71.4)).toBe("5'11\"");
  });
});
