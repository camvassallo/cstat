import { useRef, type KeyboardEvent, type ReactNode } from 'react';

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
// Note for those conversions: `text-xs` is baked into the base class because
// it is what every current caller wants, and `className` is placement-only —
// a caller passing `text-sm` would emit two competing size classes and let
// stylesheet order decide. Predict's venue picker is `text-sm`, so #301 has to
// add a real size variant rather than route it through `className`.
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
 * Radiogroup semantics follow the pattern the Predict page's venue picker
 * already uses: `role="radiogroup"` on the container, `role="radio"` +
 * `aria-checked` on each button. Predict aside, the other seven copies of this
 * markup are plain unlabelled buttons, so screen readers get a strictly better
 * announcement here — and none of it is visible on screen, which keeps the
 * Coaches conversion a pixel-for-pixel no-op.
 *
 * Declaring those roles takes on the radio pattern's KEYBOARD contract too,
 * which Predict's picker does not honor: a radiogroup is a single tab stop and
 * moves between options with the arrow keys. Announcing "radio group, 1 of 2"
 * and then ignoring Arrow Right is worse than plain buttons, so the contract is
 * implemented here rather than inherited half-done — this component is slated
 * to replace eight more controls (#301), so the pattern it establishes
 * propagates.
 */
export default function ModeToggle<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
  className = '',
}: Props<T>) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);

  // The checked option owns the group's tab stop; every other option is
  // reachable only by arrow key (roving tabindex). `findIndex` returning -1 —
  // a `value` outside `options` — would otherwise leave EVERY button at
  // tabIndex -1 and make the control unreachable from the keyboard, so fall
  // back to the first option. Nothing is visually selected in that state
  // either, which is the real bug; this just keeps it operable.
  const activeIndex = options.findIndex((o) => o.value === value);
  const tabStopIndex = activeIndex >= 0 ? activeIndex : 0;

  // Arrow/Home/End move the selection AND the focus together — for radios,
  // focus follows selection, so arrowing onto an option chooses it.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const last = options.length - 1;
    let next: number;
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        next = tabStopIndex === last ? 0 : tabStopIndex + 1;
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        next = tabStopIndex === 0 ? last : tabStopIndex - 1;
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = last;
        break;
      default:
        return;
    }
    e.preventDefault();
    onChange(options[next].value);
    buttons.current[next]?.focus();
  };

  return (
    <div
      className={`inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs ${className}`}
      role="radiogroup"
      aria-label={ariaLabel}
      onKeyDown={onKeyDown}
    >
      {options.map((o, i) => (
        <button
          key={o.value}
          ref={(el) => {
            buttons.current[i] = el;
          }}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          tabIndex={i === tabStopIndex ? 0 : -1}
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
