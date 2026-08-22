import { useEffect, useMemo, useState } from 'react';
import { AgGridReact } from 'ag-grid-react';
import type { ColDef } from 'ag-grid-community';
import { fetchTeamRankings, type TeamRanking } from '../api/client';
import { conferenceLabel, conferenceSearchText } from '../lib/conferences';
import { gridTheme } from '../theme';
import { TableToolbar, TableSearchInput } from '../components/TableToolbar';
import { ScoreTicker } from '../components/ScoreTicker';
import { pctileTextColor } from '../components/pctile';
import { useSeason } from '../components/season';
import { SeasonLink } from '../components/SeasonLink';
import { usePageTitle } from '../components/usePageTitle';
import { useIsMobile } from '../components/useIsMobile';
import {
  BAND_CHIP_CLASS,
  BAND_CHIP_TOP_STRONG,
  BAND_EMPTY_CHIP_CLASS,
} from '../components/scale';

// AdjEM presentation tiers. Colored from the shared site scale (`scale.ts`)
// so a team's chip and a player's CAM chip mean the same thing at a glance —
// they used to run on separate palettes. Thresholds use the conventional
// absolute scale where +20 is roughly Final Four caliber and 0 is the
// league-average D-I team.
type AdjEmTier =
  | 'Elite'
  | 'Strong'
  | 'Above average'
  | 'Average'
  | 'Below average'
  | 'Weak';

function adjEmTier(em: number | null | undefined): AdjEmTier | null {
  if (em == null) return null;
  if (em >= 25) return 'Elite';
  if (em >= 15) return 'Strong';
  if (em >= 5) return 'Above average';
  if (em >= -5) return 'Average';
  if (em >= -15) return 'Below average';
  return 'Weak';
}

// Six tiers over the five bands, mirroring how `camTierColor` handles the same
// shape: the bottom five map one-to-one (red → green) and the top tier is set
// apart by emphasis — a solid fill instead of a tint — rather than a sixth hue.
const ADJ_EM_TIER_BAND: Record<AdjEmTier, number> = {
  Weak: 0,
  'Below average': 1,
  Average: 2,
  'Above average': 3,
  Strong: 4,
  Elite: 4,
};

function adjEmTierColor(tier: AdjEmTier | null): string {
  if (tier == null) return BAND_EMPTY_CHIP_CLASS;
  // ~8% of teams clear +25, so this takes the STRONG tint rather than the solid
  // fill CAM gets — a solid block would cover this board's whole first screen.
  if (tier === 'Elite') return BAND_CHIP_TOP_STRONG;
  return BAND_CHIP_CLASS[ADJ_EM_TIER_BAND[tier]];
}

const fmt = (v: number | null, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null) => (v != null ? (v * 100).toFixed(1) : '—');

/** Cell renderer that shows a formatted value with a subtle rank underneath.
 *  The rank text is tinted by percentile when `totalTeams` is provided —
 *  small enough to stay subtle, but gives an at-a-glance cue alongside the
 *  precise rank number. AdjEM gets its own chip treatment (see
 *  `adjEmTierColor`); this helper is for the supporting columns. */
function RankedCell({
  value,
  rank,
  format,
  totalTeams,
}: {
  value: number | null;
  rank: number | null;
  format: (v: number | null) => string;
  totalTeams?: number;
}) {
  const pctile =
    rank != null && totalTeams && totalTeams > 1
      ? 1 - (rank - 1) / (totalTeams - 1)
      : null;
  const rankColor = pctile != null ? pctileTextColor(pctile) : '#6b7280';
  return (
    <div className="leading-tight py-0.5">
      <div>{format(value)}</div>
      {rank != null && (
        <div className="text-[10px]" style={{ color: rankColor }}>
          #{rank}
        </div>
      )}
    </div>
  );
}

type RankingsView = 'standard' | 'offense' | 'defense';

// Visual divider before the four-factors block — matches the roster table's
// `border-l border-gray-800` category separator. Applied via inline style so
// it survives AG Grid's themed cell borders.
const CATEGORY_DIVIDER_STYLE = { borderLeft: '1px solid rgb(31 41 55)' } as const;

