// Custom tick renderer for `<PolarAngleAxis tick={...}>`. Recharts passes
// (x, y, payload, textAnchor) per spoke; we wrap the label in a clickable
// `<g>` with a 64×20 invisible hit target so taps are comfortable on touch.

type TextAnchor = 'start' | 'middle' | 'end' | 'inherit';

interface Props {
  // Recharts-injected props (numeric coords arrive as `number | string`).
  x?: number | string;
  y?: number | string;
  payload?: { value: string };
  textAnchor?: TextAnchor;
  // App props.
  selected: boolean;
  onSelect: (axis: string) => void;
}

export function RadarTick({
  x = 0,
  y = 0,
  payload,
  textAnchor,
  selected,
  onSelect,
}: Props) {
  if (!payload) return null;
  const label = payload.value;
  const cx = typeof x === 'number' ? x : Number(x);
  const cy = typeof y === 'number' ? y : Number(y);
  return (
    <g
      style={{ cursor: 'pointer' }}
      onClick={(e) => {
        e.stopPropagation();
        onSelect(label);
      }}
    >
      <rect
        x={cx - 36}
        y={cy - 10}
        width={72}
        height={20}
        fill="transparent"
      />
      <text
        x={cx}
        y={cy}
        textAnchor={textAnchor}
        dominantBaseline="middle"
        fill={selected ? '#60a5fa' : '#94a3b8'}
        fontSize={12}
        fontWeight={selected ? 600 : 400}
        style={{ userSelect: 'none' }}
      >
        {label}
      </text>
    </g>
  );
}
