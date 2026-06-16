import { useEffect, useRef, useState } from 'react';
import { CLASS_ORDER, classColor, classTagline } from './archetypeColors';

export type MatchMode = 'any' | 'all';

interface Props {
  /** Currently-selected archetype classes. */
  selected: Set<string>;
  /** Toggle one class in/out of the selection. */
  onToggle: (cls: string) => void;
  /**
   * 'any' = union (show players in ANY selected class).
   * 'all' = intersection (player must hold EVERY selected class across their
   * primary + secondary). Players have at most two classes, so 'all' only
   * matches with 1-2 selected.
   */
  matchMode: MatchMode;
  onSetMatchMode: (m: MatchMode) => void;
  /**
   * When true, an 'any'-mode player matches if EITHER their primary or
   * secondary class is selected. Ignored in 'all' mode (which always considers
   * both classes), so the control is hidden there.
   */
  includeSecondary: boolean;
  onToggleIncludeSecondary: () => void;
  /** Clear the whole selection (and the include-secondary flag). */
  onClear: () => void;
}

// A dropdown checklist of the 12 archetype classes. Selecting multiple classes
// filters the already-loaded player pool client-side, unioned ('any') or
// intersected ('all'). Mirrors the chrome of the View toggle next to it.
export default function ArchetypeFilter({
  selected,
  onToggle,
  matchMode,
  onSetMatchMode,
  includeSecondary,
  onToggleIncludeSecondary,
  onClear,
}: Props) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close on outside-click / Escape.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const count = selected.size;
  // 'all' mode can never match 3+ classes — nobody holds three. Flag it so the
  // user understands an empty grid rather than reading it as "no such players".
  const impossibleAll = matchMode === 'all' && count >= 3;

  return (
    <div className="relative" ref={ref}>
      <button
        onClick={() => setOpen((o) => !o)}
        className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md border text-xs ${
          count > 0
            ? 'border-blue-500 bg-blue-600/20 text-blue-200'
            : 'border-gray-700 bg-gray-800 text-gray-300 hover:bg-gray-700'
        }`}
      >
        <span>Archetypes</span>
        {count > 0 && (
          <span className="px-1.5 rounded-full bg-blue-600 text-white tabular-nums">{count}</span>
        )}
        <span className="text-gray-400">▾</span>
      </button>

      {open && (
        <div className="absolute z-20 mt-1 right-0 w-60 rounded-md border border-gray-700 bg-gray-900 shadow-lg py-1 text-xs">
          {/* Match mode — OR (union, 'any') vs AND (intersection, 'all'). */}
          <div className="flex items-center justify-between gap-2 px-3 py-1.5">
            <span className="text-gray-500">Match</span>
            <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden">
              <button
                onClick={() => onSetMatchMode('any')}
                title="Players in any selected class (union)"
                className={`px-2 py-0.5 ${
                  matchMode === 'any'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                }`}
              >
                OR
              </button>
              <button
                onClick={() => onSetMatchMode('all')}
                title="Players who hold every selected class (intersection)"
                className={`px-2 py-0.5 ${
                  matchMode === 'all'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                }`}
              >
                AND
              </button>
            </div>
          </div>
          <div className="border-t border-gray-800 my-1" />

          {CLASS_ORDER.map((cls) => {
            const on = selected.has(cls);
            return (
              <label
                key={cls}
                className="flex items-center gap-2 px-3 py-1.5 cursor-pointer hover:bg-gray-800"
                title={classTagline(cls)}
              >
                <input
                  type="checkbox"
                  checked={on}
                  onChange={() => onToggle(cls)}
                  className="rounded"
                />
                <span
                  className="inline-block w-2.5 h-2.5 rounded-full shrink-0"
                  style={{ backgroundColor: classColor(cls) }}
                />
                <span className={on ? 'text-gray-100 font-medium' : 'text-gray-300'}>{cls}</span>
              </label>
            );
          })}

          <div className="border-t border-gray-800 mt-1 pt-1 px-3 pb-1">
            {impossibleAll && (
              <p className="text-amber-400/90 py-1">
                Players hold at most two classes — “AND” matches nothing with 3+ selected.
              </p>
            )}
            {/* Include-secondary only applies to 'any' mode; 'all' always uses
                both classes, so hide it there to avoid implying it does anything. */}
            {matchMode === 'any' && (
              <label
                className={`flex items-center gap-2 py-1 ${
                  count > 0 ? 'cursor-pointer text-gray-300' : 'cursor-not-allowed text-gray-600'
                }`}
              >
                <input
                  type="checkbox"
                  checked={includeSecondary}
                  disabled={count === 0}
                  onChange={onToggleIncludeSecondary}
                  className="rounded"
                />
                Include secondary class
              </label>
            )}
            <button
              onClick={onClear}
              disabled={count === 0}
              className={`mt-1 w-full text-left ${
                count > 0 ? 'text-blue-400 hover:underline' : 'text-gray-600 cursor-not-allowed'
              }`}
            >
              Clear selection
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
