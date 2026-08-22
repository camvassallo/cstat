import type { ReactNode } from 'react';

// A segmented two-or-more-option control — the small pill of adjoining buttons
// with the active option filled blue. Lifted verbatim out of `Coaches.tsx`
// (career/season) so the cross-year surfaces don't each re-type it.
//
// Eight OTHER copies of this markup exist across the app (Rankings view,
// Players mode + view, TeamDetail roster view + lineup size, Lineups size,
// Predict venue, ArchetypeFilter match mode). They vary in padding, text size
// and inactive tone, so consolidating them is a deliberate follow-up (#301)
// rather than a drive-by here. New segmented controls should use this
// component.
//
// ---------------------------------------------------------------------------
// The cross-year contract (why this component exists now)
// ---------------------------------------------------------------------------
// Three surfaces — Predict, PlayerCompare, and PlayerDetail's comparable
// players — are gaining a single-season/cross-year mode. Two conventions are
// settled here so those three don't each invent their own:
//
// 1. LABELS. The cross-year control reads **"Any year | Season"**, defaulting
//    to `Season` so a user who never touches it sees today's behavior. It
//    parallels the Coaches board's "Career | Season" and avoids naming
//    anything internal ("cross-year" is how we talk about the work, not what
//    the reader is choosing between).
//
// 2. THE GLOBAL SEASON PICKER. In cross-year mode each slot carries its own
//    year, so the site-wide `?season=` selector in the navbar is ambiguous and
//    must be hidden. The mechanism already exists and is NOT specific to
//    detail pages: publish an EMPTY list via `setPageSeasons([])` from
//    `components/season.ts` and the selector renders nothing (`Layout.tsx`
//    returns null on a zero-length override). Release it with
//    `setPageSeasons(null)` when leaving cross-year mode AND in the page's
//    unmount cleanup — the override is module state, so a page that forgets
//    leaves the navbar picker hidden on whatever the user navigates to next.
//    `Coaches.tsx` is the reference implementation for both halves.

export type ModeToggleOption<T extends string> = {
  value: T;
  /** What the button reads. Pass display-ready text — this component applies
   *  no casing transform. */
  label: ReactNode;
  /** Optional native tooltip, for options whose label is too terse to stand
   *  on its own. */
  title?: string;
};

type Props<T extends string> = {
  options: readonly ModeToggleOption<T>[];
  value: T;
  onChange: (value: T) => void;
  /** Names the group for screen readers. Required — an unlabelled radiogroup
   *  announces as a bare set of choices with no indication of what is being
   *  chosen. */
  ariaLabel: string;
  /** Extra classes on the container, for placement only (`self-start
   *  shrink-0`, `mb-3`, …). Not a styling escape hatch — if a caller needs a
   *  different size or tone, add a variant here instead so the surfaces stay
   *  consistent. */
  className?: string;
};

/**
 * Radiogroup semantics follow the pattern already used by the Predict page's
 * venue picker: `role="radiogroup"` on the container, `role="radio"` +
 * `aria-checked` on each button. The seven other copies of this markup are
 * plain unlabelled buttons, so screen readers get a strictly better
 * announcement here — invisible on screen, which keeps the Coaches conversion
 * a pixel-for-pixel no-op.
 */
export default function ModeToggle<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  className = '',
}: Props<T>) {
  return (
    <div
      className={`inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs ${className}`}
      role="radiogroup"
      aria-label={ariaLabel}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          title={o.title}
          onClick={() => onChange(o.value)}
          className={`px-3 py-1.5 ${
            value === o.value
              ? 'bg-blue-600 text-white'
              : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
