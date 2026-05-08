// 8-axis player radar. Clockwise order roughly groups offense (top half) and
// defense / physicality (bottom half).
//
// `resolveAxes` takes a player snapshot and returns one entry per axis with
// the spoke length, percentile, raw display value, and a plain-English blurb.
// Tooltip and methodology surfaces consume this; the underlying source
// (cstat vs Torvik fallbacks) is intentionally hidden — the user doesn't
// need to care which compute pipeline produced the percentile.

import type {
  Percentiles,
  PlayerSeasonStats,
  TorkvikStats,
} from '../api/client';

export interface PlayerSnapshot {
  season_stats: PlayerSeasonStats | null;
  percentiles: Percentiles | null;
  torvik_stats: TorkvikStats | null;
}

export interface ResolvedAxis {
  /** Axis label rendered as the spoke's outer tick. */
  stat: string;
  /** One-line plain-English description shown in the tooltip. */
  blurb: string;
  /** 0–100 — the value rendered as the spoke length. */
  value: number;
  /** Raw display value (e.g. "18.4 PPG", "37.2% on 5.1/g"). Null if absent. */
  rawValue: string | null;
  /** 0–99 integer percentile for the tooltip. Null if absent. */
  percentile: number | null;
}

// Display formatters. Match the convention in `format.ts` but tack on a unit
// since these strings are presented standalone in the tooltip.
const fracPctStr = (v: number | null | undefined) =>
  v != null ? `${(v * 100).toFixed(1)}%` : null;
const pointPctStr = (v: number | null | undefined) =>
  v != null ? `${v.toFixed(1)}%` : null;

const clamp01 = (v: number) => Math.max(0, Math.min(1, v));

export function resolveAxes(p: PlayerSnapshot): ResolvedAxis[] {
  const ss = p.season_stats;
  const pc = p.percentiles;
  const tv = p.torvik_stats;

  // 3-Point — volume-gated. Pure 3P% percentile spikes for players who hit a
  // rare attempt at high accuracy; gating by 3PA/G shrinks low-volume guys
  // toward the 30th percentile so the axis reads honestly as both volume +
  // accuracy. Full credit at 2+ attempts/game.
  const tpaPerGame =
    tv?.tpa != null && ss?.games_played && ss.games_played > 0
      ? tv.tpa / ss.games_played
      : null;
  const volWeight = tpaPerGame != null ? clamp01(tpaPerGame / 2) : 0;
  const tpRawPct = pc?.tp_pct_pct ?? null;
  const threePtPct =
    tpRawPct != null ? tpRawPct * volWeight + 0.3 * (1 - volWeight) : null;
  const threePtRaw = (() => {
    const acc = fracPctStr(ss?.tp_pct);
    const vol = tpaPerGame != null ? `${tpaPerGame.toFixed(1)}/g` : null;
    if (acc && vol) return `${acc} on ${vol}`;
    return acc ?? vol ?? null;
  })();

  // Rebounding — average of OREB% + DREB% percentiles. If only one is
  // present, use it directly.
  const orbP = pc?.orb_pct_pct ?? null;
  const drbP = pc?.drb_pct_pct ?? null;
  const rebPct =
    orbP != null && drbP != null ? (orbP + drbP) / 2 : (orbP ?? drbP);
  const rebRaw = (() => {
    const o = ss?.orb_pct;
    const d = ss?.drb_pct;
    if (o == null && d == null) return null;
    return `${o != null ? o.toFixed(1) : '—'}% OR / ${
      d != null ? d.toFixed(1) : '—'
    }% DR`;
  })();

  // Playmaking — AST% preferred, APG fallback.
  const playmakingPct = pc?.ast_pct_pct ?? pc?.apg_pct ?? null;
  const playmakingRaw =
    fracPctStr(ss?.ast_pct) ??
    (ss?.apg != null ? `${ss.apg.toFixed(1)} APG` : null);

  const axes: Array<{
    stat: string;
    blurb: string;
    pct: number | null;
    raw: string | null;
  }> = [
    {
      stat: 'Scoring',
      blurb: 'Total point production per game.',
      pct: pc?.ppg_pct ?? null,
      raw: ss?.ppg != null ? `${ss.ppg.toFixed(1)} PPG` : null,
    },
    {
      stat: 'Efficiency',
      blurb: 'Points per shooting possession (true shooting %).',
      pct: pc?.true_shooting_pct_pct ?? null,
      raw: fracPctStr(ss?.true_shooting_pct),
    },
    {
      stat: '3-Point',
      blurb: 'Volume and accuracy from beyond the arc.',
      pct: threePtPct,
      raw: threePtRaw,
    },
    {
      stat: 'Playmaking',
      blurb: 'How often this player creates baskets for teammates.',
      pct: playmakingPct,
      raw: playmakingRaw,
    },
    {
      stat: 'Free Throws',
      blurb: 'Drawing fouls by attacking the basket.',
      pct: pc?.ft_rate_pct ?? null,
      raw: ss?.ft_rate != null ? `${ss.ft_rate.toFixed(2)} FT rate` : null,
    },
    {
      stat: 'Rebounding',
      blurb: 'Controlling missed shots on both ends.',
      pct: rebPct,
      raw: rebRaw,
    },
    {
      stat: 'Defense',
      blurb:
        'Overall defensive impact — help defense, rotations, on-ball pressure. Captures the bulk of defensive value beyond just steals and blocks.',
      pct: tv?.dgbpm_pct ?? null,
      raw:
        tv?.dgbpm != null
          ? `${tv.dgbpm >= 0 ? '+' : ''}${tv.dgbpm.toFixed(1)} defensive impact`
          : null,
    },
    {
      stat: 'Blocks',
      blurb: 'Rim protection.',
      pct: pc?.blk_pct_pct ?? null,
      raw: pointPctStr(ss?.blk_pct),
    },
  ];

  return axes.map((a) => ({
    stat: a.stat,
    blurb: a.blurb,
    value: a.pct != null ? clamp01(a.pct) * 100 : 0,
    rawValue: a.raw,
    percentile: a.pct != null ? Math.round(clamp01(a.pct) * 100) : null,
  }));
}
