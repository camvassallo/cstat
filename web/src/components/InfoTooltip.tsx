import { useState, type ReactNode } from 'react';
import { useDismissOnOutside } from './useDismissOnOutside';

/// Generic hover/tap-toggle tooltip. Wraps any trigger element; opens on
/// hover (pointer-fine) or tap (touch), dismisses on outside click or
/// Escape. The trigger keeps its own click semantics — the tooltip
/// stop-propagates its own clicks so the wrapper toggle doesn't immediately
/// flip back closed.
export function InfoTooltip({
  title,
  body,
  children,
  width = 'w-64',
}: {
  title: string;
  body: ReactNode;
  children: ReactNode;
  /// Tailwind width class for the popover; override for wider explanations.
  width?: string;
}) {
  const [open, setOpen] = useState(false);
  const ref = useDismissOnOutside(open, () => setOpen(false));
  const setRef = (node: HTMLElement | null) => {
    ref.current = node;
  };

  return (
    <span
      ref={setRef}
      className="relative inline-block"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <span
        className="cursor-help"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        {children}
      </span>
      <span
        className={`absolute left-1/2 -translate-x-1/2 top-full mt-2 z-30 ${width} bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 transition-opacity text-left whitespace-normal ${
          open ? 'opacity-100 visible' : 'opacity-0 invisible pointer-events-none'
        }`}
      >
        <span className="block text-xs font-bold text-gray-100">{title}</span>
        <span className="block text-[11px] text-gray-300 mt-1 leading-snug font-normal normal-case tracking-normal">
          {body}
        </span>
      </span>
    </span>
  );
}

/// Small "?" icon for trigger surfaces. Inherits color from its parent.
export function InfoIcon({ className = '' }: { className?: string }) {
  return (
    <span
      className={`inline-flex items-center justify-center w-3.5 h-3.5 rounded-full bg-gray-700 text-gray-300 text-[9px] font-bold leading-none align-middle ${className}`}
      aria-hidden="true"
    >
      ?
    </span>
  );
}