function buildColumns(
  totalTeams: number,
  view: RankingsView,
  isMobile: boolean,
): ColDef<TeamRanking>[] {
  // Column sizing strategy differs by viewport:
  //
  // - Desktop: flex-distributed so columns expand to fill the container. AG
  //   Grid normalizes flex weights, so we pass natural-width values directly
  //   (flex=200 gets 2.5× the share of flex=80). minWidth is ~20px below
  //   natural with a 65px floor.
  // - Mobile: fixed natural width. Container is narrower than the sum of
  //   columns, so AG Grid horizontal-scrolls — cleaner than compressing
  //   columns to a sub-natural minWidth and clipping headers/values.
  const flexCol = (w: number, min?: number) =>
    isMobile
      ? { width: w }
      : { flex: w, minWidth: min ?? Math.max(65, w - 20) };

  const base: ColDef<TeamRanking>[] = [
    // Pinned identity columns stay at fixed width (don't flex with the
    // content area; AG Grid recommends fixed widths for pinned cols).
    // Explicit `wrapHeaderText: false` so a tall header row caused by another
    // column doesn't visually push "Rk" into a multi-line layout.
    {
      field: 'rank',
      headerName: 'Rk',
      width: 64,
      pinned: 'left',
      wrapHeaderText: false,
    },
    {
      field: 'name',
      headerName: 'Team',
      width: isMobile ? 134 : 150,
      pinned: 'left',
      // Let long team names wrap onto a second line on mobile rather than
      // clipping with ellipsis. Row height of 48 fits two lines of text-sm.
      wrapText: true,
      cellRenderer: (p: { value: string; data?: TeamRanking }) => {
        const id = p.data?.team_id;
        if (!id) return <span>{p.value}</span>;
        return (
          <SeasonLink to={`/teams/${id}`} className="text-blue-400 hover:underline">
            {p.value}
          </SeasonLink>
        );
      },
    },
    {
      field: 'conference',
      headerName: 'Conf',
      ...flexCol(100, 96),
      // Full conference names; the longest ("Missouri Valley") wraps onto a
      // second line at the 48px row height rather than widening the column.
      wrapText: true,
      valueFormatter: (p: { value: string | null }) => conferenceLabel(p.value),
      getQuickFilterText: (p: { value: string | null }) => conferenceSearchText(p.value),
    },
    {
      headerName: 'Record',
      ...flexCol(80),
      valueGetter: (p) => (p.data ? `${p.data.wins}-${p.data.losses}` : ''),
      sortable: false,
    },
    {
      field: 'adj_efficiency_margin',
      headerName: 'AdjEM',
      ...flexCol(85),
      cellRenderer: (p: { value: number | null }) => {
        if (p.value == null) return <span className="text-slate-500">—</span>;
        const tier = adjEmTier(p.value);
        return (
          <span
            className={`px-1.5 rounded border text-xs ${adjEmTierColor(tier)}`}
            title={tier ?? ''}
          >
            {p.value.toFixed(1)}
          </span>
        );
      },
    },
    {
      field: 'adj_offense',
      headerName: 'AdjO',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_offense} rank={p.data.adj_offense_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'adj_defense',
      headerName: 'AdjD',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_defense} rank={p.data.adj_defense_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'adj_tempo',
      headerName: 'Tempo',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.adj_tempo} rank={p.data.adj_tempo_rank} format={(v) => fmt(v)} totalTeams={totalTeams} />,
    },
    {
      field: 'sos',
      headerName: 'SOS',
      ...flexCol(75),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.sos} rank={p.data.sos_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
    {
      field: 'elo_rating',
      headerName: 'ELO',
      ...flexCol(80),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.elo_rating} rank={p.data.elo_rank} format={(v) => fmt(v, 0)} totalTeams={totalTeams} />,
    },
  ];

  // Offensive four factors — what this team's offense does. First column
  // gets the category divider so it visually breaks from the efficiency
  // block; same pattern the roster table uses.
  const offense: ColDef<TeamRanking>[] = [
    {
      field: 'effective_fg_pct',
      headerName: 'eFG%',
      ...flexCol(90),
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.effective_fg_pct} rank={p.data.effective_fg_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'turnover_pct',
      headerName: 'TOV%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.turnover_pct} rank={p.data.turnover_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'off_rebound_pct',
      headerName: 'ORB%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.off_rebound_pct} rank={p.data.off_rebound_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'ft_rate',
      headerName: 'FTR',
      ...flexCol(85),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.ft_rate} rank={p.data.ft_rate_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
  ];

  // Defensive four factors — what this team's defense forces opponents into.
  const defense: ColDef<TeamRanking>[] = [
    {
      field: 'opp_effective_fg_pct',
      headerName: 'OpFG%',
      ...flexCol(95, 75),
      headerStyle: CATEGORY_DIVIDER_STYLE,
      cellStyle: CATEGORY_DIVIDER_STYLE,
      headerTooltip: 'Opponent eFG% — defense holds opponents to lower number = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_effective_fg_pct} rank={p.data.opp_effective_fg_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'opp_turnover_pct',
      headerName: 'OpTOV%',
      ...flexCol(100, 80),
      headerTooltip: 'Opponent TOV% — defense forces turnovers; higher = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_turnover_pct} rank={p.data.opp_turnover_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'def_rebound_pct',
      headerName: 'DRB%',
      ...flexCol(90),
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.def_rebound_pct} rank={p.data.def_rebound_pct_rank} format={pct} totalTeams={totalTeams} />,
    },
    {
      field: 'opp_ft_rate',
      headerName: 'OpFTR',
      ...flexCol(90),
      headerTooltip: 'Opponent FT Rate — defense avoids fouling; lower = better',
      cellRenderer: (p: { data: TeamRanking }) =>
        p.data && <RankedCell value={p.data.opp_ft_rate} rank={p.data.opp_ft_rate_rank} format={(v) => fmt(v, 2)} totalTeams={totalTeams} />,
    },
  ];

  if (view === 'offense') return [...base, ...offense];
  if (view === 'defense') return [...base, ...defense];
  return base;
}

