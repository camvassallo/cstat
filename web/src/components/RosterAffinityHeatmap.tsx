import { useMemo, useState } from 'react';
import type { RosterEntry } from '../api/client';
import { CLASS_ORDER, classColor, classTagline } from './archetypeColors';
import { useDismissOnOutside } from './useDismissOnOutside';

/// Heatmap of each player's full 12-class archetype affinity vector.
/// Rows are rostered players sorted by minutes played descending (most
/// load-bearing players at the top). Columns are the 12 archetype
/// classes in `CLASS_ORDER` — the same order used by the radial roster
/// plot, so the two views read the same way around the dial.
///
/// Each cell is tinted with the column's archetype color, with opacity
/// scaled to the affinity score. Soft-max-normalized scores typically
/// peak around 0.4-0.7 for pure-class players and around 0.15-0.20 for
/// the most balanced hybrids; we use a fixed 0.6 reference for full
/// opacity rather than the per-roster max so brightness reads
/// consistently across teams.
///
/// Reveals what the radial can't: hybrid players who don't show up
/// purely in their primary class (their cells light up across multiple
/// columns), and "second-archetype" depth for the team as a whole
/// (e.g., a roster that's two-third Wizard primaries with secondary
/// Bard affinity is meaningfully different from one that's split
/// Wizard/Sorcerer with no Bard).

interface HoveredCell {
  player: RosterEntry;
  className: string;
  score: number;
}

interface Props {
  roster: RosterEntry[];
  /// Score that maps to full opacity. 0.6 by default — pure-class
  /// players sit around 0.4-0.7 in this scoring (softmax over 12
  /// classes; sum = 1.0). Tuned so most cells stay in a readable
  /// brightness range rather than washing out at the dim end.
  fullOpacityAt?: number;
}

const MIN_OPACITY = 0.06;

function opacityFromScore(score: number, fullAt: number): number {
  const clamped = Math.max(0, Math.min(1, score / fullAt));
  /// Mild gamma so mid-range hybrids are visible without elite-affinity
  /// cells looking identical to pure-class peaks.
  return MIN_OPACITY + (1 - MIN_OPACITY) * Math.pow(clamped, 0.85);
}

export function RosterAffinityHeatmap({ roster, fullOpacityAt = 0.6 }: Props) {
  const [hovered, setHovered] = useState<HoveredCell | null>(null);
  const popoverRef = useDismissOnOutside(hovered !== null, () =>
    setHovered(null),
  );

  /// Drop players without an affinity vector. The radial surfaces them
  /// in its "unclassified" caption — repeating that here would be
  /// noise, since their cells would all be blank.
  const rows = useMemo(() => {
    return roster
      .filter((p) => p.affinity_scores != null)
      .slice()
      .sort((a, b) => {
        const aMin = (a.minutes_per_game ?? 0) * a.games_played;
        const bMin = (b.minutes_per_game ?? 0) * b.games_played;
        return bMin - aMin;
      });
  }, [roster]);

  if (rows.length === 0) return null;

  const popover = hovered ? (
    <div
      className="pointer-events-none fixed z-30 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 text-left max-w-[14rem]"
      style={{
        left: '50%',
        top: '20%',
        transform: 'translateX(-50%)',
      }}
    >
      <div className="text-xs font-bold text-gray-100 truncate">
        {hovered.player.name}
      </div>
      <div className="text-[11px] mt-0.5">
        <span style={{ color: classColor(hovered.className) }}>
          {hovered.className}
        </span>
        <span className="text-gray-400">
          {' '}— {classTagline(hovered.className)}
        </span>
      </div>
      <div className="text-[10px] text-gray-500 mt-1 tabular-nums">
        Affinity {(hovered.score * 100).toFixed(1)}%
      </div>
    </div>
  ) : null;

  return (
    <div
      className="relative"
      ref={(node) => {
        popoverRef.current = node;
      }}
    >
      <div className="overflow-x-auto">
        <table className="text-xs border-separate" style={{ borderSpacing: 0 }}>
          <thead>
            <tr>
              <th
                className="text-left text-[10px] uppercase tracking-wider text-gray-500 font-semibold pb-2 pr-3 sticky left-0 bg-gray-800 z-10"
                style={{ minWidth: '11rem' }}
              >
                Player
              </th>
              <th className="text-right text-[10px] uppercase tracking-wider text-gray-500 font-semibold pb-2 pr-3">
                MIN%
              </th>
              {CLASS_ORDER.map((cls) => (
                <th
                  key={cls}
                  className="pb-2 px-0.5 text-center align-bottom"
                  style={{ minWidth: '2.4rem' }}
                >
                  <span
                    className="block text-[10px] font-bold uppercase tracking-wider"
                    style={{ color: classColor(cls) }}
                    title={`${cls} — ${classTagline(cls)}`}
                  >
                    {cls.slice(0, 4)}
                  </span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((p) => {
              const totalMinutes = rows.reduce(
                (s, r) =>
                  s + (r.minutes_per_game ?? 0) * r.games_played,
                0,
              );
              const myMinutes =
                (p.minutes_per_game ?? 0) * p.games_played;
              const shareTxt =
                totalMinutes > 0
                  ? ((myMinutes / totalMinutes) * 100).toFixed(1) + '%'
                  : '—';
              return (
                <tr key={p.player_id} className="hover:bg-gray-900/40">
                  <td
                    className="py-1 pr-3 truncate sticky left-0 bg-gray-800 z-10"
                    style={{ minWidth: '11rem', maxWidth: '14rem' }}
                  >
                    <span className="text-gray-100">{p.name}</span>
                    {p.primary_class && (
                      <span
                        className="ml-1.5 text-[10px]"
                        style={{ color: classColor(p.primary_class) }}
                      >
                        {p.primary_class}
                      </span>
                    )}
                  </td>
                  <td className="py-1 pr-3 text-right text-gray-400 tabular-nums">
                    {shareTxt}
                  </td>
                  {CLASS_ORDER.map((cls) => {
                    const score = p.affinity_scores?.[cls] ?? 0;
                    const opacity = opacityFromScore(score, fullOpacityAt);
                    const color = classColor(cls);
                    const isHovered =
                      hovered?.player.player_id === p.player_id &&
                      hovered?.className === cls;
                    return (
                      <td key={cls} className="px-0.5 py-0.5">
                        <div
                          className="h-5 rounded-sm cursor-pointer transition-transform"
                          style={{
                            background: color,
                            opacity,
                            outline: isHovered
                              ? `1px solid ${color}`
                              : undefined,
                            outlineOffset: 1,
                          }}
                          onMouseEnter={() =>
                            setHovered({ player: p, className: cls, score })
                          }
                          onMouseLeave={() =>
                            setHovered((h) =>
                              h?.player.player_id === p.player_id &&
                              h?.className === cls
                                ? null
                                : h,
                            )
                          }
                          onClick={(e) => {
                            e.stopPropagation();
                            setHovered((h) =>
                              h?.player.player_id === p.player_id &&
                              h?.className === cls
                                ? null
                                : { player: p, className: cls, score },
                            );
                          }}
                        />
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {popover}
    </div>
  );
}
