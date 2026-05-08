import { useEffect, useMemo, useRef, useState } from 'react';
import {
  fetchPrediction,
  fetchTeamRankings,
  type FeatureContribution,
  type PredictionResult,
  type TeamRanking,
  type Venue,
} from '../api/client';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';
import { InfoIcon, InfoTooltip } from '../components/InfoTooltip';
import { FEATURE_EXPLANATIONS, GROUP_EXPLANATIONS } from '../components/featureExplanations';

const TEAM_1_COLOR = '#3b82f6'; // blue (matches PlayerCompare PLAYER_COLORS[0])
const TEAM_2_COLOR = '#ef4444'; // red

export default function Predict() {
  const { season } = useSeason();
  usePageTitle('Game Prediction');
  const [team1, setTeam1] = useState('');
  const [team2, setTeam2] = useState('');
  const [venue, setVenue] = useState<Venue>('home');
  const [result, setResult] = useState<PredictionResult | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  // Pull the team list once for autocomplete on both pickers. Rankings is
  // already keyed by season and lightweight (~360 rows for D-I), so client-
  // side filtering is simpler than a per-keystroke server search.
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  useEffect(() => {
    let alive = true;
    fetchTeamRankings(season)
      .then((r) => {
        if (alive) setTeams(r.teams);
      })
      .catch(() => {
        // Picker still works; fallback to free-text typing.
      });
    return () => {
      alive = false;
    };
  }, [season]);

  const handleSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!team1.trim() || !team2.trim()) return;
    setLoading(true);
    setError('');
    setResult(null);
    try {
      const r = await fetchPrediction(team1.trim(), team2.trim(), venue, season);
      setResult(r);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Prediction failed');
    } finally {
      setLoading(false);
    }
  };

  const team1Prob = result ? result.home_win_probability * 100 : 50;

  const venueLabel: Record<Venue, string> = {
    home: team1.trim() ? `${team1.trim()} home` : 'Team 1 home',
    neutral: 'Neutral',
    away: team2.trim() ? `${team2.trim()} home` : 'Team 2 home',
  };

  return (
    <div className="max-w-3xl mx-auto">
      <h1 className="text-2xl font-bold mb-1">Game Prediction</h1>
      <p className="text-xs text-gray-500 mb-5">
        Predicting matchups in the{' '}
        <span className="text-gray-300">
          {season - 1}-{String(season).slice(2)}
        </span>{' '}
        season. Switch the season selector in the nav to back-test historical games.
      </p>

      <form onSubmit={handleSubmit} className="bg-gray-800 rounded-lg p-6 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <TeamPicker
            label="Team 1"
            value={team1}
            onChange={setTeam1}
            teams={teams}
            placeholder="e.g. Duke"
            color={TEAM_1_COLOR}
          />
          <TeamPicker
            label="Team 2"
            value={team2}
            onChange={setTeam2}
            teams={teams}
            placeholder="e.g. Michigan"
            color={TEAM_2_COLOR}
          />
        </div>

        <div>
          <label className="block text-sm text-gray-400 mb-1.5">Venue</label>
          <div
            className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-sm w-full sm:w-auto"
            role="radiogroup"
            aria-label="Game venue"
          >
            {(['home', 'neutral', 'away'] as const).map((v) => (
              <button
                key={v}
                type="button"
                role="radio"
                aria-checked={venue === v}
                onClick={() => setVenue(v)}
                className={`flex-1 sm:flex-none px-3 py-1.5 ${
                  venue === v
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-900 text-gray-300 hover:bg-gray-700'
                }`}
              >
                {venueLabel[v]}
              </button>
            ))}
          </div>
        </div>

        <button
          type="submit"
          disabled={loading || !team1.trim() || !team2.trim()}
          className="w-full bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white font-medium py-2.5 rounded transition-colors"
        >
          {loading ? 'Predicting...' : 'Predict'}
        </button>
      </form>

      {error && (
        <div className="mt-4 bg-red-900/50 border border-red-800 rounded-lg p-4 text-red-300">
          {error}
        </div>
      )}

      {result && (
        <div className="mt-6 space-y-4">
          <ResultHeadline result={result} team1Prob={team1Prob} />
          <KeysToGame result={result} />
          <SideBySideStats result={result} teams={teams} />
          <FourFactorsPanel result={result} teams={teams} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Keys to the Game
// ---------------------------------------------------------------------------

interface Key {
  group: string;
  team: string;
  towardHome: boolean;
  contribution: number;
  tier: 'slight' | 'clear' | 'meaningful' | 'decisive';
  headline?: FeatureContribution;
}

const TIER_PHRASE: Record<Key['tier'], string> = {
  slight: 'has a slight nudge',
  clear: 'holds an edge',
  meaningful: 'has a meaningful advantage',
  decisive: 'dominates',
};

const TIER_BADGE: Record<Key['tier'], string> = {
  slight: 'bg-gray-700/60 text-gray-300',
  clear: 'bg-blue-900/40 text-blue-200 ring-1 ring-blue-500/40',
  meaningful: 'bg-amber-900/40 text-amber-200 ring-1 ring-amber-500/50',
  decisive: 'bg-rose-900/50 text-rose-200 ring-1 ring-rose-500/60',
};

function tierFor(mag: number): Key['tier'] | null {
  if (mag < 0.3) return null;
  if (mag < 0.6) return 'slight';
  if (mag < 1.2) return 'clear';
  if (mag < 2.0) return 'meaningful';
  return 'decisive';
}

function generateKeys(result: PredictionResult): Key[] {
  const keys: Key[] = [];
  for (const g of result.contributions_by_group) {
    const tier = tierFor(Math.abs(g.contribution));
    if (!tier) continue;
    const towardHome = g.contribution > 0;
    const headline = result.top_contributors
      .filter((c) => c.group === g.group)
      .sort((a, b) => Math.abs(b.contribution) - Math.abs(a.contribution))[0];
    keys.push({
      group: g.group,
      team: towardHome ? result.home_team : result.away_team,
      towardHome,
      contribution: g.contribution,
      tier,
      headline,
    });
  }
  keys.sort((a, b) => Math.abs(b.contribution) - Math.abs(a.contribution));
  return keys.slice(0, 4);
}

function KeysToGame({ result }: { result: PredictionResult }) {
  const keys = useMemo(() => generateKeys(result), [result]);

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Keys to the Game
        </h2>
        <div className="text-[11px] text-gray-500">Synthesized from the model</div>
      </div>
      {keys.length === 0 ? (
        <p className="text-sm text-gray-400">
          The model sees this as an even matchup with no clear advantages on either side.
        </p>
      ) : (
        <ul className="space-y-3">
          {keys.map((k) => (
            <KeyItem key={k.group} k={k} />
          ))}
        </ul>
      )}
    </div>
  );
}

function KeyItem({ k }: { k: Key }) {
  const teamColor = k.towardHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  const groupExplanation = GROUP_EXPLANATIONS[k.group];
  const headlineExplanation = k.headline ? FEATURE_EXPLANATIONS[k.headline.name] : undefined;
  const sign = k.contribution > 0 ? '+' : '';

  return (
    <li className="flex items-start gap-3">
      <div className="flex-shrink-0 w-1 self-stretch rounded-full" style={{ backgroundColor: teamColor }} />
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline justify-between gap-2 flex-wrap">
          <div className="text-sm">
            {groupExplanation ? (
              <InfoTooltip title={k.group} body={groupExplanation}>
                <span className="text-gray-200 font-medium hover:text-white transition-colors">
                  {k.group}
                </span>
              </InfoTooltip>
            ) : (
              <span className="text-gray-200 font-medium">{k.group}</span>
            )}
            <span className="text-gray-500">: </span>
            <span style={{ color: teamColor }} className="font-medium">
              {k.team}
            </span>{' '}
            <span className="text-gray-300">{TIER_PHRASE[k.tier]}</span>
          </div>
          <span
            className={`text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded ${TIER_BADGE[k.tier]}`}
          >
            {k.tier} · {sign}
            {k.contribution.toFixed(1)}
          </span>
        </div>
        {k.headline && (
          <div className="text-xs text-gray-400 mt-1 leading-snug">
            Headlined by{' '}
            {headlineExplanation ? (
              <InfoTooltip title={k.headline.label} body={headlineExplanation}>
                <span className="text-gray-300 hover:text-white transition-colors">
                  {k.headline.label}
                </span>
              </InfoTooltip>
            ) : (
              <span className="text-gray-300">{k.headline.label}</span>
            )}{' '}
            (gap of {k.headline.value > 0 ? '+' : ''}
            {k.headline.value.toFixed(2)}, worth{' '}
            <span style={{ color: teamColor }}>
              {sign}
              {k.headline.contribution.toFixed(1)}
            </span>{' '}
            on the spread)
          </div>
        )}
      </div>
    </li>
  );
}

// ---------------------------------------------------------------------------
// Side-by-side stats — uses the rankings data we already fetched for the picker
// ---------------------------------------------------------------------------

function lookupTeam(name: string, teams: TeamRanking[]): TeamRanking | undefined {
  return teams.find((t) => t.name === name);
}

interface StatRow {
  label: string;
  home: number | string | null;
  away: number | string | null;
  /// `'high'` means higher is better, `'low'` lower is better, `'neither'` no
  /// directional bias (e.g. tempo), and `'record'` is a special case for W-L
  /// where we compare win pct.
  better: 'high' | 'low' | 'neither' | 'record';
  /// Used when comparing W-L records (we sort by win pct).
  homeNum?: number;
  awayNum?: number;
  format?: (v: number) => string;
  tooltip?: string;
}

function SideBySideStats({
  result,
  teams,
}: {
  result: PredictionResult;
  teams: TeamRanking[];
}) {
  const home = lookupTeam(result.home_team, teams);
  const away = lookupTeam(result.away_team, teams);
  if (!home || !away) return null;

  const fmt1 = (v: number) => (v > 0 ? '+' : '') + v.toFixed(1);
  const fmt0 = (v: number) => Math.round(v).toString();

  const winPct = (t: TeamRanking) => {
    const total = t.wins + t.losses;
    return total > 0 ? t.wins / total : 0.5;
  };

  const rows: StatRow[] = [
    {
      label: 'Record',
      home: `${home.wins}-${home.losses}`,
      away: `${away.wins}-${away.losses}`,
      homeNum: winPct(home),
      awayNum: winPct(away),
      better: 'record',
    },
    {
      label: 'AdjEM',
      home: home.adj_efficiency_margin,
      away: away.adj_efficiency_margin,
      better: 'high',
      format: fmt1,
      tooltip:
        'Adjusted efficiency margin: net points per 100 possessions, opponent-adjusted. The headline KenPom-style rating.',
    },
    {
      label: 'AdjO',
      home: home.adj_offense,
      away: away.adj_offense,
      better: 'high',
      format: (v) => v.toFixed(1),
      tooltip: 'Adjusted offensive efficiency — points scored per 100 possessions, opponent-adjusted.',
    },
    {
      label: 'AdjD',
      home: home.adj_defense,
      away: away.adj_defense,
      better: 'low',
      format: (v) => v.toFixed(1),
      tooltip: 'Adjusted defensive efficiency — points allowed per 100 possessions, opponent-adjusted (lower = better).',
    },
    {
      label: 'Tempo',
      home: home.adj_tempo,
      away: away.adj_tempo,
      better: 'neither',
      format: (v) => v.toFixed(1),
      tooltip: 'Adjusted possessions per 40 minutes. Higher = faster game; not inherently good or bad.',
    },
    {
      label: 'SOS',
      home: home.sos,
      away: away.sos,
      better: 'high',
      format: fmt1,
      tooltip: 'Strength of schedule from team-level adjusted efficiencies of opponents faced.',
    },
    {
      label: 'ELO',
      home: home.elo_rating,
      away: away.elo_rating,
      better: 'high',
      format: fmt0,
      tooltip: 'NatStat ELO rating — head-to-head + margin-of-victory power rating.',
    },
  ];

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Side by Side
        </h2>
      </div>
      <div className="space-y-1.5">
        {/* Header */}
        <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 pb-2 border-b border-gray-700">
          <div className="text-right text-sm font-medium" style={{ color: TEAM_1_COLOR }}>
            {home.name}
          </div>
          <div className="w-20 text-center text-[11px] uppercase tracking-wide text-gray-500">
            stat
          </div>
          <div className="text-left text-sm font-medium" style={{ color: TEAM_2_COLOR }}>
            {away.name}
          </div>
        </div>
        {rows.map((r) => (
          <StatComparisonRow key={r.label} row={r} />
        ))}
      </div>
    </div>
  );
}

function StatComparisonRow({ row }: { row: StatRow }) {
  const homeBetter = computeWinner(row) === 'home';
  const awayBetter = computeWinner(row) === 'away';
  const fmt = row.format ?? ((v: number) => v.toFixed(1));

  const renderValue = (v: number | string | null, better: boolean, color: string) => {
    if (v == null) return <span className="text-gray-500">—</span>;
    const text = typeof v === 'number' ? fmt(v) : v;
    return (
      <span className={better ? 'font-semibold' : 'text-gray-400'} style={better ? { color } : {}}>
        {text}
      </span>
    );
  };

  return (
    <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 text-sm">
      <div className="text-right">
        {renderValue(row.home, homeBetter, TEAM_1_COLOR)}
      </div>
      <div className="w-20 text-center text-[11px] text-gray-500 uppercase tracking-wide">
        {row.tooltip ? (
          <InfoTooltip title={row.label} body={row.tooltip}>
            <span className="hover:text-gray-300 transition-colors">{row.label}</span>
            <span className="ml-1">
              <InfoIcon />
            </span>
          </InfoTooltip>
        ) : (
          row.label
        )}
      </div>
      <div className="text-left">{renderValue(row.away, awayBetter, TEAM_2_COLOR)}</div>
    </div>
  );
}

function computeWinner(row: StatRow): 'home' | 'away' | null {
  const h = row.better === 'record' ? row.homeNum : (row.home as number | null);
  const a = row.better === 'record' ? row.awayNum : (row.away as number | null);
  if (h == null || a == null || row.better === 'neither') return null;
  if (h === a) return null;
  if (row.better === 'low') return h < a ? 'home' : 'away';
  return h > a ? 'home' : 'away';
}

// ---------------------------------------------------------------------------
// Four factors — split into two panels, one per "side of the ball". Each row
// is a single tug-of-war bar showing the matchup-specific advantage between
// the offensive team and the defensive team for that factor.
// ---------------------------------------------------------------------------

/// Approximate D-I averages — used to decompose each side's strength
/// (deviation from league average) so per-matchup advantages compose
/// cleanly. Tuned by inspection of recent seasons; precision matters less
/// than relative ordering for the visual.
const LEAGUE_AVG = {
  EFG: 0.50,
  TOV: 0.17,
  ORB: 0.30, // offensive rebound rate
  DRB: 0.70, // defensive rebound rate (1 - opponent ORB)
  FT_RATE: 0.30,
} as const;

interface MatchupRow {
  label: string;
  tooltip: string;
  /// Offensive team's value for this factor (e.g. team1's eFG% on offense).
  offValue: number | null;
  /// Defensive team's allowed/forced value (e.g. team2's opp_eFG% allowed).
  defValue: number | null;
  /// Net advantage in factor units. Positive = offense wins, negative =
  /// defense wins. Computed from off_strength − def_strength so each side's
  /// deviation from league average composes correctly.
  advantage: number | null;
  /// Display formatter for raw values (e.g. "55.0%" for percentages).
  formatRaw: (v: number) => string;
  /// Display formatter for the advantage chip (e.g. "+5.2pp").
  formatAdvantage: (v: number) => string;
  /// Bar cap (in advantage units) at which the tug-of-war fills one half.
  /// Tuned per factor so a decisive matchup edge fills the bar.
  barCap: number;
}

function FourFactorsPanel({
  result,
  teams,
}: {
  result: PredictionResult;
  teams: TeamRanking[];
}) {
  const home = lookupTeam(result.home_team, teams);
  const away = lookupTeam(result.away_team, teams);
  if (!home || !away) return null;

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <div className="flex items-baseline justify-between mb-4">
        <h2 className="text-sm font-semibold text-gray-200 uppercase tracking-wide">
          Four Factors
        </h2>
        <div className="text-[11px] text-gray-500">Per-matchup advantage</div>
      </div>
      <div className="space-y-6">
        <MatchupSubpanel
          offTeam={home}
          defTeam={away}
          offColor={TEAM_1_COLOR}
          defColor={TEAM_2_COLOR}
        />
        <MatchupSubpanel
          offTeam={away}
          defTeam={home}
          offColor={TEAM_2_COLOR}
          defColor={TEAM_1_COLOR}
        />
      </div>
    </div>
  );
}

