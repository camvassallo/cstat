import { useMemo, useState } from 'react';
import type { ArchetypeShare } from '../api/client';
import { CLASS_ORDER, classColor, classTagline } from './archetypeColors';
import { useDismissOnOutside } from './useDismissOnOutside';

/// 10×10 waffle of the team's minute distribution across archetype
/// primary classes. Each square ≈ 1% of team minutes (with secondary
/// archetype weighted at 0.5× — same convention as `team_share` itself).
///
/// Reads as a "minutes budget": 100 squares total, grouped into
/// contiguous blocks per class so a Sorcerer-heavy team has a visible
/// red region you can count at a glance ("28% of the team's minutes
/// are Sorcerer"). More honest to perception than a pie's wedge
/// comparisons; more discrete than a flex bar.
///
/// Integer counts per class are allocated by largest-remainder
/// (Hamilton's method) so the squares always sum to exactly 100 even
/// when each class's `team_share × 100` rounds awkwardly.

interface Props {
  archetypeDist: ArchetypeShare[];
  /// Cell size in px. Default 32 fits a 10×10 grid + 4px gaps into
  /// ~356px, sized to roughly match the basketball-court SVG used in
  /// the sibling `TeamShotDiet` panel for visual balance when both
  /// live in the same 2-column grid on TeamDetail.
  cellSize?: number;
  gap?: number;
}

interface Cell {
  className: string;
  /// Index within this class's contiguous block (0..count-1). Used as
  /// the React key so the same physical cell keeps its identity even
  /// if `team_share` shifts a percentage point between renders.
  intraIndex: number;
}

interface ClassBucket {
  className: string;
  count: number;
  share: number;
  d1Share: number;
  index: number | null;
}

/// Largest-remainder method: floor every count, then distribute the
/// remaining squares one-by-one to the classes with the largest
/// fractional remainders. Guaranteed to produce integer counts that
/// sum to exactly `total`.
function allocateCounts(
  buckets: { className: string; share: number }[],
  total: number,
): Map<string, number> {
  const out = new Map<string, number>();
  const raw = buckets.map((b) => ({
    className: b.className,
    exact: b.share * total,
  }));
  let used = 0;
  for (const r of raw) {
    const f = Math.floor(r.exact);
    out.set(r.className, f);
    used += f;
  }
  let remaining = total - used;
  if (remaining <= 0) return out;
  const remainders = raw
    .map((r) => ({ className: r.className, frac: r.exact - Math.floor(r.exact) }))
    .sort((a, b) => b.frac - a.frac);
  let i = 0;
  while (remaining > 0 && i < remainders.length) {
    const k = remainders[i].className;
    out.set(k, (out.get(k) ?? 0) + 1);
    remaining -= 1;
    i += 1;
  }
  return out;
}

