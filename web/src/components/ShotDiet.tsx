import { useId } from 'react';
import type { TorkvikStats } from '../api/client';

// Saturated red → yellow → green gradient for shot efficiency (percentile 0-1).
const efficiencyColor = (pctile: number | null | undefined) => {
  if (pctile == null) return '#4b5563';
  const p = Math.max(0, Math.min(1, pctile));
  if (p <= 0.5) {
    const t = p / 0.5;
    const r = Math.round(239 + (250 - 239) * t);
    const g = Math.round(68 + (204 - 68) * t);
    const b = Math.round(68 + (21 - 68) * t);
    return `rgb(${r},${g},${b})`;
  }
  const t = (p - 0.5) / 0.5;
  const r = Math.round(250 + (34 - 250) * t);
  const g = Math.round(204 + (211 - 204) * t);
  const b = Math.round(21 + (103 - 21) * t);
  return `rgb(${r},${g},${b})`;
};

export function ShotDietCourt({ torvik }: { torvik: TorkvikStats }) {
  const filterId = `zone-glow-${useId().replace(/:/g, '')}`;

  const rimPct = torvik.rim_pct != null ? torvik.rim_pct * 100 : null;
  const midPct = torvik.mid_pct != null ? torvik.mid_pct * 100 : null;
  const tpPctVal = torvik.tp_pct != null ? torvik.tp_pct * 100 : null;
  const ftPct =
    torvik.ftm != null && torvik.fta != null && torvik.fta > 0
      ? (torvik.ftm / torvik.fta) * 100
      : null;

  const rimAtt = torvik.rim_attempted ?? 0;
  const midAtt = torvik.mid_attempted ?? 0;
  const tpAtt = torvik.tpa ?? 0;
  const totalAtt = rimAtt + midAtt + tpAtt;
  const volOpacity = (att: number) =>
    totalAtt > 0 ? Math.min(0.4 + (att / totalAtt) * 1.2, 0.95) : 0.4;

  const cx = 150;
  const hoopY = 14;

  return (
    <svg viewBox="0 0 300 200" className="w-full max-w-lg">
      <defs>
        <filter id={filterId} x="-20%" y="-20%" width="140%" height="140%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="3" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>

      <rect x="0" y="0" width="300" height="200" rx="6" fill="#1f2937" />

      <g filter={`url(#${filterId})`}>
        <rect x="10" y="0" width="280" height="200" fill={efficiencyColor(torvik.tp_pct_pct)} opacity={volOpacity(tpAtt)} />
        <path d="M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0 Z" fill={efficiencyColor(torvik.mid_pct_pct)} opacity={volOpacity(midAtt)} />
        <rect x="105" y="0" width="90" height="108" fill={efficiencyColor(torvik.rim_pct_pct)} opacity={volOpacity(rimAtt)} />
        <path
          d="M 105 108 A 45 45 0 0 0 195 108"
          fill={efficiencyColor(ftPct != null ? Math.min(Math.max((ftPct - 55) / 35, 0), 1) : null)}
          opacity="0.65"
        />
      </g>

      <rect x="10" y="0" width="280" height="200" fill="none" stroke="rgba(255,255,255,0.35)" strokeWidth="1" />
      <line x1="10" y1="0" x2="290" y2="0" stroke="rgba(255,255,255,0.5)" strokeWidth="1.5" />
      <rect x="105" y="0" width="90" height="108" fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
      <path d="M 105 108 A 45 45 0 0 0 195 108" fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
      <path d="M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0" fill="none" stroke="rgba(255,255,255,0.35)" strokeWidth="1" />
      <path d={`M ${cx - 20} ${hoopY} A 20 20 0 0 0 ${cx + 20} ${hoopY}`} fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
      <circle cx={cx} cy={hoopY} r="5" fill="none" stroke="#f97316" strokeWidth="1.5" />
      <line x1={cx - 15} y1={hoopY - 6} x2={cx + 15} y2={hoopY - 6} stroke="rgba(255,255,255,0.4)" strokeWidth="1.5" />

      <g style={{ filter: 'drop-shadow(0 1px 2px rgba(0,0,0,0.8))' }}>
        <text x={cx} y="48" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Rim</text>
        <text x={cx} y="62" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
          {rimPct != null ? `${rimPct.toFixed(1)}%` : '—'}
        </text>
        <text x={cx} y="74" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
          {torvik.rim_made ?? 0}-{torvik.rim_attempted ?? 0}
        </text>

        <text x={cx} y="123" textAnchor="middle" fill="white" fontSize="10" fontWeight="600">FT</text>
        <text x={cx} y="134" textAnchor="middle" fill="white" fontSize="9" opacity="0.9">
          {ftPct != null ? `${ftPct.toFixed(1)}%` : '—'}
        </text>
        <text x={cx} y="144" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
          {torvik.ftm ?? 0}-{torvik.fta ?? 0}
        </text>

        <text x="232" y="55" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Mid</text>
        <text x="232" y="68" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
          {midPct != null ? `${midPct.toFixed(1)}%` : '—'}
        </text>
        <text x="232" y="78" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
          {torvik.mid_made ?? 0}-{torvik.mid_attempted ?? 0}
        </text>

        <text x="50" y="155" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">3PT</text>
        <text x="50" y="168" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
          {tpPctVal != null ? `${tpPctVal.toFixed(1)}%` : '—'}
        </text>
        <text x="50" y="180" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
          {torvik.tpm ?? 0}-{torvik.tpa ?? 0}
        </text>
      </g>
    </svg>
  );
}

export function ShotDistributionBar({ torvik }: { torvik: TorkvikStats }) {
  const rimAtt = torvik.rim_attempted ?? 0;
  const midAtt = torvik.mid_attempted ?? 0;
  const tpAtt = torvik.tpa ?? 0;
  const totalAtt = rimAtt + midAtt + tpAtt;
  if (totalAtt === 0) return null;
  const rimW = (rimAtt / totalAtt) * 100;
  const midW = (midAtt / totalAtt) * 100;
  const tpW = (tpAtt / totalAtt) * 100;
  return (
    <div className="flex rounded-full h-7 overflow-hidden text-xs font-medium gap-[2px]">
      {rimW > 0 && (
        <div
          className="flex items-center justify-center first:rounded-l-full"
          style={{ width: `${rimW}%`, backgroundColor: efficiencyColor(torvik.rim_pct_pct) }}
        >
          {rimW >= 15 ? `Rim ${rimW.toFixed(0)}%` : ''}
        </div>
      )}
      {midW > 0 && (
        <div
          className="flex items-center justify-center"
          style={{ width: `${midW}%`, backgroundColor: efficiencyColor(torvik.mid_pct_pct) }}
        >
          {midW >= 15 ? `Mid ${midW.toFixed(0)}%` : ''}
        </div>
      )}
      {tpW > 0 && (
        <div
          className="flex items-center justify-center last:rounded-r-full"
          style={{ width: `${tpW}%`, backgroundColor: efficiencyColor(torvik.tp_pct_pct) }}
        >
          {tpW >= 15 ? `3PT ${tpW.toFixed(0)}%` : ''}
        </div>
      )}
    </div>
  );
}
