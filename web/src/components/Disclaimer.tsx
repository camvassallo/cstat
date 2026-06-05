import type { ReactNode } from 'react';

/**
 * Reusable caveat/disclaimer box + a footer wrapper for parking methodology
 * notes at the BOTTOM of a page rather than dumping a wall of text up top.
 * Styling mirrors the inline amber/emerald boxes that used to live at the top
 * of the projections surfaces (rounded, tinted, xs text).
 */

type Tone = 'amber' | 'emerald' | 'slate';

const TONES: Record<Tone, { box: string; label: string }> = {
  amber: {
    box: 'border-amber-800/40 bg-amber-950/20 text-amber-200',
    label: 'text-amber-300',
  },
  emerald: {
    box: 'border-emerald-800/40 bg-emerald-950/20 text-emerald-200',
    label: 'text-emerald-300',
  },
  slate: {
    box: 'border-gray-700/60 bg-gray-800/30 text-gray-400',
    label: 'text-gray-300',
  },
};

export function Disclaimer({
  tone = 'amber',
  label,
  children,
  className = '',
}: {
  tone?: Tone;
  label?: string;
  children: ReactNode;
  className?: string;
}) {
  const t = TONES[tone];
  return (
    <div
      className={`rounded border ${t.box} text-xs p-3 leading-relaxed ${className}`}
    >
      {label && <strong className={t.label}>{label}</strong>}
      {label ? ' ' : ''}
      {children}
    </div>
  );
}

/**
 * Bottom-of-page container for one or more <Disclaimer> boxes. Adds a small
 * "Methodology & caveats" eyebrow and top spacing so the notes read as a
 * footer, not a banner.
 */
export function DisclaimerFooter({
  children,
  className = '',
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <footer className={`mt-6 space-y-2 ${className}`}>
      <div className="text-[10px] uppercase tracking-wide text-gray-600">
        Methodology &amp; caveats
      </div>
      {children}
    </footer>
  );
}
