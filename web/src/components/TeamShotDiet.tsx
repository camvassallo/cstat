import { useId, useMemo, useState } from 'react';
import type { RosterEntry } from '../api/client';
import { efficiencyColor } from './shotEfficiency';
import { useDismissOnOutside } from './useDismissOnOutside';

/// Team-aggregate version of `ShotDietCourt`. Same court geometry,
/// but zones are colored from team-aggregate FG% (mapped through
/// rough D-I bands → percentile → red/yellow/green) and sized by
/// team-aggregate volume opacity. Hovering or tapping any zone opens
/// a tooltip with the team aggregate for that zone plus a "who shoots
/// what" list — the rotation players who contributed most to that
/// zone, with their personal makes/attempts and FG%. The per-player
/// rows that used to sit beneath the court live here now: they only
/// surface when the user asks for them.
///
/// Players without Torvik shot data are surfaced via a footnote.

type ZoneKey = 'rim' | 'mid' | 'tp' | 'ft';

const ZONE_LABELS: Record<ZoneKey, string> = {
  rim: 'Rim',
  mid: 'Mid',
  tp: '3PT',
  ft: 'FT',
};

interface PlayerShotRow {
  player: RosterEntry;
  rim: number;
  mid: number;
  tp: number;
  ft: number;
  rimMade: number;
  midMade: number;
  tpMade: number;
  ftMade: number;
  totalShots: number;
}

interface TeamAggregate {
  rimAtt: number;
  midAtt: number;
  tpAtt: number;
  ftAtt: number;
  rimMade: number;
  midMade: number;
  tpMade: number;
  ftMade: number;
  totalFga: number;
}

/// Map a zone FG% to a 0..1 pseudo-percentile so we can re-use the
/// shared `efficiencyColor` gradient. The bands are rough D-I
/// averages ±~1 SD — calibrated to make "league-average" map near
/// 0.5 and elite ~0.9. Not as precise as a real percentile (we
/// don't compute team-level zone percentiles in the DB), but plenty
/// directional for the red→yellow→green visual.
function zonePctile(zone: ZoneKey, fgPct: number | null): number | null {
  if (fgPct == null) return null;
  const [lo, hi]: [number, number] =
    zone === 'rim'
      ? [55, 72]
      : zone === 'mid'
        ? [28, 46]
        : zone === 'tp'
          ? [28, 40]
          : [60, 82];
  const raw = (fgPct - lo) / (hi - lo);
  return Math.max(0, Math.min(1, raw));
}

