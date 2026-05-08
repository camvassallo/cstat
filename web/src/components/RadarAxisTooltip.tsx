import type { ResolvedAxis } from './radarAxes';

interface ResolvedAxisForLabel extends ResolvedAxis {
  /** Optional player label — renders as a colored chip when multiple players
   *  are passed (Compare page). Single-player view omits this. */
  playerLabel?: string;
  /** Player color for the chip (PLAYER_COLORS[i] on Compare). */
  playerColor?: string;
}

interface Props {
  /** One entry per player. Length 1 = PlayerDetail; >1 = PlayerCompare. */
  resolutions: ResolvedAxisForLabel[];
  onClose: () => void;
}

/** Compact popover: axis name + plain-English blurb + per-player raw value
 *  and percentile. Deliberately minimal — no fallback chains, no source-of-
 *  truth callouts. */
export function RadarAxisTooltip({ resolutions, onClose }: Props) {
  if (resolutions.length === 0) return null;
  const first = resolutions[0];
  const multi = resolutions.length > 1;

  return (
    <div
      className="absolute z-20 top-2 right-2 max-w-[260px] rounded-md border border-gray-700 bg-gray-900/95 backdrop-blur p-3 text-xs shadow-lg"
      role="dialog"
      aria-label={`${first.stat} details`}
    >
      <div className="flex items-start justify-between gap-2 mb-1">
        <div className="font-semibold text-sm text-gray-100">{first.stat}</div>
        <button
          onClick={onClose}
          className="text-gray-500 hover:text-gray-200 leading-none"
          aria-label="Close"
        >
          ×
        </button>
      </div>
      <div className="text-gray-400 mb-2 leading-snug">{first.blurb}</div>
      <div className="space-y-1">
        {resolutions.map((r, i) => (
          <div
            key={i}
            className="flex items-baseline justify-between gap-2"
          >
            {multi && (
              <span
                className="font-semibold whitespace-nowrap"
                style={{ color: r.playerColor ?? '#cbd5e1' }}
              >
                {r.playerLabel}
              </span>
            )}
            <span className="text-gray-100 flex-1 text-right">
              {r.rawValue ?? '—'}
            </span>
            <span className="text-gray-500 whitespace-nowrap w-10 text-right">
              {r.percentile != null ? `${r.percentile}th` : '—'}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