export function RosterWaffle({
  archetypeDist,
  cellSize = 32,
  gap = 4,
}: Props) {
  const [hoveredClass, setHoveredClass] = useState<string | null>(null);
  const popoverRef = useDismissOnOutside(hoveredClass !== null, () =>
    setHoveredClass(null),
  );

  const { cells, buckets } = useMemo(() => {
    /// Order classes by the canonical `CLASS_ORDER` so each archetype
    /// lands in the same waffle position across every team. Critical
    /// for the side-by-side use case on the Predict page — comparing
    /// Duke's Sorcerer block to UNC's only reads pre-attentively if
    /// both blocks live in the same region of the grid. The tradeoff
    /// vs sorting by dominance is that the visual "biggest first"
    /// hierarchy is lost, but the chip row below already names the
    /// concentrated classes, so dominance reads from the chips and
    /// position reads from the waffle.
    const byClass = new Map(
      archetypeDist.map((a) => [a.primary_class, a]),
    );
    const present = CLASS_ORDER.map((cls) => byClass.get(cls)).filter(
      (a): a is ArchetypeShare => a !== undefined && a.team_share > 0,
    );
    if (present.length === 0) return { cells: [] as Cell[], buckets: [] as ClassBucket[] };

    const counts = allocateCounts(
      present.map((a) => ({ className: a.primary_class, share: a.team_share })),
      100,
    );

    const orderedBuckets: ClassBucket[] = present.map((a) => ({
      className: a.primary_class,
      count: counts.get(a.primary_class) ?? 0,
      share: a.team_share,
      d1Share: a.d1_share,
      index: a.index,
    }));

    const flat: Cell[] = [];
    for (const b of orderedBuckets) {
      for (let i = 0; i < b.count; i++) {
        flat.push({ className: b.className, intraIndex: i });
      }
    }
    return { cells: flat, buckets: orderedBuckets };
  }, [archetypeDist]);

  if (cells.length === 0) return null;

  const hoveredBucket = hoveredClass
    ? buckets.find((b) => b.className === hoveredClass) ?? null
    : null;

  const popover = hoveredBucket ? (
    <div
      className="pointer-events-none absolute z-30 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 text-left"
      style={{
        top: '100%',
        left: '50%',
        transform: 'translate(-50%, 0.5rem)',
        minWidth: '14rem',
        maxWidth: '18rem',
      }}
    >
      <div className="flex items-baseline justify-between gap-2 mb-0.5">
        <span
          className="text-xs font-bold"
          style={{ color: classColor(hoveredBucket.className) }}
        >
          {hoveredBucket.className}
        </span>
        <span className="text-[11px] text-gray-200 tabular-nums font-semibold">
          {hoveredBucket.count} {hoveredBucket.count === 1 ? 'square' : 'squares'}
        </span>
      </div>
      <div className="text-[11px] text-gray-400">
        {classTagline(hoveredBucket.className)}
      </div>
      <div className="text-[10px] text-gray-500 mt-1 tabular-nums">
        {(hoveredBucket.share * 100).toFixed(1)}% team ·{' '}
        {(hoveredBucket.d1Share * 100).toFixed(1)}% D-I
        {hoveredBucket.index != null && (
          <> · {hoveredBucket.index.toFixed(2)}×</>
        )}
      </div>
    </div>
  ) : null;

  /// Layout the 100 cells as a 10×10 grid. Reading order is
  /// left-to-right, top-to-bottom (English-language convention) so
  /// the dominant class fills the upper-left.
  const COLS = 10;
  const rows = Math.ceil(cells.length / COLS);
  const width = COLS * cellSize + (COLS - 1) * gap;
  const height = rows * cellSize + (rows - 1) * gap;

  return (
    <div
      className="relative"
      ref={(node) => {
        popoverRef.current = node;
      }}
    >
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        // Natural width (356px at the default cell size) is the cap, but the
        // viewBox lets the grid scale down to fit a narrow phone column rather
        // than overflowing past `<main>`'s `overflow-x-clip` and getting
        // silently clipped. `h-auto` keeps the squares square as it shrinks.
        className="block max-w-full h-auto"
        role="img"
        aria-label="Team minutes distribution by archetype"
      >
        {cells.map((c, idx) => {
          const col = idx % COLS;
          const row = Math.floor(idx / COLS);
          const x = col * (cellSize + gap);
          const y = row * (cellSize + gap);
          const isHovered = hoveredClass === c.className;
          const color = classColor(c.className);
          return (
            <rect
              key={`${c.className}-${c.intraIndex}`}
              x={x}
              y={y}
              width={cellSize}
              height={cellSize}
              rx={3}
              ry={3}
              fill={color}
              opacity={isHovered ? 1 : 0.92}
              stroke={isHovered ? '#f3f4f6' : 'none'}
              strokeWidth={isHovered ? 1.25 : 0}
              style={{ cursor: 'pointer', transition: 'opacity 80ms' }}
              onMouseEnter={() => setHoveredClass(c.className)}
              onMouseLeave={() =>
                setHoveredClass((h) => (h === c.className ? null : h))
              }
              onClick={(e) => {
                e.stopPropagation();
                setHoveredClass((h) =>
                  h === c.className ? null : c.className,
                );
              }}
            />
          );
        })}
      </svg>
      {popover}
    </div>
  );
}
