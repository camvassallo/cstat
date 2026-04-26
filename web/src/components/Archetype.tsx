import type { ReactNode } from 'react';
import { Link } from 'react-router-dom';
import type { PlayerArchetype, SimilarPlayer } from '../api/client';
import { classColor, classTagline, classTitle } from './archetypeColors';

/// Styled hover tooltip for any class label — mirrors the look of the
/// affinity popover on `ArchetypeBadge`. Wrap a chip / span with this and
/// the tooltip pops below the trigger on hover. Pass `extra` to append a
/// secondary line (e.g. "27.6% of minutes · 2 players").
export function ClassTooltip({
  cls,
  children,
  extra,
  asBlock = false,
}: {
  cls: string;
  children: ReactNode;
  extra?: ReactNode;
  /// Render the wrapper as a block-level element. Use this when the trigger
  /// is itself a block (e.g. flex bar segments) so layout isn't disrupted.
  asBlock?: boolean;
}) {
  const color = classColor(cls);
  const tagline = classTagline(cls);
  const Wrap = asBlock ? 'div' : 'span';
  return (
    <Wrap className={`relative group ${asBlock ? 'block h-full' : 'inline-block'}`}>
      <Wrap className="cursor-help block h-full">{children}</Wrap>
      <span
        className="absolute left-1/2 -translate-x-1/2 top-full mt-2 z-20 w-48 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-opacity pointer-events-none text-left whitespace-normal"
      >
        <span className="block text-xs font-bold" style={{ color }}>
          {cls}
        </span>
        {tagline && (
          <span className="block text-[11px] text-gray-300 mt-0.5 normal-case font-normal tracking-normal">
            {tagline}
          </span>
        )}
        {extra && (
          <span className="block text-[10px] text-gray-400 mt-1 normal-case font-normal tracking-normal">
            {extra}
          </span>
        )}
      </span>
    </Wrap>
  );
}

export function ArchetypeBadge({
  archetype,
  size = 'md',
}: {
  archetype: PlayerArchetype;
  size?: 'sm' | 'md';
}) {
  const primaryColor = classColor(archetype.primary_class);
  const primaryTagline = classTagline(archetype.primary_class);
  const ranked = Object.entries(archetype.affinity_scores)
    .sort((a, b) => b[1] - a[1]);

  // Compact pill + hover popover with full affinity ranking.
  const sizing =
    size === 'sm'
      ? 'text-[10px] px-2 py-0.5'
      : 'text-xs px-2.5 py-1';

  const titleStr = archetype.secondary_class
    ? `${classTitle(archetype.primary_class)} / ${classTitle(archetype.secondary_class)}`
    : classTitle(archetype.primary_class);

  return (
    <div className="relative group inline-block">
      <span
        className={`inline-flex items-center gap-1.5 ${sizing} rounded-full font-bold uppercase tracking-wide ring-1 cursor-help`}
        style={{
          background: primaryColor + '22',
          color: primaryColor,
          // ring color via inline style (Tailwind ring-color uses DEFAULT)
          boxShadow: `inset 0 0 0 1px ${primaryColor}66`,
        }}
        title={titleStr}
      >
        <span
          className="inline-block w-1.5 h-1.5 rounded-full"
          style={{ background: primaryColor }}
        />
        {archetype.primary_class}
        {archetype.secondary_class && (
          <span
            className="font-normal opacity-75"
            style={{ color: classColor(archetype.secondary_class) }}
          >
            / {archetype.secondary_class}
          </span>
        )}
      </span>
      <div
        className="absolute left-0 top-full mt-2 z-20 w-64 bg-gray-900 border border-gray-700 rounded-lg shadow-xl p-3 opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-opacity pointer-events-none"
      >
        {primaryTagline && (
          <div className="text-xs font-bold mb-1" style={{ color: primaryColor }}>
            {archetype.primary_class}
            <span className="font-normal text-gray-400"> — {primaryTagline}</span>
          </div>
        )}
        <div className="text-[10px] font-bold text-gray-500 mb-2 uppercase tracking-wider">
          Class Affinity
        </div>
        <div className="space-y-1">
          {ranked.map(([cls, score]) => {
            const pct = Math.max(0, Math.min(1, score));
            const c = classColor(cls);
            return (
              <div key={cls} className="flex items-center gap-2 text-xs">
                <div className="w-16 truncate" style={{ color: c }}>
                  {cls}
                </div>
                <div className="flex-1 bg-gray-800 rounded h-1.5 overflow-hidden">
                  <div
                    className="h-1.5"
                    style={{ width: `${pct * 100}%`, background: c }}
                  />
                </div>
                <div className="w-9 text-right text-gray-400">
                  {(score * 100).toFixed(0)}%
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

export function SimilarPlayers({
  players,
  title = 'Most Similar Players',
}: {
  players: SimilarPlayer[];
  title?: string;
}) {
  if (players.length === 0) return null;
  return (
    <div className="bg-gray-800 rounded-lg p-5">
      <h2 className="text-lg font-bold mb-1">{title}</h2>
      <p className="text-xs text-gray-500 mb-3">
        Closest in standardized feature space (rate stats, shot diet, impact, minutes share).
      </p>
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-3">
        {players.map((p) => {
          const c = classColor(p.primary_class);
          const simPct = Math.round(p.similarity * 100);
          return (
            <Link
              key={p.player_id}
              to={`/players/${p.player_id}`}
              className="bg-gray-900 hover:bg-gray-700/60 rounded p-3 transition-colors border-l-4 block"
              style={{ borderLeftColor: c }}
            >
              <div className="font-medium text-sm truncate">{p.name}</div>
              <div className="text-xs text-gray-400 truncate">
                {p.team_name ?? '—'}
              </div>
              <div className="flex items-center gap-2 mt-2">
                <ClassTooltip cls={p.primary_class}>
                  <span
                    className="text-[10px] font-bold uppercase tracking-wide"
                    style={{ color: c }}
                  >
                    {p.primary_class}
                  </span>
                </ClassTooltip>
                {p.secondary_class && (
                  <ClassTooltip cls={p.secondary_class}>
                    <span
                      className="text-[10px] opacity-70"
                      style={{ color: classColor(p.secondary_class) }}
                    >
                      / {p.secondary_class}
                    </span>
                  </ClassTooltip>
                )}
              </div>
              <div className="mt-2 flex items-center gap-2">
                <div className="flex-1 h-1 bg-gray-700 rounded overflow-hidden">
                  <div
                    className="h-1"
                    style={{ width: `${simPct}%`, background: c }}
                  />
                </div>
                <span className="text-[10px] text-gray-500">
                  {simPct}%
                </span>
              </div>
            </Link>
          );
        })}
      </div>
    </div>
  );
}
