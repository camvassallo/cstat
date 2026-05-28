import { useMemo, useState } from 'react';
import type { RosterEntry } from '../api/client';
import { CLASS_ORDER, classColor } from './archetypeColors';
import { useDismissOnOutside } from './useDismissOnOutside';

/// 12-spoked radial plot of a roster's archetype distribution. Each
/// player is placed at the angle of their primary class, with radial
/// distance proportional to their minutes share on the roster. A dimmed
/// half-radius point at the secondary class is connected to the primary
/// point with a thin line, mirroring the 1.0× primary / 0.5× secondary
/// weighting the rest of the site uses for archetype distributions.
///
/// Pure-frontend: the team-detail API already returns `primary_class`,
/// `secondary_class`, `minutes_per_game`, and `games_played` on every
/// `RosterEntry`. Archetype-less players (sub-D1 transfers, late-joining
/// walk-ons) can't be placed angularly and are surfaced as a small
/// caption below the plot.
///
/// Accepts a multi-roster array so the Team Compare page and the Predict
/// page's Roster Compare panel can drop in 2-team overlays without a
/// second component.

interface PointDatum {
  player: RosterEntry;
  rosterIdx: number;
  /// Minute share on this player's roster, 0..1.
  share: number;
  /// Primary class angle, radians. `null` when the player has no class.
  primaryAngle: number | null;
  /// Secondary class angle, radians. `null` when no secondary.
  secondaryAngle: number | null;
}

export interface RosterRadialPlotRoster {
  /// Short label rendered above the plot legend (e.g. team name or "Home").
  label: string;
  roster: RosterEntry[];
  /// Optional accent stroke for primary points. Defaults: roster 0 = bright
  /// (full saturation), roster 1 = hollow ring. Archetype color always
  /// drives the fill so spoke identity stays self-reinforcing.
  accent?: string;
}

interface Props {
  rosters: RosterRadialPlotRoster[];
  /// Outer dimension in px. The actual chart radius is ~38% of this so
  /// labels at the 12 spoke ends have room.
  size?: number;
}

const ANGLE_STEP = (2 * Math.PI) / CLASS_ORDER.length;
/// Place the first spoke at the top (12 o'clock) and go clockwise.
const ANGLE_FOR_INDEX = (i: number) => -Math.PI / 2 + i * ANGLE_STEP;
const CLASS_ANGLE: Map<string, number> = new Map(
  CLASS_ORDER.map((cls, i) => [cls, ANGLE_FOR_INDEX(i)]),
);

function polar(angle: number, radius: number): [number, number] {
  return [Math.cos(angle) * radius, Math.sin(angle) * radius];
}

function buildPoints(roster: RosterEntry[], rosterIdx: number): PointDatum[] {
  const total = roster.reduce(
    (s, p) => s + (p.minutes_per_game ?? 0) * p.games_played,
    0,
  );
  return roster.map((player) => {
    const minutes = (player.minutes_per_game ?? 0) * player.games_played;
    const share = total > 0 ? minutes / total : 0;
    const primaryAngle = player.primary_class
      ? (CLASS_ANGLE.get(player.primary_class) ?? null)
      : null;
    const secondaryAngle = player.secondary_class
      ? (CLASS_ANGLE.get(player.secondary_class) ?? null)
      : null;
    return { player, rosterIdx, share, primaryAngle, secondaryAngle };
  });
}