export function TeamShotDiet({ roster }: { roster: RosterEntry[] }) {
  const filterId = `team-zone-glow-${useId().replace(/:/g, '')}`;
  const [hoveredZone, setHoveredZone] = useState<ZoneKey | null>(null);
  const popoverRef = useDismissOnOutside(hoveredZone !== null, () =>
    setHoveredZone(null),
  );

  const { team, players, missing } = useMemo(() => {
    const players: PlayerShotRow[] = [];
    const missing: RosterEntry[] = [];
    const team: TeamAggregate = {
      rimAtt: 0,
      midAtt: 0,
      tpAtt: 0,
      ftAtt: 0,
      rimMade: 0,
      midMade: 0,
      tpMade: 0,
      ftMade: 0,
      totalFga: 0,
    };

    for (const p of roster) {
      const rim = p.rim_attempted ?? 0;
      const mid = p.mid_attempted ?? 0;
      const tp = p.tpa ?? 0;
      const ft = p.fta ?? 0;
      const rimMade = p.rim_made ?? 0;
      const midMade = p.mid_made ?? 0;
      const tpMade = p.tpm ?? 0;
      const ftMade = p.ftm ?? 0;
      const totalShots = rim + mid + tp;

      if (rim + mid + tp + ft === 0) {
        /// Skip players with no shot data. Flag rotation-meaningful
        /// missing players via the footnote so users know the
        /// picture isn't complete for thin rosters / non-D1 arrivals.
        if ((p.minutes_per_game ?? 0) >= 5 && p.games_played >= 5) {
          missing.push(p);
        }
        continue;
      }
      players.push({
        player: p,
        rim,
        mid,
        tp,
        ft,
        rimMade,
        midMade,
        tpMade,
        ftMade,
        totalShots,
      });
      team.rimAtt += rim;
      team.midAtt += mid;
      team.tpAtt += tp;
      team.ftAtt += ft;
      team.rimMade += rimMade;
      team.midMade += midMade;
      team.tpMade += tpMade;
      team.ftMade += ftMade;
    }
    team.totalFga = team.rimAtt + team.midAtt + team.tpAtt;
    return { team, players, missing };
  }, [roster]);

  if (team.totalFga === 0) {
    return (
      <div className="text-xs text-gray-500 italic">
        No Torvik shot-zone data available for this roster.
      </div>
    );
  }

  // Team-level zone FG% (cumulative across roster minutes — these are
  // attempts/makes totals, not per-minute rates).
  const rimPct = team.rimAtt > 0 ? (team.rimMade / team.rimAtt) * 100 : null;
  const midPct = team.midAtt > 0 ? (team.midMade / team.midAtt) * 100 : null;
  const tpPct = team.tpAtt > 0 ? (team.tpMade / team.tpAtt) * 100 : null;
  const ftPct = team.ftAtt > 0 ? (team.ftMade / team.ftAtt) * 100 : null;

  // Volume opacity per zone — share of team FGA. Matches the
  // ShotDietCourt scaling so "high-volume" zones read as bright.
  const volOpacity = (att: number) =>
    team.totalFga > 0
      ? Math.min(0.4 + (att / team.totalFga) * 1.2, 0.95)
      : 0.4;

  const rimColor = efficiencyColor(zonePctile('rim', rimPct));
  const midColor = efficiencyColor(zonePctile('mid', midPct));
  const tpColor = efficiencyColor(zonePctile('tp', tpPct));
  const ftColor = efficiencyColor(zonePctile('ft', ftPct));

  const cx = 150;
  const hoopY = 14;

  // Per-zone team aggregates + top contributors for the popover.
  const zoneAgg = (zone: ZoneKey) => {
    const att =
      zone === 'rim' ? team.rimAtt
      : zone === 'mid' ? team.midAtt
      : zone === 'tp' ? team.tpAtt
      : team.ftAtt;
    const made =
      zone === 'rim' ? team.rimMade
      : zone === 'mid' ? team.midMade
      : zone === 'tp' ? team.tpMade
      : team.ftMade;
    const share = zone === 'ft'
      ? team.totalFga > 0 ? team.ftAtt / team.totalFga : 0  // FT rate (FTA / FGA), not FT share of FGA
      : team.totalFga > 0 ? att / team.totalFga : 0;
    const fgPct = att > 0 ? (made / att) * 100 : null;
    return { att, made, share, fgPct };
  };

  const topContributors = (zone: ZoneKey) => {
    const rows = players
      .map((p) => {
        const att =
          zone === 'rim' ? p.rim
          : zone === 'mid' ? p.mid
          : zone === 'tp' ? p.tp
          : p.ft;
        const made =
          zone === 'rim' ? p.rimMade
          : zone === 'mid' ? p.midMade
          : zone === 'tp' ? p.tpMade
          : p.ftMade;
        return { player: p.player, att, made };
      })
      .filter((r) => r.att > 0)
      .sort((a, b) => b.att - a.att)
      .slice(0, 5);
    return rows;
  };

  const popover = hoveredZone ? (
    <div
      className="pointer-events-none absolute z-30 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 text-left"
      style={{
        top: '100%',
        left: '50%',
        transform: 'translate(-50%, 0.5rem)',
        minWidth: '18rem',
        maxWidth: '22rem',
      }}
    >
      <ZoneTooltip zone={hoveredZone} zoneAgg={zoneAgg} topContributors={topContributors} />
    </div>
  ) : null;

  const onZoneEnter = (z: ZoneKey) => () => setHoveredZone(z);
  const onZoneLeave = (z: ZoneKey) => () =>
    setHoveredZone((h) => (h === z ? null : h));
  const onZoneClick = (z: ZoneKey) => (e: React.MouseEvent) => {
    e.stopPropagation();
    setHoveredZone((h) => (h === z ? null : z));
  };

  return (
    <div
      className="relative"
      ref={(node) => {
        popoverRef.current = node;
      }}
    >
      <svg
        viewBox="0 0 300 200"
        className="w-full max-w-lg mx-auto block"
        role="img"
        aria-label="Team shot diet on basketball court"
      >
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
          {/* 3PT zone — full backdrop. Interactive only outside the
              mid arc; we render this first so the mid + rim shapes
              layer on top and intercept their own hovers. The hit
              target for 3PT is the visible region NOT covered by
              mid/rim. */}
          <rect
            x="10"
            y="0"
            width="280"
            height="200"
            fill={tpColor}
            opacity={volOpacity(team.tpAtt)}
            style={{ cursor: 'pointer' }}
            onMouseEnter={onZoneEnter('tp')}
            onMouseLeave={onZoneLeave('tp')}
            onClick={onZoneClick('tp')}
          />
          {/* Mid arc */}
          <path
            d="M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0 Z"
            fill={midColor}
            opacity={volOpacity(team.midAtt)}
            style={{ cursor: 'pointer' }}
            onMouseEnter={onZoneEnter('mid')}
            onMouseLeave={onZoneLeave('mid')}
            onClick={onZoneClick('mid')}
          />
          {/* Rim rectangle */}
          <rect
            x="105"
            y="0"
            width="90"
            height="108"
            fill={rimColor}
            opacity={volOpacity(team.rimAtt)}
            style={{ cursor: 'pointer' }}
            onMouseEnter={onZoneEnter('rim')}
            onMouseLeave={onZoneLeave('rim')}
            onClick={onZoneClick('rim')}
          />
          {/* FT semicircle */}
          <path
            d="M 105 108 A 45 45 0 0 0 195 108"
            fill={ftColor}
            opacity="0.65"
            style={{ cursor: 'pointer' }}
            onMouseEnter={onZoneEnter('ft')}
            onMouseLeave={onZoneLeave('ft')}
            onClick={onZoneClick('ft')}
          />
        </g>

        {/* Court outlines (non-interactive) */}
        <g style={{ pointerEvents: 'none' }}>
          <rect x="10" y="0" width="280" height="200" fill="none" stroke="rgba(255,255,255,0.35)" strokeWidth="1" />
          <line x1="10" y1="0" x2="290" y2="0" stroke="rgba(255,255,255,0.5)" strokeWidth="1.5" />
          <rect x="105" y="0" width="90" height="108" fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
          <path d="M 105 108 A 45 45 0 0 0 195 108" fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
          <path d="M 22 0 L 22 72 A 138 138 0 0 0 278 72 L 278 0" fill="none" stroke="rgba(255,255,255,0.35)" strokeWidth="1" />
          <path d={`M ${cx - 20} ${hoopY} A 20 20 0 0 0 ${cx + 20} ${hoopY}`} fill="none" stroke="rgba(255,255,255,0.3)" strokeWidth="0.75" />
          <circle cx={cx} cy={hoopY} r="5" fill="none" stroke="#f97316" strokeWidth="1.5" />
          <line x1={cx - 15} y1={hoopY - 6} x2={cx + 15} y2={hoopY - 6} stroke="rgba(255,255,255,0.4)" strokeWidth="1.5" />
        </g>

        {/* Labels (non-interactive so hover passes through to the
            colored zones beneath). */}
        <g style={{ filter: 'drop-shadow(0 1px 2px rgba(0,0,0,0.8))', pointerEvents: 'none' }}>
          <text x={cx} y="48" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Rim</text>
          <text x={cx} y="62" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
            {rimPct != null ? `${rimPct.toFixed(1)}%` : '—'}
          </text>
          <text x={cx} y="74" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
            {team.totalFga > 0 ? `${((team.rimAtt / team.totalFga) * 100).toFixed(0)}% of FGA` : ''}
          </text>

          <text x={cx} y="123" textAnchor="middle" fill="white" fontSize="10" fontWeight="600">FT</text>
          <text x={cx} y="134" textAnchor="middle" fill="white" fontSize="9" opacity="0.9">
            {ftPct != null ? `${ftPct.toFixed(1)}%` : '—'}
          </text>
          <text x={cx} y="144" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
            FT rate {team.totalFga > 0 ? `${((team.ftAtt / team.totalFga) * 100).toFixed(0)}%` : '—'}
          </text>

          <text x="232" y="55" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">Mid</text>
          <text x="232" y="68" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
            {midPct != null ? `${midPct.toFixed(1)}%` : '—'}
          </text>
          <text x="232" y="78" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
            {team.totalFga > 0 ? `${((team.midAtt / team.totalFga) * 100).toFixed(0)}% of FGA` : ''}
          </text>

          <text x="50" y="155" textAnchor="middle" fill="white" fontSize="11" fontWeight="600">3PT</text>
          <text x="50" y="168" textAnchor="middle" fill="white" fontSize="10" opacity="0.9">
            {tpPct != null ? `${tpPct.toFixed(1)}%` : '—'}
          </text>
          <text x="50" y="180" textAnchor="middle" fill="white" fontSize="8" opacity="0.7">
            {team.totalFga > 0 ? `${((team.tpAtt / team.totalFga) * 100).toFixed(0)}% of FGA` : ''}
          </text>
        </g>
      </svg>

      {/* Horizontal shot-distribution bar — KenPom-style alternative
          read of the same data the court shows. FGA-only (no FT)
          because FT isn't a field-goal attempt and including it
          re-bases the percentages misleadingly. Same color palette
          as the court so the two read as one panel; same hover
          handlers so hovering either surface opens the same
          per-player tooltip. Heading mirrors `Shot Distribution`
          on PlayerDetail. */}
      <div className="mt-5 max-w-lg mx-auto px-1">
        <h4 className="text-sm font-semibold text-gray-300 mb-2">
          Shot Distribution
        </h4>
        <div className="flex h-7 rounded overflow-hidden bg-gray-900 gap-[2px]">
          {[
            { key: 'rim' as const, share: team.totalFga > 0 ? team.rimAtt / team.totalFga : 0, color: rimColor, fgPct: rimPct },
            { key: 'mid' as const, share: team.totalFga > 0 ? team.midAtt / team.totalFga : 0, color: midColor, fgPct: midPct },
            { key: 'tp'  as const, share: team.totalFga > 0 ? team.tpAtt  / team.totalFga : 0, color: tpColor,  fgPct: tpPct  },
          ].map((seg) =>
            seg.share > 0 ? (
              <div
                key={seg.key}
                className="relative flex items-center justify-center text-[11px] font-semibold cursor-pointer transition-opacity"
                style={{
                  width: `${seg.share * 100}%`,
                  background: seg.color,
                  opacity: hoveredZone === seg.key ? 1 : 0.92,
                  color: '#0b1220',
                }}
                onMouseEnter={onZoneEnter(seg.key)}
                onMouseLeave={onZoneLeave(seg.key)}
                onClick={onZoneClick(seg.key)}
              >
                {seg.share >= 0.12 &&
                  `${ZONE_LABELS[seg.key]} ${(seg.share * 100).toFixed(0)}%`}
              </div>
            ) : null,
          )}
        </div>
        <div className="mt-1 flex items-baseline justify-between text-[10px] text-gray-500">
          <span>Shot mix (% of FGA)</span>
          <span
            className="tabular-nums cursor-pointer"
            onMouseEnter={onZoneEnter('ft')}
            onMouseLeave={onZoneLeave('ft')}
            onClick={onZoneClick('ft')}
          >
            FT rate{' '}
            <span
              className="text-gray-200 font-semibold"
              style={{ color: ftColor }}
            >
              {team.totalFga > 0
                ? `${((team.ftAtt / team.totalFga) * 100).toFixed(1)}%`
                : '—'}
            </span>
          </span>
        </div>
      </div>

      {popover}

      {missing.length > 0 && (
        <div className="mt-3 text-[10px] text-gray-500 text-center">
          +{missing.length} rotation player
          {missing.length === 1 ? '' : 's'} missing Torvik shot data:{' '}
          {missing
            .slice(0, 3)
            .map((p) => p.name)
            .join(', ')}
          {missing.length > 3 && ` (+${missing.length - 3} more)`}
        </div>
      )}

      <div className="mt-2 text-[10px] text-gray-500 text-center">
        Hover or tap a zone for the per-player breakdown
      </div>
    </div>
  );
}

