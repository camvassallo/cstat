import { useEffect, useMemo, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import {
  fetchTeamDetail,
  fetchProjectedTeam,
  type TeamProfile,
  type ScheduleEntry,
  type RosterEntry,
  type ArchetypeShare,
  type ProjectedReturning,
  type ProjectedArrival,
  type ProjectedRecruitDetail,
  type ProjectedDeparture,
  type ProjectedUncertain,
  type ProjectedTeam,
} from '../api/client';
import { classColor } from '../components/archetypeColors';
import { ClassTooltip } from '../components/Archetype';
import { campomTier, campomTierColor } from '../components/campom';
import { compareValues, type SortDir } from '../components/tableSort';
import { SortHeader, StickyHeader } from '../components/TableHeaders';
import { pctileTextColor } from '../components/pctile';
import { fracPct, pointPct } from '../components/format';
import { SeasonLink } from '../components/SeasonLink';
import {
  AVAILABLE_SEASONS_FALLBACK,
  seasonHref,
  setPageSeasons,
  useSeason,
} from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

const fmt = (v: number | null | undefined, d = 1) => (v != null ? v.toFixed(d) : '—');
const pct = (v: number | null | undefined) => (v != null ? (v * 100).toFixed(1) + '%' : '—');
const rkStr = (v: number | null | undefined) => (v != null ? `#${v}` : undefined);

function StatCard({ label, value, rank }: { label: string; value: string; rank?: string }) {
  return (
    <div className="bg-gray-800 rounded-lg p-4 text-center">
      <div className="text-xs text-gray-400 uppercase tracking-wide mb-1">{label}</div>
      <div className="text-2xl font-bold">{value}</div>
      {rank && <div className="text-xs text-gray-500 mt-1">{rank}</div>}
    </div>
  );
}

function FourFactors({ team, label }: { team: TeamProfile; label: string }) {
  const isOffense = label === 'Offense';
  const items = isOffense
    ? [
        { label: 'eFG%', value: pct(team.effective_fg_pct), rank: rkStr(team.effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.turnover_pct), rank: rkStr(team.turnover_pct_rank) },
        { label: 'ORB%', value: pct(team.off_rebound_pct), rank: rkStr(team.off_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.ft_rate, 2), rank: rkStr(team.ft_rate_rank) },
      ]
    : [
        { label: 'eFG%', value: pct(team.opp_effective_fg_pct), rank: rkStr(team.opp_effective_fg_pct_rank) },
        { label: 'TOV%', value: pct(team.opp_turnover_pct), rank: rkStr(team.opp_turnover_pct_rank) },
        { label: 'DRB%', value: pct(team.def_rebound_pct), rank: rkStr(team.def_rebound_pct_rank) },
        { label: 'FT Rate', value: fmt(team.opp_ft_rate, 2), rank: rkStr(team.opp_ft_rate_rank) },
      ];

  return (
    <div className="bg-gray-800 rounded-lg p-4">
      <h3 className="text-sm font-semibold text-gray-400 uppercase mb-3">{label} Four Factors</h3>
      <div className="grid grid-cols-4 gap-3 text-center">
        {items.map((item) => (
          <div key={item.label}>
            <div className="text-xs text-gray-500">{item.label}</div>
            <div className="font-semibold">{item.value}</div>
            {item.rank && <div className="text-[10px] text-gray-500">{item.rank}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}

/// Routing shim: the page handles two distinct modes (played-season
/// historical detail vs upcoming-season projection) backed by
/// different APIs. We branch by `season > AVAILABLE_SEASONS_FALLBACK[0]`
/// at the wrapper layer so each mode's hooks live in their own
/// component (Rules of Hooks). Triggered today by `?season=2027`
/// links from the transfer-portal "next team" and recruit "committed
/// to" cells, plus the Projected page's team links.
export default function TeamDetail() {
  const { id } = useParams<{ id: string }>();
  const { season } = useSeason();
  const maxPlayed = AVAILABLE_SEASONS_FALLBACK[0];
  if (id && season > maxPlayed) {
    return <ProjectedTeamView id={id} year={season} />;
  }
  return <HistoricalTeamDetail />;
}

function HistoricalTeamDetail() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const { season } = useSeason();
  const [team, setTeam] = useState<TeamProfile | null>(null);
  const [schedule, setSchedule] = useState<ScheduleEntry[]>([]);
  const [roster, setRoster] = useState<RosterEntry[]>([]);
  const [archetypeDist, setArchetypeDist] = useState<ArchetypeShare[]>([]);
  const [loading, setLoading] = useState(true);
  // Tab title tracks the loaded team and reflects the season selector so a
  // shared `/teams/<id>?season=2025` link reads "Duke 2025 — CamPom".
  usePageTitle(team ? `${team.name} ${team.season}` : null);

  useEffect(() => {
    if (!id) return;
    // No `setLoading(true)` here — see Rankings.tsx for the rationale.
    let cancelled = false;
    fetchTeamDetail(id, season)
      .then((r) => {
        if (cancelled) return;
        // Constrain the site-wide season selector to years where this team
        // has data. Edge case: D-I promotions mean a team may not appear in
        // every historical season — the dropdown reflects that.
        setPageSeasons(r.available_seasons);
        // Team UUIDs are season-scoped. The API resolves cross-season via
        // `natstat_id`; if it returned a different UUID, swap the URL to the
        // canonical one for this season so refresh/share/back work. Leave
        // `loading` true through the redirect so the UI doesn't render the
        // "Team not found" empty state in the gap before the next fetch.
        if (r.team.id !== id) {
          navigate(seasonHref(`/teams/${r.team.id}`, season), { replace: true });
          return;
        }
        setTeam(r.team);
        setSchedule(r.schedule);
        setRoster(r.roster);
        setArchetypeDist(r.archetype_distribution);
        setLoading(false);
      })
      .catch(() => {
        if (cancelled) return;
        // Clear stale state so the "not found" path renders cleanly when
        // the team has no row in the requested season. Mirrors PlayerDetail
        // — without this reset, the previous season's data lingers because
        // `team` is still set and the empty state never renders.
        setTeam(null);
        setSchedule([]);
        setRoster([]);
        setArchetypeDist([]);
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id, season, navigate]);

  // Release the season-selector override on unmount so the dropdown returns
  // to the global list when the user navigates away.
  useEffect(() => {
    return () => setPageSeasons(null);
  }, []);

  // Classes the team actually plays — sorted by team_share desc — drive the
  // visualization bar and chip row.
  const present = useMemo(
    () => archetypeDist.filter((a) => a.team_share > 0),
    [archetypeDist],
  );

  // "Identity": classes the team rosters meaningfully more than the D-I norm.
  // Filter on `team_share >= 5%` so we don't surface 1-game noise.
  const identity = useMemo(() => {
    return archetypeDist
      .filter((a) => a.index != null && a.index >= 1.3 && a.team_share >= 0.05)
      .sort((a, b) => (b.index ?? 0) - (a.index ?? 0))
      .slice(0, 3);
  }, [archetypeDist]);

  // "Gaps": classes that are common in D-I (>= 5% of league minutes) but
  // either missing or underweighted on this team. Sorted ascending by index
  // so missing classes (index = 0) come first.
  const gaps = useMemo(() => {
    return archetypeDist
      .filter(
        (a) =>
          a.d1_share >= 0.05 &&
          a.index != null &&
          a.index <= 0.5,
      )
      .sort((a, b) => (a.index ?? 0) - (b.index ?? 0))
      .slice(0, 3);
  }, [archetypeDist]);

  if (loading) return <div className="text-gray-400">Loading...</div>;
  if (!team) return <div className="text-red-400">Team not found</div>;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-3xl font-bold">{team.name}</h1>
        <div className="text-gray-400">
          {team.conference ?? 'Independent'} &middot; {team.wins ?? 0}-{team.losses ?? 0}
        </div>
      </div>

      {/* Stat Cards */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
        <StatCard label="AdjEM" value={fmt(team.adj_efficiency_margin)} rank={rkStr(team.adj_efficiency_margin_rank)} />
        <StatCard label="AdjO" value={fmt(team.adj_offense)} rank={rkStr(team.adj_offense_rank)} />
        <StatCard label="AdjD" value={fmt(team.adj_defense)} rank={rkStr(team.adj_defense_rank)} />
        <StatCard label="Tempo" value={fmt(team.adj_tempo)} rank={rkStr(team.adj_tempo_rank)} />
        <StatCard label="SOS" value={fmt(team.sos, 2)} rank={team.sos_rank ? `#${team.sos_rank}` : undefined} />
        <StatCard label="ELO" value={fmt(team.elo_rating, 0)} rank={team.elo_rank ? `#${team.elo_rank}` : undefined} />
      </div>

      {/* Four Factors */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FourFactors team={team} label="Offense" />
        <FourFactors team={team} label="Defense" />
      </div>

      {/* Archetype index vs D-I norm */}
      {present.length > 0 && (
        <div className="bg-gray-800 rounded-lg p-5">
          <div className="flex items-baseline justify-between mb-1 flex-wrap gap-2">
            <SeasonLink
              to="/archetypes"
              title="Learn about archetypes"
              className="group inline-flex items-baseline gap-1.5 text-lg font-bold hover:underline"
            >
              Roster Archetypes
              <svg
                viewBox="0 0 16 16"
                className="w-3.5 h-3.5 self-center opacity-50 group-hover:opacity-100 transition-opacity"
                fill="currentColor"
                aria-hidden="true"
              >
                <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm.75 3.25a.75.75 0 11-1.5 0 .75.75 0 011.5 0zM7 7h1.5v5H10v1H6v-1h1V8H6V7h1z" />
              </svg>
            </SeasonLink>
            <span className="text-xs text-gray-500">
              Indexed vs D-I average · 1.0× = league norm
            </span>
          </div>
          <p className="text-xs text-gray-500 mb-3">
            Minute-weighted share of each class on this roster (primary at full
            weight, secondary at half), compared against the D-I cohort. Hover
            the bar for counts.
          </p>

          {/* Stacked bar — present classes only, sized by team_share */}
          <div className="flex h-3 rounded overflow-hidden bg-gray-900">
            {present.map((a) => (
              <div
                key={a.primary_class}
                style={{ flexBasis: `${a.team_share * 100}%` }}
              >
                <ClassTooltip
                  cls={a.primary_class}
                  asBlock
                  extra={
                    <>
                      {(a.team_share * 100).toFixed(1)}% of minutes ·{' '}
                      {a.team_count} {a.team_count === 1 ? 'player' : 'players'}
                      {a.index != null && (
                        <>
                          {' '}· {a.index.toFixed(2)}× vs D-I
                        </>
                      )}
                    </>
                  }
                >
                  <div
                    className="h-3 w-full"
                    style={{ background: classColor(a.primary_class) }}
                  />
                </ClassTooltip>
              </div>
            ))}
          </div>

          {/* Identity / Gaps callouts — the actual takeaway */}
          {(identity.length > 0 || gaps.length > 0) && (
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3 mt-4">
              {identity.length > 0 && (
                <div>
                  <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-1.5">
                    Identity
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {identity.map((a) => (
                      <ClassTooltip
                        key={a.primary_class}
                        cls={a.primary_class}
                        extra={`${(a.team_share * 100).toFixed(1)}% team · ${(a.d1_share * 100).toFixed(1)}% D-I`}
                      >
                        <span className="inline-flex items-baseline gap-1.5 text-xs px-2 py-1 rounded bg-gray-900">
                          <span
                            className="inline-block w-2 h-2 rounded-full"
                            style={{ background: classColor(a.primary_class) }}
                          />
                          <span
                            className="font-semibold"
                            style={{ color: classColor(a.primary_class) }}
                          >
                            {a.primary_class}
                          </span>
                          <span className="text-green-400 font-bold">
                            {a.index != null ? `${a.index.toFixed(1)}×` : '—'}
                          </span>
                        </span>
                      </ClassTooltip>
                    ))}
                  </div>
                </div>
              )}
              {gaps.length > 0 && (
                <div>
                  <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-1.5">
                    Gaps
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {gaps.map((a) => (
                      <ClassTooltip
                        key={a.primary_class}
                        cls={a.primary_class}
                        extra={`${(a.team_share * 100).toFixed(1)}% team · ${(a.d1_share * 100).toFixed(1)}% D-I`}
                      >
                        <span className="inline-flex items-baseline gap-1.5 text-xs px-2 py-1 rounded bg-gray-900">
                          <span
                            className="inline-block w-2 h-2 rounded-full opacity-50"
                            style={{ background: classColor(a.primary_class) }}
                          />
                          <span
                            className="font-semibold opacity-70"
                            style={{ color: classColor(a.primary_class) }}
                          >
                            {a.primary_class}
                          </span>
                          <span className="text-red-400 font-bold">
                            {a.index === 0
                              ? 'missing'
                              : a.index != null
                                ? `${a.index.toFixed(1)}×`
                                : '—'}
                          </span>
                        </span>
                      </ClassTooltip>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Roster */}
      <RosterTable roster={roster} />

      {/* Schedule */}
      <ScheduleTable schedule={schedule} teamName={team.name} />
    </div>
  );
}

type RosterSortKey =
  | 'name'
  | 'campom'
  | 'games_played'
  | 'minutes_per_game'
  | 'usage_rate'
  | 'ppg'
  | 'rpg'
  | 'apg'
  | 'spg'
  | 'bpg'
  | 'topg'
  | 'true_shooting_pct'
  | 'ast_pct'
  | 'tov_pct'
  | 'orb_pct'
  | 'drb_pct'
  | 'stl_pct'
  | 'blk_pct';

type RosterView = 'raw' | 'rate';

// Continuous red → neutral → green gradient on percentile (0–1).
// Anchors: red-400 (#f87171) → gray-200 (#e5e7eb, the table's default text) → green-400 (#4ade80).
// Returns an rgb() string suitable for a `style.color` value.
function ValueWithPctile({ value, pctile }: { value: string; pctile: number | null | undefined }) {
  return <span style={{ color: pctileTextColor(pctile) }}>{value}</span>;
}

function RosterTable({ roster }: { roster: RosterEntry[] }) {
  const [view, setView] = useState<RosterView>('raw');
  const [sort, setSort] = useState<{ key: RosterSortKey; dir: SortDir }>({
    key: 'campom',
    dir: 'desc',
  });
  const onSort = (key: RosterSortKey) => {
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' }
        : { key, dir: key === 'name' ? 'asc' : 'desc' },
    );
  };

  // If the current sort column isn't visible in the new view, fall back to CamPom desc.
  const onViewChange = (next: RosterView) => {
    setView(next);
    const rawOnly: RosterSortKey[] = ['ppg', 'rpg', 'apg', 'spg', 'bpg', 'topg'];
    const rateOnly: RosterSortKey[] = ['ast_pct', 'tov_pct', 'orb_pct', 'drb_pct', 'stl_pct', 'blk_pct'];
    if (next === 'rate' && rawOnly.includes(sort.key)) setSort({ key: 'campom', dir: 'desc' });
    if (next === 'raw' && rateOnly.includes(sort.key)) setSort({ key: 'campom', dir: 'desc' });
  };

  const sorted = useMemo(() => {
    return [...roster].sort((a, b) => compareValues(a[sort.key], b[sort.key], sort.dir));
  }, [roster, sort]);

  return (
    <div>
      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <h2 className="text-xl font-bold">Roster</h2>
        <div className="inline-flex items-center rounded-md border border-gray-700 overflow-hidden text-xs">
          <button
            onClick={() => onViewChange('raw')}
            className={`px-3 py-1 ${view === 'raw' ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'}`}
          >
            Raw
          </button>
          <button
            onClick={() => onViewChange('rate')}
            className={`px-3 py-1 ${view === 'rate' ? 'bg-blue-600 text-white' : 'bg-gray-800 text-gray-300 hover:bg-gray-700'}`}
          >
            Rate
          </button>
        </div>
      </div>
      <div className="overflow-x-auto">
        <table className="min-w-full text-sm whitespace-nowrap">
        <thead>
          <tr className="text-gray-400 border-b border-gray-700">
            <SortHeader
              label="Player"
              sortKey="name"
              current={sort}
              onSort={onSort}
              className="left-0 z-20 border-r border-gray-700"
            />
            <StickyHeader>Class</StickyHeader>
              <SortHeader
                label="CamPom"
                sortKey="campom"
                current={sort}
                onSort={onSort}
                align="right"
                title="Composite player valuation."
                className="border-l border-gray-800"
              />
              <SortHeader label="GP" sortKey="games_played" current={sort} onSort={onSort} align="right" />
              <SortHeader label="MPG" sortKey="minutes_per_game" current={sort} onSort={onSort} align="right" />
              <SortHeader label="USG%" sortKey="usage_rate" current={sort} onSort={onSort} align="right" />
              <SortHeader label="TS%" sortKey="true_shooting_pct" current={sort} onSort={onSort} align="right" />
              {view === 'raw' ? (
                <>
                  <SortHeader
                    label="PPG"
                    sortKey="ppg"
                    current={sort}
                    onSort={onSort}
                    align="right"
                    className="border-l border-gray-800"
                  />
                  <SortHeader label="RPG" sortKey="rpg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="APG" sortKey="apg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="SPG" sortKey="spg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="BPG" sortKey="bpg" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="TOPG" sortKey="topg" current={sort} onSort={onSort} align="right" />
                </>
              ) : (
                <>
                  <SortHeader
                    label="AST%"
                    sortKey="ast_pct"
                    current={sort}
                    onSort={onSort}
                    align="right"
                    className="border-l border-gray-800"
                  />
                  <SortHeader label="TOV%" sortKey="tov_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="ORB%" sortKey="orb_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="DRB%" sortKey="drb_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="STL%" sortKey="stl_pct" current={sort} onSort={onSort} align="right" />
                  <SortHeader label="BLK%" sortKey="blk_pct" current={sort} onSort={onSort} align="right" />
                </>
              )}
            </tr>
          </thead>
          <tbody>
            {sorted.map((p) => (
              <tr key={p.player_id} className="group border-b border-gray-800 hover:bg-gray-800">
                <td className="py-2 px-2 sticky left-0 z-10 bg-gray-900 group-hover:bg-gray-800 border-r border-gray-700">
                  <SeasonLink to={`/players/${p.player_id}`} className="text-blue-400 hover:underline">
                    {p.name}
                  </SeasonLink>
                </td>
                <td className="py-2 px-2">
                  {p.primary_class ? (
                    <span className="inline-flex items-center gap-1">
                      <ClassTooltip cls={p.primary_class}>
                        <span
                          className="text-xs font-bold uppercase tracking-wide px-1.5 py-0.5 rounded"
                          style={{
                            color: classColor(p.primary_class),
                            background: classColor(p.primary_class) + '22',
                          }}
                        >
                          {p.primary_class}
                        </span>
                      </ClassTooltip>
                      {p.secondary_class && (
                        <ClassTooltip cls={p.secondary_class}>
                          <span
                            className="text-xs uppercase tracking-wide opacity-75"
                            style={{ color: classColor(p.secondary_class) }}
                          >
                            / {p.secondary_class}
                          </span>
                        </ClassTooltip>
                      )}
                    </span>
                  ) : (
                    <span className="text-gray-600 text-xs">—</span>
                  )}
                </td>
                <td className="py-2 px-2 text-right border-l border-gray-800">
                  {p.campom != null ? (
                    <span
                      className={`px-1.5 rounded border text-xs ${campomTierColor(campomTier(p.campom))}`}
                      title={campomTier(p.campom) ?? ''}
                    >
                      {p.campom.toFixed(1)}
                    </span>
                  ) : (
                    <span className="text-gray-600">—</span>
                  )}
                </td>
                <td className="py-2 px-2 text-right">{p.games_played}</td>
                <td className="py-2 px-2 text-right">{fmt(p.minutes_per_game)}</td>
                <td className="py-2 px-2 text-right">
                  <ValueWithPctile value={fracPct(p.usage_rate)} pctile={p.usage_rate_pct} />
                </td>
                <td className="py-2 px-2 text-right">
                  <ValueWithPctile value={fracPct(p.true_shooting_pct)} pctile={p.true_shooting_pct_pct} />
                </td>
                {view === 'raw' ? (
                  <>
                    <td className="py-2 px-2 text-right border-l border-gray-800">
                      <ValueWithPctile value={fmt(p.ppg)} pctile={p.ppg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.rpg)} pctile={p.rpg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.apg)} pctile={p.apg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.spg)} pctile={p.spg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.bpg)} pctile={p.bpg_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fmt(p.topg)} pctile={p.topg_pct} />
                    </td>
                  </>
                ) : (
                  <>
                    <td className="py-2 px-2 text-right border-l border-gray-800">
                      <ValueWithPctile value={fracPct(p.ast_pct)} pctile={p.ast_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={fracPct(p.tov_pct)} pctile={p.tov_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.orb_pct)} pctile={p.orb_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.drb_pct)} pctile={p.drb_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.stl_pct)} pctile={p.stl_pct_pct} />
                    </td>
                    <td className="py-2 px-2 text-right">
                      <ValueWithPctile value={pointPct(p.blk_pct)} pctile={p.blk_pct_pct} />
                    </td>
                  </>
                )}
              </tr>
            ))}
          {sorted.length === 0 && (
            <tr>
              <td colSpan={13} className="py-6 text-center text-gray-500 text-sm">
                No roster data.
              </td>
            </tr>
          )}
        </tbody>
      </table>
      </div>
    </div>
  );
}

function ScheduleTable({
  schedule,
  teamName,
}: {
  schedule: ScheduleEntry[];
  teamName: string;
}) {
  return (
    <div>
      <h2 className="text-xl font-bold mb-3">Schedule</h2>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-700">
              <StickyHeader>Date</StickyHeader>
              <StickyHeader>Opponent</StickyHeader>
              <StickyHeader align="center">Result</StickyHeader>
              <StickyHeader align="center">Score</StickyHeader>
              <StickyHeader align="center">Projected</StickyHeader>
            </tr>
          </thead>
          <tbody>
            {schedule.map((g) => (
              <ScheduleRow key={g.game_id} g={g} teamName={teamName} />
            ))}
            {schedule.length === 0 && (
              <tr>
                <td colSpan={5} className="py-6 text-center text-gray-500 text-sm">
                  No games scheduled.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ScheduleRow({ g, teamName }: { g: ScheduleEntry; teamName: string }) {
  const won =
    g.team_score != null && g.opponent_score != null && g.team_score > g.opponent_score;
  const lost =
    g.team_score != null && g.opponent_score != null && g.team_score < g.opponent_score;

  // "View matchup" link: routes to /predict pre-loaded with these two teams
  // and the correct venue (host=this team if is_home, opponent if not, neutral
  // overrides). Only active when the opponent name is populated — Predict
  // resolves teams by name, so a missing opponent_name would 404.
  const opponentName = g.opponent_name;
  let predictTo: string | null = null;
  if (opponentName) {
    let host = teamName;
    let visitor = opponentName;
    if (g.is_home === false && !g.is_neutral) {
      host = opponentName;
      visitor = teamName;
    }
    const venueParam = g.is_neutral ? '&venue=neutral' : '';
    predictTo = `/predict?home=${encodeURIComponent(host)}&away=${encodeURIComponent(visitor)}${venueParam}`;
  }

  // Render the projected cell. Always show when a projection is available —
  // upcoming games get the current model's pre-game forecast; completed
  // games show what we'd project *today* (current team state, not pre-game),
  // muted so it doesn't compete with the actual result. Proper pre-game
  // predictions for historical games are queued as a future roadmap item
  // (point-in-time historical predictions in `game_forecasts`).
  const projected = (() => {
    if (g.projected_margin == null) return null;
    const completed = g.team_score != null;
    const m = g.projected_margin;
    const fav = m > 0;
    const spread = `${fav ? '−' : '+'}${Math.abs(m).toFixed(1)}`;
    const winPct =
      g.projected_win_prob != null ? Math.round(g.projected_win_prob * 100) : null;
    // Score pair, requested team first. Null if either side missing
    // (totals model could fail independently of margin in edge cases).
    const scorePair =
      g.projected_score_team != null && g.projected_score_opp != null
        ? `${g.projected_score_team}–${g.projected_score_opp}`
        : null;
    const colorClass = completed
      ? 'text-gray-500'
      : fav
        ? 'text-green-400'
        : 'text-gray-300';
    const subdued = completed ? 'text-gray-600' : 'text-gray-500';
    const title = completed
      ? `If we replayed this matchup today: ${teamName} ${spread}. Not a pre-game prediction.`
      : `Predicted from ${teamName}'s perspective`;
    return (
      <span className={`font-mono ${colorClass}`} title={title}>
        {scorePair && (
          <>
            <span>{scorePair}</span>
            <span className="text-gray-600 mx-2">·</span>
          </>
        )}
        <span className={subdued}>{spread}</span>
        {winPct != null && (
          <span className={`${subdued} ml-1`}>({winPct}%)</span>
        )}
      </span>
    );
  })();

  return (
    <tr className="border-b border-gray-800 hover:bg-gray-800/50">
      <td className="py-2 px-2 text-gray-400">{g.game_date}</td>
      <td className="py-2 px-2">
        {g.is_home === false && '@ '}
        {g.opponent_id ? (
          <SeasonLink to={`/teams/${g.opponent_id}`} className="text-blue-400 hover:underline">
            {g.opponent_name ?? 'Unknown'}
          </SeasonLink>
        ) : (
          g.opponent_name ?? 'Unknown'
        )}
        {g.is_neutral && ' (N)'}
        {g.is_conference && <span className="text-gray-500 ml-1">*</span>}
      </td>
      <td
        className={`py-2 px-2 text-center font-semibold ${
          won ? 'text-green-400' : lost ? 'text-red-400' : ''
        }`}
      >
        {g.team_score != null ? (won ? 'W' : 'L') : '—'}
      </td>
      <td className="py-2 px-2 text-center">
        {g.team_score != null ? (
          predictTo ? (
            <SeasonLink to={predictTo} className="hover:underline">
              {g.team_score}-{g.opponent_score}
            </SeasonLink>
          ) : (
            `${g.team_score}-${g.opponent_score}`
          )
        ) : (
          '—'
        )}
      </td>
      <td className="py-2 px-2 text-center">
        {predictTo ? (
          <SeasonLink to={predictTo} className="hover:underline">
            {projected ?? <span className="text-gray-500">—</span>}
          </SeasonLink>
        ) : (
          projected ?? <span className="text-gray-500">—</span>
        )}
      </td>
    </tr>
  );
}

// ─── Projection mode ──────────────────────────────────────────────────
//
// Stripped TeamDetail variant for upcoming-season projections. Shares
// the same URL shape (`/teams/:id?season=2027`) — `TeamDetail`'s
// wrapper picks this component when the requested season is past the
// latest played one. No game log / schedule / season stats since the
// season hasn't happened; we render the projected AdjEM band, a small
// stat strip, and four roster cards (returning / arrivals / recruits /
// departures + uncertain) so the user can see who composes the roster
// the projection is built from. Future iteration: minutes-share radial
// roster plot from §5b, projected schedule from the predict model.

interface ProjectedTeamViewProps {
  id: string;
  year: number;
}

function ProjectedTeamView({ id, year }: ProjectedTeamViewProps) {
  const [data, setData] = useState<{
    team: { id: string; name: string | null; short_name: string | null };
    projection: ProjectedTeam;
    returning: ProjectedReturning[];
    arrivals: ProjectedArrival[];
    recruits: ProjectedRecruitDetail[];
    departures: ProjectedDeparture[];
    uncertain: ProjectedUncertain[];
    base_season: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // YYYY-YY label, e.g. "2026-27" for year=2027. Mirrors the
  // ProjectedRecruit chip on PlayerDetail / the Projected page.
  const seasonLabel = `${year - 1}-${(year % 100).toString().padStart(2, '0')}`;
  usePageTitle(data?.team?.name ? `${data.team.name} ${seasonLabel} projection` : null);

  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    fetchProjectedTeam(year, id)
      .then((r) => {
        if (cancelled) return;
        setData(r);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id, year]);

  if (loading) return <div className="p-4 text-gray-400">Loading projection...</div>;
  if (error) return <div className="p-4 text-rose-300">Failed to load projection: {error}</div>;
  if (!data) return <div className="p-4 text-gray-400">No projection data.</div>;

  const { team, projection: p, returning, arrivals, recruits, departures, uncertain, base_season } =
    data;
  const displayName = team.name ?? team.short_name ?? '(unknown team)';

  // Sort each roster section by CamPom desc so the visual headline of
  // each card is the most impactful player. Nulls sink to the bottom in
  // every section.
  //   - Returning / Arrivals: sort by projected next-season CamPom (the
  //     forward-looking chip is the page's main number), fall back to
  //     current CamPom when projection missing.
  //   - Recruits: sort by projected freshman CamPom from the freshman
  //     model (matches the only CamPom number shown on the row).
  //   - Departures: sort by counterfactual projection ("what they'd
  //     have been worth had they stayed"), falling back to base-season
  //     CamPom when the trajectory qual gate dropped the row — biggest
  //     losses first.
  const cmpDesc = (a: number | null | undefined, b: number | null | undefined) => {
    if (a == null && b == null) return 0;
    if (a == null) return 1;
    if (b == null) return -1;
    return b - a;
  };
  const returningSorted = [...returning].sort((x, y) =>
    cmpDesc(x.projected_campom_mean ?? x.cam_v3, y.projected_campom_mean ?? y.cam_v3),
  );
  const arrivalsSorted = [...arrivals].sort((x, y) =>
    cmpDesc(x.projected_campom_mean ?? x.cam_v3, y.projected_campom_mean ?? y.cam_v3),
  );
  const recruitsSorted = [...recruits].sort((x, y) =>
    cmpDesc(x.projected_cam_v3, y.projected_cam_v3),
  );
  const departuresSorted = [...departures].sort((x, y) =>
    cmpDesc(x.projected_campom_mean ?? x.cam_v3, y.projected_campom_mean ?? y.cam_v3),
  );
  const uncertainSorted = [...uncertain].sort((x, y) =>
    cmpDesc(x.projected_campom_mean ?? x.cam_v3, y.projected_campom_mean ?? y.cam_v3),
  );

  // Mid AdjEM chip color tier — mirrors `adjEmTone` on Projected but
  // duplicated here rather than promoted to a shared module so the
  // Projected page stays self-contained.
  const tone = (v: number | null): string => {
    if (v == null) return 'bg-slate-800/40 border-slate-700 text-slate-400';
    if (v >= 25) return 'bg-emerald-900/50 border-emerald-700 text-emerald-200';
    if (v >= 15) return 'bg-emerald-950/40 border-emerald-800 text-emerald-300';
    if (v >= 5) return 'bg-teal-950/40 border-teal-800 text-teal-300';
    if (v >= -5) return 'bg-slate-800/40 border-slate-700 text-slate-300';
    if (v >= -15) return 'bg-amber-950/40 border-amber-800 text-amber-300';
    return 'bg-rose-950/40 border-rose-800 text-rose-300';
  };
  const signed = (v: number | null) =>
    v == null ? '—' : v >= 0 ? `+${v.toFixed(1)}` : v.toFixed(1);

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-3xl font-bold flex items-center gap-3 flex-wrap">
            {displayName}
            <span className="text-xs uppercase tracking-wide px-2 py-0.5 rounded border border-amber-700/60 bg-amber-950/40 text-amber-300">
              {seasonLabel} projection
            </span>
          </h1>
          <div className="text-gray-400 mt-1 text-sm">
            Composed from{' '}
            <SeasonLink
              to={`/teams/${team.id}?season=${base_season}`}
              className="text-blue-400 hover:underline"
            >
              {base_season - 1}-{(base_season % 100).toString().padStart(2, '0')} roster
            </SeasonLink>{' '}
            minus departures + portal arrivals + HS commits.
          </div>
        </div>
        <div className="grid grid-cols-3 gap-3 min-w-[400px]">
          <div className="bg-gray-800 rounded-lg p-3 text-center">
            <div className="text-[10px] text-gray-400 uppercase tracking-wide">Mid AdjEM</div>
            <div className={`mt-1 inline-block px-2 py-0.5 rounded border ${tone(p.midpoint_adj_em)}`}>
              <span className="text-xl font-bold">{signed(p.midpoint_adj_em)}</span>
            </div>
          </div>
          <div className="bg-gray-800 rounded-lg p-3 text-center">
            <div className="text-[10px] text-gray-400 uppercase tracking-wide">Floor → Ceiling</div>
            <div className="mt-1 text-sm font-mono text-gray-300">
              {signed(p.floor_adj_em)} → {signed(p.ceiling_adj_em)}
            </div>
          </div>
          <div className="bg-gray-800 rounded-lg p-3 text-center">
            <div className="text-[10px] text-gray-400 uppercase tracking-wide">
              Δ vs {base_season - 1}-{(base_season % 100).toString().padStart(2, '0')}
            </div>
            <div className="mt-1 text-sm font-mono text-gray-300">
              {p.midpoint_adj_em != null && p.baseline_adj_em != null
                ? signed(p.midpoint_adj_em - p.baseline_adj_em)
                : '—'}
            </div>
          </div>
        </div>
      </div>

      {/* Honesty band — minimal version of the Projected banner. */}
      <div className="rounded border border-amber-800/40 bg-amber-950/20 text-amber-200 text-xs p-3 leading-relaxed">
        <strong className="text-amber-300">Projection mode:</strong>{' '}
        This page is the {seasonLabel} forward-looking view, not a played season. Roster = returners
        (minus seniors, outbound portal, firm NBA-draft departures) + incoming portal commits +
        HS-recruit class commits. Recruits use a tier-mean profile keyed on 247 composite rank — see
        the{' '}
        <SeasonLink to="/projected/2027" className="text-amber-200 underline hover:text-amber-100">
          Projected {seasonLabel} grid
        </SeasonLink>{' '}
        for full methodology + cross-team rankings.
      </div>

      {/* Roster cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <RosterCard title={`Returning (${returning.length})`}>
          {returning.length === 0 ? (
            <Empty label="No qualified returners" />
          ) : (
            returningSorted.map((r) => (
              <PlayerCard
                key={r.player_id}
                mpg={r.mpg}
                cam_v3={r.cam_v3}
                primary_class={r.primary_class}
                projected_mean={r.projected_campom_mean}
                projected_lower={r.projected_campom_lower}
                projected_upper={r.projected_campom_upper}
              >
                <SeasonLink
                  to={`/players/${r.player_id}?season=${base_season}`}
                  className="text-blue-400 hover:underline"
                >
                  {r.name}
                </SeasonLink>
              </PlayerCard>
            ))
          )}
        </RosterCard>

        <RosterCard title={`Incoming transfers (${arrivals.length})`}>
          {arrivals.length === 0 ? (
            <Empty label="No portal arrivals" />
          ) : (
            arrivalsSorted.map((a) => (
              <PlayerCard
                key={a.player_id}
                mpg={a.mpg}
                cam_v3={a.cam_v3}
                primary_class={a.primary_class}
                projected_mean={a.projected_campom_mean}
                projected_lower={a.projected_campom_lower}
                projected_upper={a.projected_campom_upper}
              >
                <SeasonLink
                  to={`/players/${a.player_id}?season=${base_season}`}
                  className="text-blue-400 hover:underline"
                >
                  {a.name}
                </SeasonLink>
                {a.source_team_id && a.source_team_name && (
                  <span className="text-xs text-gray-400 ml-2">
                    from{' '}
                    <SeasonLink
                      to={`/teams/${a.source_team_id}?season=${base_season}`}
                      className="text-blue-400 hover:underline"
                    >
                      {a.source_team_name}
                    </SeasonLink>
                  </span>
                )}
              </PlayerCard>
            ))
          )}
        </RosterCard>

        <RosterCard title={`Incoming recruits (${recruits.length})`}>
          {recruits.length === 0 ? (
            <Empty label="No HS commits" />
          ) : (
            recruitsSorted.map((r) => <RecruitCard key={r.recruit_id} r={r} />)
          )}
        </RosterCard>

        <RosterCard title={`Departures (${departures.length})${uncertain.length > 0 ? ` · ? ${uncertain.length}` : ''}`}>
          {departures.length === 0 && uncertain.length === 0 ? (
            <Empty label="No departures" />
          ) : (
            <>
              {/* Draft prospects (?) render above firm departures — they're
                  the highest-stakes uncertainty on the roster and deserve
                  visual priority over confirmed departures the user can no
                  longer act on. Each row carries the same name link +
                  archetype chip + counterfactual "if they stayed"
                  projection as the firm-departure rows, plus the Tankathon
                  mock-pick chip. */}
              {uncertainSorted.map((u) => {
                // Tankathon mock-pick informational chip. Top-30 picks are
                // green (the model effectively treats them as gone since
                // withdrawal rates from the lottery are near zero), 31-60
                // amber (real consideration but withdrawal common), and
                // missing-from-board styled muted to flag "declared but
                // not projected to be drafted — high withdrawal odds."
                // Phase 1 is informational only; no auto-promotion.
                const mockTone =
                  u.mock_pick == null
                    ? 'text-slate-400 border-slate-600/40'
                    : u.mock_pick <= 30
                      ? 'text-emerald-300 border-emerald-600/40'
                      : 'text-amber-300 border-amber-600/40';
                const mockLabel =
                  u.mock_pick == null
                    ? 'mock: NR'
                    : `mock #${u.mock_pick}`;
                const mockTitle =
                  u.mock_pick == null
                    ? 'Not on the current Tankathon mock draft (top 60). Declared players who fall off the board often withdraw before the deadline.'
                    : u.mock_pick <= 30
                      ? `Tankathon mock pick #${u.mock_pick}${u.mock_team ? ` (${u.mock_team})` : ''} — first-round projection. Withdrawal from this tier is rare.`
                      : `Tankathon mock pick #${u.mock_pick}${u.mock_team ? ` (${u.mock_team})` : ''} — second-round projection. Real draft consideration but second-rounders withdraw more often than lottery picks.`;
                return (
                  <div key={u.player_id} className="flex items-center justify-between py-1.5 px-2 hover:bg-gray-800/60 rounded gap-2">
                    <div className="flex items-center gap-2 flex-1 min-w-0">
                      <div className="truncate">
                        <SeasonLink
                          to={`/players/${u.player_id}?season=${base_season}`}
                          className="text-blue-400 hover:underline"
                        >
                          {u.name}
                        </SeasonLink>
                      </div>
                      {u.primary_class && (
                        <span
                          className="text-[10px] font-bold uppercase tracking-wide"
                          style={{ color: classColor(u.primary_class) }}
                        >
                          {u.primary_class}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-3 text-xs text-gray-400 tabular-nums">
                      {u.mpg != null && (
                        <span title="Prior-season MPG">{u.mpg.toFixed(0)}'</span>
                      )}
                      {u.projected_campom_mean != null ? (
                        <span className="flex items-center gap-1.5">
                          {u.cam_v3 != null && (
                            <>
                              <span
                                className="text-[10px] text-gray-500"
                                title="Prior-season CamPom v3"
                              >
                                {u.cam_v3.toFixed(1)}
                              </span>
                              <span className="text-gray-600 text-[10px]">→</span>
                            </>
                          )}
                          <span
                            className={`px-1.5 rounded border ${campomTierColor(campomTier(u.projected_campom_mean))}`}
                            title={
                              u.projected_campom_lower != null && u.projected_campom_upper != null
                                ? `If they withdraw and return: projected ${u.projected_campom_mean.toFixed(1)} (${u.projected_campom_lower.toFixed(1)}–${u.projected_campom_upper.toFixed(1)}). Current ${u.cam_v3 != null ? u.cam_v3.toFixed(1) : '—'}.`
                                : `If they withdraw and return: projected ${u.projected_campom_mean.toFixed(1)}.`
                            }
                          >
                            {u.projected_campom_mean.toFixed(1)}
                          </span>
                        </span>
                      ) : (
                        u.cam_v3 != null && (
                          <span
                            className={`px-1.5 rounded border ${campomTierColor(campomTier(u.cam_v3))}`}
                            title={`Prior-season CamPom v3: ${u.cam_v3.toFixed(1)}`}
                          >
                            {u.cam_v3.toFixed(1)}
                          </span>
                        )
                      )}
                      <span
                        className={`text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded border ${mockTone}`}
                        title={mockTitle}
                      >
                        {mockLabel}
                      </span>
                      <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded text-amber-400" title={u.reason}>
                        ? draft (TBD)
                      </span>
                    </div>
                  </div>
                );
              })}
              {departuresSorted.map((d) => {
                // Mirror PlayerCard's row shape: name · archetype on the
                // left, stats + status chip on the right. The right-most
                // pill replaces the "current → projected" chip pair
                // (departures have no next-season projection) with a
                // status indicator coloured by departure kind.
                const statusClass =
                  d.kind === 'senior'
                    ? 'text-slate-300 border-slate-500/40'
                    : d.kind === 'transferred'
                      ? 'text-amber-300 border-amber-500/40'
                      : 'text-rose-300 border-rose-500/40';
                const statusLabel =
                  d.kind === 'senior'
                    ? 'Sr graduation'
                    : d.kind === 'draft_gone'
                      ? 'NBA draft'
                      : `→ ${d.destination ?? 'portal'}`;
                return (
                  <div
                    key={d.player_id}
                    className="flex items-center justify-between py-1.5 px-2 hover:bg-gray-800/60 rounded gap-2"
                  >
                    <div className="flex items-center gap-2 flex-1 min-w-0">
                      <div className="truncate">
                        <SeasonLink
                          to={`/players/${d.player_id}?season=${d.prior_season}`}
                          className="text-blue-400 hover:underline"
                        >
                          {d.name}
                        </SeasonLink>
                      </div>
                      {d.primary_class && (
                        <span
                          className="text-[10px] font-bold uppercase tracking-wide"
                          style={{ color: classColor(d.primary_class) }}
                        >
                          {d.primary_class}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-3 text-xs text-gray-400 tabular-nums">
                      {d.mpg != null && (
                        <span title="Prior-season MPG">{d.mpg.toFixed(0)}'</span>
                      )}
                      {d.projected_campom_mean != null ? (
                        // Counterfactual "if they stayed" projection — same
                        // current → projected layout as Returning/Arrivals so
                        // the visual rhythm matches across all four sections.
                        <span className="flex items-center gap-1.5">
                          {d.cam_v3 != null && (
                            <>
                              <span
                                className="text-[10px] text-gray-500"
                                title="Prior-season CamPom v3"
                              >
                                {d.cam_v3.toFixed(1)}
                              </span>
                              <span className="text-gray-600 text-[10px]">→</span>
                            </>
                          )}
                          <span
                            className={`px-1.5 rounded border ${campomTierColor(campomTier(d.projected_campom_mean))}`}
                            title={
                              d.projected_campom_lower != null && d.projected_campom_upper != null
                                ? `Counterfactual: if they'd stayed, projected ${d.projected_campom_mean.toFixed(1)} (${d.projected_campom_lower.toFixed(1)}–${d.projected_campom_upper.toFixed(1)}). Current ${d.cam_v3 != null ? d.cam_v3.toFixed(1) : '—'}.`
                                : `Counterfactual: if they'd stayed, projected ${d.projected_campom_mean.toFixed(1)}.`
                            }
                          >
                            {d.projected_campom_mean.toFixed(1)}
                          </span>
                        </span>
                      ) : (
                        d.cam_v3 != null && (
                          <span
                            className={`px-1.5 rounded border ${campomTierColor(campomTier(d.cam_v3))}`}
                            title={`Prior-season CamPom v3: ${d.cam_v3.toFixed(1)} (no counterfactual projection — trajectory qual gate failed or batch inference dropped the row).`}
                          >
                            {d.cam_v3.toFixed(1)}
                          </span>
                        )
                      )}
                      {d.kind === 'transferred' && d.destination_team_id ? (
                        <SeasonLink
                          to={`/teams/${d.destination_team_id}?season=${year}`}
                          className={`text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded border hover:underline ${statusClass}`}
                          title={`to ${d.destination}`}
                        >
                          {statusLabel}
                        </SeasonLink>
                      ) : (
                        <span
                          className={`text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded border ${statusClass}`}
                          title={
                            d.kind === 'transferred' && d.destination
                              ? `to ${d.destination}`
                              : undefined
                          }
                        >
                          {statusLabel}
                        </span>
                      )}
                    </div>
                  </div>
                );
              })}
            </>
          )}
        </RosterCard>
      </div>
    </div>
  );
}

function RosterCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="bg-gray-900 rounded-lg border border-gray-800">
      <div className="px-4 py-2 border-b border-gray-800 text-sm font-semibold text-gray-300 uppercase tracking-wide">
        {title}
      </div>
      <div className="p-2 space-y-1">{children}</div>
    </div>
  );
}

function Empty({ label }: { label: string }) {
  return <div className="text-sm text-gray-500 italic px-2 py-3">{label}</div>;
}

function PlayerCard({
  mpg,
  cam_v3,
  primary_class,
  projected_mean,
  projected_lower,
  projected_upper,
  children,
}: {
  // Player name is intentionally passed via `children` (so callers can
  // wrap the rendered name in `<SeasonLink>` for navigation) rather
  // than as a separate prop. The shape of the row — name on the left,
  // archetype chip, then MPG + (projected → current) chips on the
  // right — is locked here. The projection page is forward-looking,
  // so the projected chip is the visual headline; the small grey
  // current number sits to its left for comparison.
  mpg: number;
  cam_v3: number | null;
  primary_class: string | null;
  projected_mean: number | null;
  projected_lower: number | null;
  projected_upper: number | null;
  children: React.ReactNode;
}) {
  // Regression-to-the-mean honesty note — same conditional as the
  // Transfer/PlayerDetail surfaces. Anchors on the model's *input*
  // (current/source-season CamPom), not the projection.
  const regressionNote =
    cam_v3 != null && cam_v3 >= 15
      ? ' Regression-to-the-mean: model under-projects elite-tier returners (≈−3 CamPom bias on inputs ≥+15). Read the q90 ceiling for the optimistic case.'
      : cam_v3 != null && cam_v3 >= 10
        ? ' Mild regression expected on this tier (≈−0.3 CamPom bias on +10..+15 inputs).'
        : '';
  const projectedTitle =
    projected_mean != null
      ? `Projected next-season CamPom: ${projected_mean.toFixed(1)}${
          projected_lower != null && projected_upper != null
            ? ` (${projected_lower.toFixed(1)}–${projected_upper.toFixed(1)})`
            : ''
        }${cam_v3 != null ? `. Current ${cam_v3.toFixed(1)}, Δ ${projected_mean - cam_v3 >= 0 ? '+' : ''}${(projected_mean - cam_v3).toFixed(1)}.` : '.'}${regressionNote}`
      : cam_v3 != null
        ? 'No next-season projection (player did not pass the trajectory model qual gate or batch inference failed). Current CamPom shown.'
        : '';
  return (
    <div className="flex items-center justify-between py-1.5 px-2 hover:bg-gray-800/60 rounded">
      <div className="flex items-center gap-2 flex-1 min-w-0">
        <div className="truncate">{children}</div>
        {primary_class && (
          <span
            className="text-[10px] font-bold uppercase tracking-wide"
            style={{ color: classColor(primary_class) }}
          >
            {primary_class}
          </span>
        )}
      </div>
      <div className="flex items-center gap-3 text-xs text-gray-400 tabular-nums">
        <span title="Prior-season MPG">{mpg.toFixed(0)}'</span>
        {projected_mean != null ? (
          <span className="flex items-center gap-1.5">
            {cam_v3 != null && (
              <>
                <span className="text-[10px] text-gray-500" title="Prior-season CamPom v3">
                  {cam_v3.toFixed(1)}
                </span>
                <span className="text-gray-600 text-[10px]">→</span>
              </>
            )}
            <span
              className={`px-1.5 rounded border ${campomTierColor(campomTier(projected_mean))}`}
              title={projectedTitle}
            >
              {projected_mean.toFixed(1)}
            </span>
          </span>
        ) : (
          cam_v3 != null && (
            <span
              className={`px-1.5 rounded border ${campomTierColor(campomTier(cam_v3))}`}
              title={projectedTitle}
            >
              {cam_v3.toFixed(1)}
            </span>
          )
        )}
      </div>
    </div>
  );
}

function RecruitCard({ r }: { r: ProjectedRecruitDetail }) {
  return (
    <div className="flex items-center justify-between py-1.5 px-2 hover:bg-gray-800/60 rounded gap-2">
      <div className="flex items-center gap-2 flex-1 min-w-0">
        <span className="text-sm text-gray-200 truncate">{r.name}</span>
        {r.composite_rank != null && (
          <span className="text-[10px] text-gray-500">#{r.composite_rank}</span>
        )}
        {r.star_rating != null && (
          <span className="text-[10px] text-amber-300">{'★'.repeat(r.star_rating)}</span>
        )}
        {r.position && (
          <span
            className="text-[11px] font-bold uppercase tracking-wide px-1.5 py-0.5 rounded bg-sky-900/40 border border-sky-700/60 text-sky-200 shrink-0"
            title="247Sports listed position"
          >
            {r.position}
          </span>
        )}
      </div>
      <div className="flex items-center gap-2 text-xs">
        {r.projected_cam_v3 != null && (
          <span
            className={`px-1.5 rounded border ${campomTierColor(campomTier(r.projected_cam_v3))}`}
            title={
              r.projected_campom_lower != null && r.projected_campom_upper != null
                ? `Phase 6 freshman-impact projection: ${r.projected_cam_v3.toFixed(1)} (${r.projected_campom_lower.toFixed(1)}–${r.projected_campom_upper.toFixed(1)}). Tier ${r.tier.slice(1).toUpperCase()} cohort.${
                    r.tier === 't1' || r.tier === 't2'
                      ? ' Wide bands on T1/T2 reflect elite-tail uncertainty.'
                      : ''
                  }`
                : `Tier ${r.tier.slice(1).toUpperCase()} mean projected CamPom v3 (batch inference fell back to tier-mean synthesis — no per-player band available).`
            }
          >
            {r.projected_cam_v3.toFixed(1)}
          </span>
        )}
      </div>
    </div>
  );
}