export function RosterRadialPlot({ rosters, size = 360 }: Props) {
  const [hovered, setHovered] = useState<PointDatum | null>(null);
  const popoverRef = useDismissOnOutside(hovered !== null, () => setHovered(null));

  const allPoints = useMemo(
    () => rosters.flatMap((r, idx) => buildPoints(r.roster, idx)),
    [rosters],
  );

  const placeable = allPoints.filter((p) => p.primaryAngle !== null);
  const unplaceable = allPoints.filter((p) => p.primaryAngle === null);

  /// Scale: max plotted share governs the outer ring. Cap at 35% so a
  /// single dominant player doesn't compress the rest of the roster
  /// into the inner third of the plot. The outer ring then displays as
  /// `≥35%` rather than the literal max.
  const maxShare = useMemo(() => {
    const m = Math.max(0, ...placeable.map((p) => p.share));
    return Math.min(Math.max(m, 0.2), 0.35);
  }, [placeable]);

  const chartRadius = size * 0.38;
  const labelRadius = chartRadius + 22;

  /// Deterministic angular jitter on overlapping points (same primary
  /// class, same roster). Spread up to ±15° within the spoke wedge so
  /// 3-4 players sharing a class don't collapse to one dot. The order
  /// is determined by player_id hash for stability across renders.
  const jitterMap = useMemo(() => {
    const buckets = new Map<string, PointDatum[]>();
    for (const p of placeable) {
      const key = `${p.rosterIdx}|${p.player.primary_class}`;
      const arr = buckets.get(key) ?? [];
      arr.push(p);
      buckets.set(key, arr);
    }
    const result = new Map<string, number>();
    const maxJitter = ANGLE_STEP * 0.18;
    for (const arr of buckets.values()) {
      arr.sort((a, b) => a.player.player_id.localeCompare(b.player.player_id));
      const n = arr.length;
      if (n === 1) {
        result.set(arr[0].player.player_id, 0);
      } else {
        for (let i = 0; i < n; i++) {
          const t = n === 1 ? 0 : i / (n - 1) - 0.5;
          result.set(arr[i].player.player_id, t * 2 * maxJitter);
        }
      }
    }
    return result;
  }, [placeable]);

  const popover = hovered ? (
    <div
      className="pointer-events-auto absolute z-30 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 text-left max-w-[14rem]"
      style={{
        left: `${size / 2 + 12}px`,
        top: `${12}px`,
      }}
    >
      <div className="flex items-baseline justify-between gap-2 mb-0.5">
        <span className="text-xs font-bold text-gray-100 truncate">
          {hovered.player.name}
        </span>
        {hovered.player.campom != null && (
          <span className="text-[11px] text-gray-400 tabular-nums">
            {hovered.player.campom.toFixed(1)}
          </span>
        )}
      </div>
      <div className="text-[11px] text-gray-300">
        {hovered.player.primary_class && (
          <span style={{ color: classColor(hovered.player.primary_class) }}>
            {hovered.player.primary_class}
          </span>
        )}
        {hovered.player.secondary_class && (
          <>
            {' / '}
            <span style={{ color: classColor(hovered.player.secondary_class) }}>
              {hovered.player.secondary_class}
            </span>
          </>
        )}
      </div>
      <div className="text-[10px] text-gray-500 mt-1 tabular-nums">
        {(hovered.share * 100).toFixed(1)}% of roster minutes ·{' '}
        {hovered.player.minutes_per_game?.toFixed(1) ?? '—'} MPG ·{' '}
        {hovered.player.games_played} GP
      </div>
    </div>
  ) : null;

  return (
    <div
      className="flex flex-col items-center"
      ref={(node) => {
        popoverRef.current = node;
      }}
    >
      <div className="relative" style={{ width: size, height: size }}>
      <svg
        viewBox={`${-size / 2} ${-size / 2} ${size} ${size}`}
        width={size}
        height={size}
        className="overflow-visible"
        role="img"
        aria-label="Roster archetype distribution"
      >
        {/* Concentric reference rings: inner 25/50/75% of max */}
        {[0.25, 0.5, 0.75, 1].map((frac) => (
          <circle
            key={frac}
            cx={0}
            cy={0}
            r={chartRadius * frac}
            fill="none"
            stroke="#374151"
            strokeWidth={frac === 1 ? 1 : 0.5}
            strokeDasharray={frac === 1 ? undefined : '2 3'}
          />
        ))}

        {/* 12 spokes */}
        {CLASS_ORDER.map((cls, i) => {
          const angle = ANGLE_FOR_INDEX(i);
          const [x, y] = polar(angle, chartRadius);
          return (
            <line
              key={cls}
              x1={0}
              y1={0}
              x2={x}
              y2={y}
              stroke="#374151"
              strokeWidth={0.5}
            />
          );
        })}

        {/* Spoke labels (class names) */}
        {CLASS_ORDER.map((cls, i) => {
          const angle = ANGLE_FOR_INDEX(i);
          const [x, y] = polar(angle, labelRadius);
          /// Text-anchor by horizontal position so labels grow outward
          /// rather than overlapping the chart on the sides.
          const cos = Math.cos(angle);
          const anchor =
            Math.abs(cos) < 0.3 ? 'middle' : cos > 0 ? 'start' : 'end';
          return (
            <text
              key={cls}
              x={x}
              y={y}
              textAnchor={anchor}
              dominantBaseline="middle"
              className="text-[10px] font-semibold uppercase tracking-wider"
              fill={classColor(cls)}
            >
              {cls}
            </text>
          );
        })}

        {/* Secondary→primary segments (drawn first so primary points sit on top) */}
        {placeable.map((p) => {
          if (p.secondaryAngle === null) return null;
          const jitter = jitterMap.get(p.player.player_id) ?? 0;
          const r = (p.share / maxShare) * chartRadius;
          const [x1, y1] = polar(p.primaryAngle! + jitter, r);
          const [x2, y2] = polar(p.secondaryAngle, r * 0.5);
          return (
            <line
              key={`seg-${p.player.player_id}`}
              x1={x1}
              y1={y1}
              x2={x2}
              y2={y2}
              stroke={classColor(p.player.primary_class)}
              strokeWidth={0.75}
              strokeOpacity={0.35}
            />
          );
        })}

        {/* Secondary points: half radius, dimmed */}
        {placeable.map((p) => {
          if (p.secondaryAngle === null) return null;
          const r = (p.share / maxShare) * chartRadius;
          const [x, y] = polar(p.secondaryAngle, r * 0.5);
          return (
            <circle
              key={`sec-${p.player.player_id}`}
              cx={x}
              cy={y}
              r={2.5}
              fill={classColor(p.player.secondary_class)}
              fillOpacity={0.45}
              stroke="#0b1220"
              strokeWidth={0.5}
            />
          );
        })}

        {/* Primary points — sized by minutes share, color by primary class.
            Mounted last so they paint on top of segments + secondaries. */}
        {placeable.map((p) => {
          const jitter = jitterMap.get(p.player.player_id) ?? 0;
          const r = (p.share / maxShare) * chartRadius;
          const [x, y] = polar(p.primaryAngle! + jitter, r);
          /// Radius scales with share so high-minutes starters are
          /// visually heavier than rotation depth. Clamped so a deep
          /// bench player is still clickable.
          const dotR = 3 + Math.sqrt(p.share) * 7;
          const isSecondRoster = p.rosterIdx === 1;
          const isHovered =
            hovered?.player.player_id === p.player.player_id &&
            hovered?.rosterIdx === p.rosterIdx;
          return (
            <g key={`prim-${p.rosterIdx}-${p.player.player_id}`}>
              {/* Invisible larger hit target. mouseEnter/Leave drive
                  the desktop hover affordance (they only fire for
                  pointer:fine devices); onClick is the touch path —
                  tap to pin open, tap outside to close via the
                  dismiss-on-outside hook. Using `onPointerEnter`
                  instead would race the click on touch and immediately
                  close the popover. */}
              <circle
                cx={x}
                cy={y}
                r={Math.max(dotR + 4, 10)}
                fill="transparent"
                style={{ cursor: 'pointer' }}
                onMouseEnter={() => setHovered(p)}
                onMouseLeave={() =>
                  setHovered((h) => (h === p ? null : h))
                }
                onClick={(e) => {
                  e.stopPropagation();
                  setHovered((h) => (h === p ? null : p));
                }}
              />
              <circle
                cx={x}
                cy={y}
                r={dotR}
                fill={isSecondRoster ? '#0b1220' : classColor(p.player.primary_class)}
                fillOpacity={isSecondRoster ? 1 : 0.9}
                stroke={classColor(p.player.primary_class)}
                strokeWidth={isHovered ? 2 : 1}
                style={{ pointerEvents: 'none' }}
              />
            </g>
          );
        })}

        {/* Center dot */}
        <circle cx={0} cy={0} r={1.5} fill="#4b5563" />
      </svg>

        {popover}
      </div>

      {/* Unplaceable players footnote — outside the size-constrained
          chart frame so it never overlaps the bottom spoke labels. */}
      {unplaceable.length > 0 && (
        <div className="mt-2 text-[10px] text-gray-500 text-center max-w-md px-2">
          +{unplaceable.length} unclassified:{' '}
          {unplaceable
            .slice(0, 3)
            .map((p) => p.player.name)
            .join(', ')}
          {unplaceable.length > 3 && ` (+${unplaceable.length - 3} more)`}
        </div>
      )}
    </div>
  );
}