function ZoneTooltip({
  zone,
  zoneAgg,
  topContributors,
}: {
  zone: ZoneKey;
  zoneAgg: (z: ZoneKey) => { att: number; made: number; share: number; fgPct: number | null };
  topContributors: (z: ZoneKey) => Array<{ player: RosterEntry; att: number; made: number }>;
}) {
  const agg = zoneAgg(zone);
  const contributors = topContributors(zone);
  const shareLabel =
    zone === 'ft' ? `${(agg.share * 100).toFixed(1)}% FT rate` : `${(agg.share * 100).toFixed(1)}% of FGA`;
  return (
    <>
      <div className="flex items-baseline justify-between gap-2 mb-0.5">
        <span className="text-xs font-bold text-gray-100">
          {ZONE_LABELS[zone]}
        </span>
        <span className="text-[11px] text-gray-200 tabular-nums font-semibold">
          {shareLabel}
        </span>
      </div>
      <div className="text-[11px] text-gray-300 tabular-nums">
        {Math.round(agg.made)} / {Math.round(agg.att)}
        {agg.fgPct != null && ` · ${agg.fgPct.toFixed(1)}% FG`}
      </div>
      {contributors.length > 0 && (
        <div className="mt-1.5 pt-1.5 border-t border-gray-800">
          <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-1">
            Top contributors
          </div>
          {contributors.map((c) => {
            const pct = c.att > 0 ? (c.made / c.att) * 100 : null;
            return (
              <div
                key={c.player.player_id}
                className="flex items-baseline justify-between gap-2 text-[11px]"
              >
                <span className="text-gray-200 truncate flex-1">
                  {c.player.name}
                </span>
                <span className="text-gray-500 tabular-nums">
                  {Math.round(c.made)}/{Math.round(c.att)}
                  {pct != null && ` · ${pct.toFixed(0)}%`}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </>
  );
}