export default function Rankings() {
  const { season } = useSeason();
  usePageTitle('Team Rankings');
  const isMobile = useIsMobile();
  const [teams, setTeams] = useState<TeamRanking[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState('');
  const [view, setView] = useState<RankingsView>('standard');
  const columns = useMemo(
    () => buildColumns(teams.length, view, isMobile),
    [teams.length, view, isMobile],
  );

  useEffect(() => {
    // No `setLoading(true)` here — `react-hooks/set-state-in-effect`
    // forbids it. The initial `useState(true)` covers first load; on
    // subsequent season changes the previous data stays visible until
    // the new fetch resolves, which is mild stale-flicker but no worse
    // than what frameworks like Next.js do by default.
    fetchTeamRankings(season)
      .then((r) => setTeams(r.teams))
      .finally(() => setLoading(false));
  }, [season]);

  return (
    <div>
      <div className="mb-4">
        <ScoreTicker />
      </div>
      <TableToolbar
        title="Team Rankings"
        count={teams.length || null}
        countLabel="teams"
        search={
          <TableSearchInput
            value={search}
            onChange={setSearch}
            placeholder="Search team or conference…"
          />
        }
        controls={
          <>
            <span className="text-xs text-gray-500">View</span>
            <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs">
              {(
                [
                  { v: 'standard', label: 'Standard' },
                  { v: 'offense', label: 'Offense' },
                  { v: 'defense', label: 'Defense' },
                ] as const
              ).map(({ v, label }) => (
                <button
                  key={v}
                  onClick={() => setView(v)}
                  className={`px-2.5 py-1 ${
                    view === v
                      ? 'bg-blue-600 text-white'
                      : 'bg-gray-800 text-gray-300 hover:bg-gray-700'
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          </>
        }
      />
      {/* `domLayout="autoHeight"` lets the grid grow to fit all ~360 D-I
          teams in the page flow rather than living inside a viewport-bound
          internal-scroll container. The page itself becomes the only scroll
          surface, which avoids the nested-scroll UX where you have to scroll
          the page until the table is in view, then scroll the table to see
          lower-ranked teams. AG Grid still handles horizontal scroll
          internally when columns exceed container width. */}
      <div style={{ width: '100%' }}>
        <AgGridReact<TeamRanking>
          theme={gridTheme}
          rowData={teams}
          columnDefs={columns}
          loading={loading}
          domLayout="autoHeight"
          rowHeight={48}
          quickFilterText={search}
          defaultColDef={{
            sortable: true,
            resizable: true,
            suppressMovable: true,
            // Wrap header text instead of clipping with ellipsis. Critical on
            // mobile where flex-distributed columns compress below natural
            // width — headers like "Tempo" / "Record" / "OpFG%" otherwise lose
            // characters to the right edge.
            wrapHeaderText: true,
            autoHeaderHeight: true,
          }}
          getRowId={(p) => p.data.team_id}
        />
      </div>
    </div>
  );
}