function MatchupSubpanel({
  offTeam,
  defTeam,
  offColor,
  defColor,
}: {
  offTeam: TeamRanking;
  defTeam: TeamRanking;
  offColor: string;
  defColor: string;
}) {
  const fmtPp = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(1)}pp`;
  const fmtRatio = (v: number) => `${v >= 0 ? '+' : ''}${v.toFixed(3)}`;
  const pctStr = (v: number) => `${(v * 100).toFixed(1)}%`;
  const ratioStr = (v: number) => v.toFixed(3);

  // Each factor's advantage formula is:
  //   off_strength = (off_value − league_avg) signed in offense's favor
  //   def_strength = (def_value − league_avg) signed in defense's favor
  //   advantage = off_strength − def_strength
  // Signs are baked into the formulas below so each one resolves to a
  // single signed scalar where positive = offense wins.
  const rows: MatchupRow[] = [
    {
      label: 'eFG%',
      tooltip:
        'Effective FG% advantage: how much better the offense shoots, relative to what the defense usually allows. Positive = offense wins this matchup.',
      offValue: offTeam.effective_fg_pct,
      defValue: defTeam.opp_effective_fg_pct,
      advantage:
        offTeam.effective_fg_pct == null || defTeam.opp_effective_fg_pct == null
          ? null
          : (offTeam.effective_fg_pct + defTeam.opp_effective_fg_pct - 2 * LEAGUE_AVG.EFG) * 100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 8,
    },
    {
      label: 'TOV%',
      tooltip:
        'Turnover advantage: how much the offense protects the ball relative to how often the defense forces turnovers. Positive = offense wins (commits fewer than defense forces on average).',
      offValue: offTeam.turnover_pct,
      defValue: defTeam.opp_turnover_pct,
      advantage:
        offTeam.turnover_pct == null || defTeam.opp_turnover_pct == null
          ? null
          : (2 * LEAGUE_AVG.TOV - offTeam.turnover_pct - defTeam.opp_turnover_pct) * 100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 5,
    },
    {
      label: 'Rebounding',
      tooltip:
        'Rebounding advantage on the offensive boards: the offense\'s ORB% above league average, less the defense\'s DRB% above league average. Positive = offense wins extra possessions.',
      offValue: offTeam.off_rebound_pct,
      defValue: defTeam.def_rebound_pct,
      advantage:
        offTeam.off_rebound_pct == null || defTeam.def_rebound_pct == null
          ? null
          : (offTeam.off_rebound_pct - LEAGUE_AVG.ORB - (defTeam.def_rebound_pct - LEAGUE_AVG.DRB)) *
            100,
      formatRaw: pctStr,
      formatAdvantage: fmtPp,
      barCap: 8,
    },
    {
      label: 'FT Rate',
      tooltip:
        'Free throw rate advantage: how much more often the offense gets to the line, relative to what the defense usually allows. Positive = offense wins.',
      offValue: offTeam.ft_rate,
      defValue: defTeam.opp_ft_rate,
      advantage:
        offTeam.ft_rate == null || defTeam.opp_ft_rate == null
          ? null
          : offTeam.ft_rate + defTeam.opp_ft_rate - 2 * LEAGUE_AVG.FT_RATE,
      formatRaw: ratioStr,
      formatAdvantage: fmtRatio,
      barCap: 0.06,
    },
  ];

  return (
    <div>
      <div className="text-xs uppercase tracking-wide text-gray-500 mb-3">
        When <span style={{ color: offColor }}>{offTeam.name}</span> has the ball
      </div>
      <div className="space-y-3">
        {rows.map((r) => (
          <MatchupRowView
            key={r.label}
            row={r}
            offTeam={offTeam.name}
            defTeam={defTeam.name}
            offColor={offColor}
            defColor={defColor}
          />
        ))}
      </div>
    </div>
  );
}

function MatchupRowView({
  row,
  offTeam,
  defTeam,
  offColor,
  defColor,
}: {
  row: MatchupRow;
  offTeam: string;
  defTeam: string;
  offColor: string;
  defColor: string;
}) {
  if (row.advantage == null || row.offValue == null || row.defValue == null) {
    return <div className="text-xs text-gray-500">{row.label}: —</div>;
  }

  const offWins = row.advantage > 0;
  const winnerColor = offWins ? offColor : defColor;
  const winnerName = offWins ? offTeam : defTeam;
  // Cap at 50 because the bar represents one half of the container (the
  // half-width on either side of the centerline). `Math.min(50, …)` keeps
  // the bar from extending past its container edge on extreme matchups.
  const barPct = Math.min(50, (Math.abs(row.advantage) / row.barCap) * 50);

  return (
    <div>
      {/* Factor label */}
      <div className="text-sm mb-1.5">
        <InfoTooltip title={row.label} body={row.tooltip}>
          <span className="text-gray-200 font-medium hover:text-white transition-colors">
            {row.label}
          </span>
          <span className="ml-1">
            <InfoIcon />
          </span>
        </InfoTooltip>
      </div>

      {/* Centered winner chip above the bar */}
      <div className="grid grid-cols-[80px_1fr_80px] gap-3 mb-0.5">
        <div />
        <div className="text-center text-xs">
          <span style={{ color: winnerColor }} className="font-semibold">
            {winnerName} {row.formatAdvantage(Math.abs(row.advantage))}
          </span>
        </div>
        <div />
      </div>

      {/* Raw offense / defense values flanking the tug-of-war bar */}
      <div className="grid grid-cols-[80px_1fr_80px] items-center gap-3 text-xs">
        <div className="text-right">
          <span style={{ color: offColor }}>{row.formatRaw(row.offValue)}</span>
        </div>
        <div className="relative h-3 bg-gray-900 rounded">
          <div className="absolute inset-y-0 left-1/2 w-px bg-gray-700" />
          <div
            className="absolute inset-y-0 rounded"
            style={
              offWins
                ? { left: `${50 - barPct}%`, width: `${barPct}%`, backgroundColor: offColor }
                : { left: '50%', width: `${barPct}%`, backgroundColor: defColor }
            }
          />
        </div>
        <div className="text-left">
          <span style={{ color: defColor }}>{row.formatRaw(row.defValue)}</span>
        </div>
      </div>

      {/* Tiny under-line clarifying which side is which */}
      <div className="grid grid-cols-2 gap-3 mt-0.5 text-[10px] text-gray-500">
        <div className="text-right">offense</div>
        <div className="text-left">defense</div>
      </div>
    </div>
  );
}

function ResultHeadline({
  result,
  team1Prob,
}: {
  result: PredictionResult;
  team1Prob: number;
}) {
  const margin = result.predicted_margin;
  const winnerIsHome = margin > 0;
  const winnerColor = winnerIsHome ? TEAM_1_COLOR : TEAM_2_COLOR;
  // Display the spread from the *winner's* perspective, KenPom-style:
  // "Duke -3.5" reads naturally regardless of which team was passed first.
  const winnerSpread = -Math.abs(margin);

  const venueText =
    result.venue === 'neutral'
      ? 'Neutral site'
      : result.venue === 'home'
        ? `at ${result.home_team}`
        : `at ${result.away_team}`;

  return (
    <div className="bg-gray-800 rounded-lg p-6 space-y-5">
      <div className="text-center">
        <div className="text-xs text-gray-500 uppercase tracking-wide mb-2">{venueText}</div>
        <div className="text-3xl font-bold" style={{ color: winnerColor }}>
          {result.predicted_winner}{' '}
          <span className="text-2xl text-gray-300 font-semibold">
            {winnerSpread.toFixed(1)}
          </span>
        </div>
        <div className="text-sm text-gray-400 mt-1">
          {(Math.max(result.home_win_probability, 1 - result.home_win_probability) * 100).toFixed(0)}
          % win probability
        </div>
      </div>

      {/* Probability bar */}
      <div>
        <div className="flex justify-between text-sm mb-1">
          <span className={winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'}>
            {result.home_team}
          </span>
          <span className={!winnerIsHome ? 'text-gray-200 font-medium' : 'text-gray-400'}>
            {result.away_team}
          </span>
        </div>
        <div className="flex h-7 rounded-full overflow-hidden ring-1 ring-gray-700">
          <div
            className="flex items-center justify-center text-xs font-medium text-white transition-[width]"
            style={{ width: `${team1Prob}%`, backgroundColor: TEAM_1_COLOR }}
          >
            {team1Prob >= 12 ? `${team1Prob.toFixed(0)}%` : ''}
          </div>
          <div
            className="flex items-center justify-center text-xs font-medium text-white transition-[width]"
            style={{ width: `${100 - team1Prob}%`, backgroundColor: TEAM_2_COLOR }}
          >
            {100 - team1Prob >= 12 ? `${(100 - team1Prob).toFixed(0)}%` : ''}
          </div>
        </div>
      </div>
    </div>
  );
}

function TeamPicker({
  label,
  value,
  onChange,
  teams,
  placeholder,
  color,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  teams: TeamRanking[];
  placeholder: string;
  color: string;
}) {
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const filtered = useMemo(() => {
    const q = value.trim().toLowerCase();
    if (q.length === 0) return [];
    return teams
      .filter(
        (t) =>
          t.name.toLowerCase().includes(q) ||
          (t.conference?.toLowerCase().includes(q) ?? false),
      )
      .slice(0, 10);
  }, [teams, value]);

  return (
    <div className="relative">
      <label className="block text-sm text-gray-400 mb-1">
        <span style={{ color }} className="font-medium">
          ●
        </span>{' '}
        {label}
      </label>
      <input
        ref={inputRef}
        type="text"
        value={value}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        placeholder={placeholder}
        className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-2 text-white placeholder-gray-500 focus:outline-none focus:border-blue-500"
        autoComplete="off"
      />
      {open && filtered.length > 0 && (
        <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-700 rounded shadow-lg">
          {filtered.map((t) => (
            <button
              key={t.team_id}
              type="button"
              onMouseDown={(e) => {
                e.preventDefault();
                onChange(t.name);
                setOpen(false);
                inputRef.current?.blur();
              }}
              className="w-full text-left px-3 py-2 hover:bg-gray-800 text-sm flex items-center justify-between gap-3"
            >
              <span className="truncate">{t.name}</span>
              <span className="text-xs text-gray-500 truncate">
                {t.conference ?? '—'}
                {t.adj_efficiency_margin != null && (
                  <>
                    {' · '}
                    {t.adj_efficiency_margin > 0 ? '+' : ''}
                    {t.adj_efficiency_margin.toFixed(1)}
                  </>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
