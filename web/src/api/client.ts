const BASE = '/api';

async function fetchJson<T>(path: string, params?: Record<string, string | undefined>): Promise<T> {
  const url = new URL(`${BASE}${path}`, window.location.origin);
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== '') url.searchParams.set(k, v);
    }
  }
  const res = await fetch(url.toString());
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json();
}

// Teams
export interface TeamRanking {
  rank: number;
  team_id: string;
  name: string;
  conference: string | null;
  wins: number;
  losses: number;
  adj_offense: number | null;
  adj_offense_rank: number | null;
  adj_defense: number | null;
  adj_defense_rank: number | null;
  adj_efficiency_margin: number | null;
  adj_tempo: number | null;
  adj_tempo_rank: number | null;
  sos: number | null;
  sos_rank: number | null;
  elo_rating: number | null;
  elo_rank: number | null;
  point_diff: number | null;
  effective_fg_pct: number | null;
  effective_fg_pct_rank: number | null;
  turnover_pct: number | null;
  turnover_pct_rank: number | null;
  off_rebound_pct: number | null;
  off_rebound_pct_rank: number | null;
  ft_rate: number | null;
  ft_rate_rank: number | null;
  opp_effective_fg_pct: number | null;
  opp_effective_fg_pct_rank: number | null;
  opp_turnover_pct: number | null;
  opp_turnover_pct_rank: number | null;
  def_rebound_pct: number | null;
  def_rebound_pct_rank: number | null;
  opp_ft_rate: number | null;
  opp_ft_rate_rank: number | null;
}

export interface ScheduleEntry {
  game_id: string;
  game_date: string;
  opponent_id: string | null;
  opponent_name: string | null;
  is_home: boolean | null;
  is_neutral: boolean | null;
  team_score: number | null;
  opponent_score: number | null;
  is_conference: boolean | null;
  is_postseason: boolean | null;
  /// Predicted margin from the requested team's perspective (positive =
  /// requested team favored). Populated for every game on the schedule.
  /// Upcoming games get the current-state pre-game forecast; completed
  /// games get the honest point-in-time projection from the pit bundle.
  /// Read `is_pre_game_projection` to decide framing — don't recompute
  /// "played" from team_score/opponent_score on this side.
  projected_margin: number | null;
  /// Probability the requested team wins, derived from `projected_margin`.
  projected_win_prob: number | null;
  /// Projected integer score for the requested team. Null when projection
  /// inputs are missing.
  projected_score_team: number | null;
  /// Projected integer score for the opponent.
  projected_score_opp: number | null;
  /// Server-side flag: the projection above came from the point-in-time
  /// bundle (`as_of_date = game_date − 1`). Single source of truth for
  /// "is this a pre-game projection" — don't derive your own predicate.
  is_pre_game_projection: boolean;
}

export interface RosterEntry {
  player_id: string;
  name: string;
  position: string | null;
  class_year: string | null;
  height_inches: number | null;
  jersey_number: string | null;
  games_played: number;
  minutes_per_game: number | null;
  ppg: number | null;
  rpg: number | null;
  apg: number | null;
  spg: number | null;
  bpg: number | null;
  topg: number | null;
  fg_pct: number | null;
  tp_pct: number | null;
  ft_pct: number | null;
  effective_fg_pct: number | null;
  true_shooting_pct: number | null;
  usage_rate: number | null;
  ast_pct: number | null;
  tov_pct: number | null;
  orb_pct: number | null;
  drb_pct: number | null;
  stl_pct: number | null;
  blk_pct: number | null;
  gbpm: number | null;
  campom: number | null;
  campom_pct: number | null;
  ppg_pct: number | null;
  rpg_pct: number | null;
  apg_pct: number | null;
  spg_pct: number | null;
  bpg_pct: number | null;
  topg_pct: number | null;
  true_shooting_pct_pct: number | null;
  usage_rate_pct: number | null;
  ast_pct_pct: number | null;
  tov_pct_pct: number | null;
  orb_pct_pct: number | null;
  drb_pct_pct: number | null;
  stl_pct_pct: number | null;
  blk_pct_pct: number | null;
  primary_class: string | null;
  secondary_class: string | null;
  /// Torvik shot-zone volumes — drive the team aggregate shot-diet
  /// panel on TeamDetail. `null` when the player has no Torvik row.
  rim_attempted: number | null;
  mid_attempted: number | null;
  tpa: number | null;
  fta: number | null;
  rim_made: number | null;
  mid_made: number | null;
  tpm: number | null;
  ftm: number | null;
}

export interface ArchetypeShare {
  primary_class: string;
  team_count: number;
  team_minutes: number;
  /// 0..1 — share of this class within the team's total rostered minutes.
  team_share: number;
  /// 0..1 — share of this class across all D-I qualified players (minute-weighted).
  d1_share: number;
  /// `team_share / d1_share`. 1.0 = league average; >1 loaded; <1 light.
  /// `null` when totals are zero (not yet computed for this team/season).
  index: number | null;
}

export interface TeamProfile {
  id: string;
  name: string;
  short_name: string | null;
  conference: string | null;
  season: number;
  wins: number | null;
  losses: number | null;
  adj_offense: number | null;
  adj_offense_rank: number | null;
  adj_defense: number | null;
  adj_defense_rank: number | null;
  adj_efficiency_margin: number | null;
  adj_efficiency_margin_rank: number | null;
  adj_tempo: number | null;
  adj_tempo_rank: number | null;
  sos: number | null;
  sos_rank: number | null;
  elo_rating: number | null;
  elo_rank: number | null;
  point_diff: number | null;
  effective_fg_pct: number | null;
  effective_fg_pct_rank: number | null;
  turnover_pct: number | null;
  turnover_pct_rank: number | null;
  off_rebound_pct: number | null;
  off_rebound_pct_rank: number | null;
  ft_rate: number | null;
  ft_rate_rank: number | null;
  opp_effective_fg_pct: number | null;
  opp_effective_fg_pct_rank: number | null;
  opp_turnover_pct: number | null;
  opp_turnover_pct_rank: number | null;
  def_rebound_pct: number | null;
  def_rebound_pct_rank: number | null;
  opp_ft_rate: number | null;
  opp_ft_rate_rank: number | null;
}

export function fetchTeamRankings(season?: number) {
  return fetchJson<{ season: number; teams: TeamRanking[] }>('/teams/rankings', {
    season: season?.toString(),
  });
}

export function fetchTeamDetail(id: string, season?: number) {
  return fetchJson<{
    team: TeamProfile;
    schedule: ScheduleEntry[];
    roster: RosterEntry[];
    archetype_distribution: ArchetypeShare[];
    /// Seasons in which this team (joined cross-season via natstat_id) has
    /// any row. Drives the page-scoped season dropdown override.
    available_seasons: number[];
    /// Count of D-I teams in this season — denominator for converting
    /// per-stat ranks to percentiles on the stat-card tints. Matches
    /// the rankings page's `teams.length`.
    total_teams: number;
  }>(`/teams/${id}`, { season: season?.toString() });
}

// Players
export interface PlayerRow {
  player_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  conference: string | null;
  position: string | null;
  class_year: string | null;
  season: number;
  games_played: number;
  minutes_per_game: number | null;
  ppg: number | null;
  rpg: number | null;
  apg: number | null;
  spg: number | null;
  bpg: number | null;
  topg: number | null;
  fg_pct: number | null;
  tp_pct: number | null;
  ft_pct: number | null;
  effective_fg_pct: number | null;
  true_shooting_pct: number | null;
  usage_rate: number | null;
  offensive_rating: number | null;
  defensive_rating: number | null;
  net_rating: number | null;
  player_sos: number | null;
  campom: number | null;
  campom_pct: number | null;
  ast_pct: number | null;
  tov_pct: number | null;
  orb_pct: number | null;
  drb_pct: number | null;
  stl_pct: number | null;
  blk_pct: number | null;
  ft_rate: number | null;
  ppg_pct: number | null;
  rpg_pct: number | null;
  apg_pct: number | null;
  spg_pct: number | null;
  bpg_pct: number | null;
  topg_pct: number | null;
  mpg_pct: number | null;
  usage_rate_pct: number | null;
  true_shooting_pct_pct: number | null;
  ast_pct_pct: number | null;
  tov_pct_pct: number | null;
  orb_pct_pct: number | null;
  drb_pct_pct: number | null;
  stl_pct_pct: number | null;
  blk_pct_pct: number | null;
  primary_class: string | null;
  secondary_class: string | null;
}

export interface PlayerProfile {
  id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  conference: string | null;
  position: string | null;
  class_year: string | null;
  height_inches: number | null;
  weight_lbs: number | null;
  jersey_number: string | null;
  season: number;
}

export interface PlayerSeasonStats {
  games_played: number;
  minutes_per_game: number | null;
  ppg: number | null;
  rpg: number | null;
  apg: number | null;
  spg: number | null;
  bpg: number | null;
  topg: number | null;
  fg_pct: number | null;
  tp_pct: number | null;
  ft_pct: number | null;
  effective_fg_pct: number | null;
  true_shooting_pct: number | null;
  offensive_rating: number | null;
  defensive_rating: number | null;
  net_rating: number | null;
  usage_rate: number | null;
  ast_pct: number | null;
  tov_pct: number | null;
  orb_pct: number | null;
  drb_pct: number | null;
  stl_pct: number | null;
  blk_pct: number | null;
  ft_rate: number | null;
  player_sos: number | null;
}

export interface Percentiles {
  ppg_pct: number | null;
  rpg_pct: number | null;
  apg_pct: number | null;
  spg_pct: number | null;
  bpg_pct: number | null;
  fg_pct_pct: number | null;
  tp_pct_pct: number | null;
  ft_pct_pct: number | null;
  effective_fg_pct_pct: number | null;
  true_shooting_pct_pct: number | null;
  usage_rate_pct: number | null;
  offensive_rating_pct: number | null;
  defensive_rating_pct: number | null;
  player_sos_pct: number | null;
  ast_pct_pct: number | null;
  tov_pct_pct: number | null;
  mpg_pct: number | null;
  topg_pct: number | null;
  orb_pct_pct: number | null;
  drb_pct_pct: number | null;
  stl_pct_pct: number | null;
  blk_pct_pct: number | null;
  ft_rate_pct: number | null;
}

export interface GameLogEntry {
  game_id: string;
  game_date: string;
  opponent_id: string | null;
  opponent_name: string | null;
  is_home: boolean | null;
  minutes: number | null;
  points: number | null;
  fgm: number | null;
  fga: number | null;
  fg_pct: number | null;
  tpm: number | null;
  tpa: number | null;
  total_rebounds: number | null;
  assists: number | null;
  steals: number | null;
  blocks: number | null;
  turnovers: number | null;
  game_score: number | null;
  rolling_ppg: number | null;
  rolling_game_score: number | null;
  rolling_ts_pct: number | null;
}

export function fetchPlayers(params: {
  search?: string;
  team?: string;
  season?: number;
  sort?: string;
  order?: string;
  archetype?: string;
  includeSecondaryArchetype?: boolean;
  limit?: number;
  offset?: number;
}) {
  return fetchJson<{ season: number; players: PlayerRow[]; total: number; limit: number; offset: number }>(
    '/players',
    {
      search: params.search,
      team: params.team,
      season: params.season?.toString(),
      sort: params.sort,
      order: params.order,
      archetype: params.archetype,
      include_secondary_archetype: params.includeSecondaryArchetype ? 'true' : undefined,
      limit: params.limit?.toString(),
      offset: params.offset?.toString(),
    },
  );
}

// Transfer portal — one row per ranked 247Sports transfer, enriched with our
// CamPom value when we can match the player to a row in the prior season.
export interface TransferRow {
  rank_247: number | null;
  name: string;
  player_id: string | null;
  position: string;
  height: string | null;
  weight: number | null;
  status: string;
  rating_247: number | null;
  previous_team: string | null;
  previous_team_full: string | null;
  previous_team_id: string | null;
  next_team: string | null;
  next_team_id: string | null;
  primary_class: string | null;
  secondary_class: string | null;
  campom: number | null;
  campom_pct: number | null;
  minutes_per_game: number | null;
  games_played: number | null;
  url_247: string | null;
  // Phase 5c trajectory projection — predicted CamPom for the transfer's
  // first destination season (= source year + 1). NULL when the transfer
  // didn't match a cstat row, didn't pass the trajectory qual gate, or
  // batch inference failed. Trajectory model is destination-agnostic.
  projected_campom_mean: number | null;
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
}

export function fetchTransfers(year: number) {
  return fetchJson<{ year: number; transfers: TransferRow[]; total: number }>(
    `/transfers/${year}`,
  );
}

// NBA Draft big board — one row per curated draft prospect for a given draft
// cycle year, sourced from `data/draft/{year}_big_board.json` (Tankathon) and
// joined to cstat players for CamPom. `campom` / `player_id` are null for
// prospects with no college row this season (seniors who left, internationals,
// G-Leaguers). `status` is derived: gone / declared / senior / international /
// g-league / prospect. (`gone` = on the early-entrant list and locked in
// post-withdrawal-deadline; `declared` = declared with the window still open.)
export interface DraftProspect {
  draft_rank: number | null;
  name: string;
  tier: string;
  position: string | null;
  class_year: string | null;
  status: string;
  current_team: string;
  team_id: string | null;
  team_name: string | null;
  player_id: string | null;
  campom: number | null;
}

export function fetchDraft(year: number) {
  return fetchJson<{ year: number; prospects: DraftProspect[]; total: number }>(
    `/draft/${year}`,
  );
}

// HS recruit class — one row per 247Sports composite-ranked HS recruit for a
// given recruiting class year. `year + 1` is the cstat-season they first play
// in (class-of-2026 → cstat-season 2027). CamPom / archetype fields stay NULL
// until that freshman cstat-season ingests box scores.
export interface RecruitRow {
  composite_rank: number | null;
  name: string;
  position: string | null;
  height: string | null;
  weight: number | null;
  city: string | null;
  state: string | null;
  high_school: string | null;
  composite_rating: number | null;
  star_rating: number | null;
  previous_rank: number | null;
  position_rank: number | null;
  state_rank: number | null;
  committed_school: string | null;
  committed_school_short: string | null;
  committed_team_id: string | null;
  commit_status: string | null;
  profile_url: string | null;
  photo_url: string | null;
  player_id: string | null;
  campom: number | null;
  campom_pct: number | null;
  primary_class: string | null;
  secondary_class: string | null;
  minutes_per_game: number | null;
  games_played: number | null;
  // Phase 6 freshman-impact projection — populated for every recruit row
  // regardless of cstat_player_id. Mean + q10/q90 band; the chip surfaces
  // the mean, tooltip shows the band.
  projected_campom_mean: number | null;
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
}

export function fetchRecruits(year: number) {
  return fetchJson<{
    year: number;
    base_season: number;
    recruits: RecruitRow[];
    total: number;
  }>(`/recruits/${year}`);
}

/// One synthesized HS recruit from the route's `top_recruits` payload.
/// Source: `recruits` table joined to committed_team_id. Surfaced in the
/// Recruits-column hover (name + composite rank + star rating).
export interface ProjectedRecruit {
  name: string;
  composite_rank: number | null;
  star_rating: number | null;
}

export interface ProjectedTeam {
  team_id: string;
  team_name: string;
  team_full_name: string;
  /// AdjEM if every declared NBA-draft player withdraws and returns.
  /// Shrunk 50% toward `baseline_adj_em`.
  ceiling_adj_em: number | null;
  /// AdjEM if every declared NBA-draft player is gone.
  /// Shrunk 50% toward `baseline_adj_em`.
  floor_adj_em: number | null;
  /// (ceiling + floor) / 2, or null when the prediction is gated out.
  midpoint_adj_em: number | null;
  returning_count: number;
  /// Σ base-season CamPom of the returning players (talent retained).
  returning_cam_v3_sum: number;
  arrivals_count: number;
  /// Σ base-season CamPom of the incoming portal arrivals (talent gained).
  arrivals_cam_v3_sum: number;
  /// Number of HS recruits committed to this team (class-of-`base_season`).
  /// Each contributes a synthesized PlayerRow drawn from a tier-mean
  /// freshman profile.
  recruits_count: number;
  /// Σ *projected* freshman-season CamPom of the recruit class (forward
  /// projection from the freshman-impact model, not prior production).
  recruits_cam_v3_sum: number;
  /// Up to 5 highest-ranked recruits for UI display.
  top_recruits: ProjectedRecruit[];
  uncertain_count: number;
  departures_count: number;
  /// Σ base-season CamPom across all departures (Sr + portal-out + draft).
  departures_cam_v3_sum: number;
  /// True when (returning + arrivals + recruits) is below the projection
  /// threshold — render '—' instead of the prediction columns.
  too_thin: boolean;
  /// Team's AdjEM at the end of the base season (= year-1, the
  /// just-completed season). Used as the shrinkage anchor and the
  /// reference for the 'Δ vs last' column.
  baseline_adj_em: number | null;
  /// Team's *actual* AdjEM for the projected season itself. Null for the
  /// live/upcoming forecast year (not played yet). Drives the historical
  /// view's "Projected vs Actual" accuracy column.
  actual_adj_em: number | null;
}

/// One projected team detail row, returned by
/// `GET /api/projections/{year}/teams/{team_id}`. Wraps the team's
/// identity, the AdjEM band + baseline, and the four roster cohorts
/// (returning / arrivals / recruits / departures + uncertain). Used by
/// the projection-mode branch of `TeamDetail.tsx`.
export interface ProjectedReturning {
  player_id: string;
  name: string;
  mpg: number;
  ppg: number | null;
  cam_v3: number | null;
  primary_class: string | null;
  // Phase 5c trajectory projection — predicted next-season CamPom for
  // this player on the projected roster. Null when the player didn't
  // pass the trajectory qual gate (≥5 GP, ≥5 MPG) or batch inference
  // failed. `cam_v3` (above) is the *current/source-season* CamPom;
  // the chip on the projection page shows the projected number with
  // the current as a delta on hover.
  projected_campom_mean: number | null;
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
}
export interface ProjectedArrival extends ProjectedReturning {
  /// Source team UUID + display name in the played base season. Powers
  /// the "from $TEAM" link on the arrival card.
  source_team_id: string | null;
  source_team_name: string | null;
}
export interface ProjectedRecruitDetail {
  recruit_id: string;
  name: string;
  composite_rank: number | null;
  star_rating: number | null;
  tier: 't1' | 't2' | 't3' | 't4';
  /// 247's listed position (e.g. "PG", "SF", "C"). Null when unset on 247.
  position: string | null;
  // Mean predicted freshman-season CamPom from the freshman-impact
  // model. Same number as the chip on the Recruits tab; null when the
  // freshman batch fell back to tier-mean synthesis.
  projected_cam_v3: number | null;
  // q10/q90 band from the freshman model. Both null on the same
  // fallback path that nulls `projected_cam_v3`.
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
}
export interface ProjectedDeparture {
  kind: 'senior' | 'transferred' | 'draft_gone';
  player_id: string;
  name: string;
  /// Base-season the player played for the source team. UI uses this to
  /// link the name to the historical detail page rather than the new
  /// season where they no longer exist as a roster row.
  prior_season: number;
  /// Player's D&D archetype primary class in base_season (e.g. "Wizard").
  primary_class?: string | null;
  /// Prior-season minutes-per-game. Null when the player didn't qualify
  /// (rare for actual departures; mostly walk-ons who never broke rotation).
  mpg?: number | null;
  /// Prior-season CamPom v3 (Torvik passthrough). Null when the player
  /// didn't have Torvik coverage for base_season.
  cam_v3?: number | null;
  /// Counterfactual trajectory projection — what we'd have forecast for
  /// this player in the projected season if they had stayed. Renders
  /// as "current → projected" chip pair matching Returning/Arrivals
  /// rows. Null when the trajectory qual gate (≥5 GP, ≥5 MPG) failed
  /// or batch inference dropped the row.
  projected_campom_mean?: number | null;
  projected_campom_lower?: number | null;
  projected_campom_upper?: number | null;
  /// Transfer destination institution name (text label from 247).
  destination?: string | null;
  /// Base-season UUID of the destination team — set when destination
  /// resolved to a D-I program in base_season, null for non-D1 dests.
  /// The frontend uses this for `/teams/{id}?season={year}`; the route
  /// transparently re-resolves to the projected-season team via
  /// `natstat_id` so the cross-season hop is a single round-trip.
  destination_team_id?: string | null;
}
export interface ProjectedUncertain {
  player_id: string;
  name: string;
  reason: string;
  // Source-season MPG / CamPom from the player's PlayerRow on the
  // base-season roster (always populated for uncertain since the
  // bucket only contains qualified returners — same gate as
  // ProjectedReturning).
  mpg: number;
  cam_v3: number | null;
  primary_class: string | null;
  // Per-player projection (route projects uncertain players alongside
  // returners since they *are* returners under the ceiling scenario).
  // null when the player wasn't found in the trajectory feature fetch
  // (gate failure) or batch inference failed.
  projected_campom_mean: number | null;
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
  /// Tankathon mock-draft pick number when the player is on the current
  /// snapshot, else null. Phase 1 surface — informational chip on each
  /// ? row, no auto-promotion to "gone". Should be removed once the
  /// withdrawal deadline passes (early June) since by then every player
  /// is gone/staying definitively.
  mock_pick?: number | null;
  /// NBA team code from the same snapshot (e.g. "WAS"). Surfaces in the
  /// tooltip alongside mock_pick.
  mock_team?: string | null;
}

export function fetchProjectedTeam(year: number, teamId: string) {
  return fetchJson<{
    year: number;
    base_season: number;
    team: { id: string; name: string | null; short_name: string | null };
    projection: ProjectedTeam;
    returning: ProjectedReturning[];
    arrivals: ProjectedArrival[];
    recruits: ProjectedRecruitDetail[];
    departures: ProjectedDeparture[];
    uncertain: ProjectedUncertain[];
  }>(`/projections/${year}/teams/${teamId}`);
}

export function fetchProjections(year: number) {
  return fetchJson<{
    year: number;
    base_season: number;
    teams: ProjectedTeam[];
    total: number;
  }>(`/projections/${year}`);
}

export interface LeagueAverages {
  avg_ppg: number | null;
  avg_game_score: number | null;
}

export interface TorkvikStats {
  // Impact metrics
  gbpm: number | null;
  ogbpm: number | null;
  dgbpm: number | null;
  stops: number | null;
  // Efficiency
  adj_oe: number | null;
  adj_de: number | null;
  // Shot zones
  rim_pct: number | null;
  rim_made: number | null;
  rim_attempted: number | null;
  mid_pct: number | null;
  mid_made: number | null;
  mid_attempted: number | null;
  dunk_pct: number | null;
  dunks_made: number | null;
  dunks_attempted: number | null;
  two_p_pct: number | null;
  tp_pct: number | null;
  tpm: number | null;
  tpa: number | null;
  // Rates (possession-based)
  orb_pct: number | null;
  drb_pct: number | null;
  stl_pct: number | null;
  blk_pct: number | null;
  ft_rate: number | null;
  personal_foul_rate: number | null;
  // Shooting volume
  ftm: number | null;
  fta: number | null;
  two_pm: number | null;
  two_pa: number | null;
  // Context
  recruiting_rank: number | null;
  hometown: string | null;
  // CamPom (canonical site-wide composite)
  campom: number | null;
  campom_pct: number | null;
  // Percentiles
  gbpm_pct: number | null;
  ogbpm_pct: number | null;
  dgbpm_pct: number | null;
  adj_oe_pct: number | null;
  adj_de_pct: number | null;
  orb_pct_pct: number | null;
  drb_pct_pct: number | null;
  stl_pct_pct: number | null;
  blk_pct_pct: number | null;
  ft_rate_pct: number | null;
  fc_rate_pct: number | null;
  // Shot zone percentiles
  rim_pct_pct: number | null;
  mid_pct_pct: number | null;
  dunk_pct_pct: number | null;
  tp_pct_pct: number | null;
}

export interface PlayerArchetype {
  primary_class: string;
  secondary_class: string | null;
  primary_score: number;
  secondary_score: number | null;
  affinity_scores: Record<string, number>;
  cluster_id: number;
}

export interface SimilarPlayer {
  player_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  primary_class: string;
  secondary_class: string | null;
  distance: number;
  similarity: number;
}

/// Phase 5c projection: next-season CamPom estimate with a quantile band.
/// `null` when the player doesn't pass the qualification gate (≥5 GP /
/// ≥5 MPG) for the requested season, or when they have no prior-season
/// CamPom to project from. Honest framing: pooled LOPO MAE is ~2.3
/// CamPom points; render as a directional projection, not a point
/// estimate. Band width is what tells the user how much signal there is.
export interface PlayerTrajectory {
  base_season: number;
  target_season: number;
  projected_mean: number;
  projected_lower: number;
  projected_upper: number;
  prior_campom: number | null;
}

export function fetchPlayerDetail(id: string, season?: number) {
  return fetchJson<{
    player: PlayerProfile;
    season_stats: PlayerSeasonStats | null;
    percentiles: Percentiles | null;
    game_log: GameLogEntry[];
    league_averages: LeagueAverages | null;
    torvik_stats: TorkvikStats | null;
    archetype: PlayerArchetype | null;
    /// Seasons in which this player (joined cross-season via natstat_id) has
    /// any row. Drives the page-scoped season dropdown override.
    available_seasons: number[];
    trajectory: PlayerTrajectory | null;
  }>(`/players/${id}`, { season: season?.toString() });
}

/// One per-season entry in the career progression view. Mirrors the
/// shape `/api/players/{id}` returns for a single season, minus the
/// game_log and league_averages — the progression page renders
/// season-over-season aggregates and side-by-side radars/shot diets,
/// not per-game logs.
export interface ProgressionSeason {
  season: number;
  player_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  position: string | null;
  class_year: string | null;
  jersey_number: string | null;
  height_inches: number | null;
  weight_lbs: number | null;
  season_stats: PlayerSeasonStats | null;
  percentiles: Percentiles | null;
  torvik_stats: TorkvikStats | null;
  archetype: PlayerArchetype | null;
}

export function fetchPlayerProgression(id: string) {
  return fetchJson<{
    available_seasons: number[];
    seasons: ProgressionSeason[];
    trajectory: PlayerTrajectory | null;
  }>(`/players/${id}/progression`);
}

export function fetchPlayerSimilar(id: string, k = 8, season?: number) {
  return fetchJson<{ season: number; players: SimilarPlayer[] }>(
    `/players/${id}/similar`,
    { k: k.toString(), season: season?.toString() },
  );
}

export interface ArchetypeExemplar {
  player_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  primary_score: number;
}

export interface ArchetypeClassInfo {
  name: string;
  count: number;
  mean_campom: number | null;
  exemplars: ArchetypeExemplar[];
}

export function fetchArchetypes(perClass = 5, season?: number) {
  return fetchJson<{ season: number; classes: ArchetypeClassInfo[] }>(
    '/archetypes',
    { per_class: perClass.toString(), season: season?.toString() },
  );
}

export interface ComparePlayer {
  player: PlayerProfile;
  season_stats: PlayerSeasonStats | null;
  percentiles: Percentiles | null;
  game_log: GameLogEntry[];
  torvik_stats: TorkvikStats | null;
  archetype: PlayerArchetype | null;
}

export function fetchPlayerCompare(ids: string[], season?: number) {
  return fetchJson<{
    season: number;
    league_averages: LeagueAverages | null;
    players: ComparePlayer[];
  }>('/players/compare', {
    ids: ids.join(','),
    season: season?.toString(),
  });
}

// Predict
export type Venue = 'home' | 'away' | 'neutral';

export interface FeatureContribution {
  /// Raw feature name (e.g. `diff_w_gbpm`) for keying / debugging.
  name: string;
  /// Human-readable label rendered in the UI ("Roster GBPM").
  label: string;
  /// Group bucket ("Roster impact", "Adjusted efficiency", …).
  group: string;
  /// The diff feature value (home − away) at prediction time.
  value: number;
  /// Ablation delta: how much this feature pushed the margin off zero.
  /// Positive = pushed toward home_team, negative = toward away_team.
  contribution: number;
}

export interface GroupContribution {
  group: string;
  contribution: number;
  feature_count: number;
}

export interface PredictionResult {
  home_team: string;
  home_team_id: string;
  away_team: string;
  away_team_id: string;
  season: number;
  venue: Venue;
  /// YYYY-MM-DD cutoff the prediction was built for, when the request
  /// carried one. Null on legacy (end-of-season-state) responses.
  as_of_date?: string | null;
  /// Server-side label for which regime produced the response.
  /// "leaky" = end-of-season-state bundle (no as_of_date).
  /// With an as_of_date the honest path applies the preseason × pit blend
  /// (ROADMAP §6): "preseason" = full preseason-projection weight (early,
  /// pre-Nov-1 cutoffs), "blended" = decaying mix, "pit" = pure
  /// point-in-time (mid-January onward, or when a team has no preseason
  /// projection row). Read this rather than inferring from local state so
  /// UI honesty claims always match what the server actually served.
  prediction_basis: 'preseason' | 'blended' | 'pit' | 'leaky';
  predicted_margin: number;
  home_win_probability: number;
  /// Total points (home + away). Materially less precise than margin
  /// (backtest MAE ~13.6 vs ~8.2) — frame as KenPom-style approximation.
  predicted_total: number;
  /// Integer projected scores. Rounded so home + away == round(total).
  predicted_home_score: number;
  predicted_away_score: number;
  predicted_winner: string;
  /// Every feature, sorted by |contribution| desc. **No current consumer**
  /// — the Keys panel that used to render these was removed (see the
  /// "Deprecate TreeSHAP infrastructure" entry in ROADMAP.md). Field
  /// stays in the response to avoid breaking the API contract; can be
  /// dropped together with the rest of the SHAP path when that work
  /// lands.
  feature_contributions: FeatureContribution[];
  /// Model's signed group sums. Same status as `feature_contributions`
  /// above — no current consumer, kept for API stability, slated for
  /// removal alongside the rest of the SHAP path.
  contributions_by_group: GroupContribution[];
  /// Roster summary per team (full RosterEntry shape). Sorted CamPom desc
  /// by the underlying query — slice top N on the frontend for display.
  roster_home: RosterEntry[];
  roster_away: RosterEntry[];
  /// Minute-weighted archetype distribution per team. Same shape /
  /// methodology as the field on the team-detail endpoint — drives
  /// the per-team RosterWaffle panels on the Predict page.
  archetype_distribution_home: ArchetypeShare[];
  archetype_distribution_away: ArchetypeShare[];
  /// Completed games between these two teams this season, newest first.
  /// Empty when they haven't played yet.
  prior_meetings: PriorMeeting[];
}

export interface PriorMeetingHeadline {
  game_id: string;
  game_date: string;
  home_team_id: string | null;
  home_team_name: string | null;
  away_team_id: string | null;
  away_team_name: string | null;
  home_score: number | null;
  away_score: number | null;
  is_neutral_site: boolean;
  is_postseason: boolean | null;
}

export interface TeamGameBox {
  game_id: string;
  team_id: string;
  points: number | null;
  fgm: number | null;
  fga: number | null;
  tpm: number | null;
  tpa: number | null;
  ftm: number | null;
  fta: number | null;
  off_rebounds: number | null;
  total_rebounds: number | null;
  assists: number | null;
  steals: number | null;
  blocks: number | null;
  turnovers: number | null;
  fouls: number | null;
}

export interface PlayerGameBox {
  game_id: string;
  player_id: string;
  player_name: string;
  team_id: string;
  starter: boolean | null;
  minutes: number | null;
  points: number | null;
  fgm: number | null;
  fga: number | null;
  tpm: number | null;
  tpa: number | null;
  ftm: number | null;
  fta: number | null;
  off_rebounds: number | null;
  def_rebounds: number | null;
  total_rebounds: number | null;
  assists: number | null;
  steals: number | null;
  blocks: number | null;
  turnovers: number | null;
  fouls: number | null;
  game_score: number | null;
}

export interface PriorMeeting {
  headline: PriorMeetingHeadline;
  team_box: TeamGameBox[];
  player_box: PlayerGameBox[];
}

export function fetchPrediction(
  home: string,
  away: string,
  venue: Venue,
  season?: number,
  asOfDate?: string,
) {
  return fetchJson<PredictionResult>('/predict', {
    home,
    away,
    venue,
    season: season?.toString(),
    // YYYY-MM-DD. Backend rebuilds CamPom v3 from per-game Torvik rows
    // up to this date and serves the pit-trained margin/win/total
    // bundle — see the predict-honesty audit. Omit for the legacy
    // end-of-season prediction.
    as_of_date: asOfDate,
  });
}

// Seasons
export interface SeasonsResponse {
  seasons: number[];
  default: number | null;
}

export function fetchSeasons() {
  return fetchJson<SeasonsResponse>('/seasons');
}

// Games
export interface GameResult {
  game_id: string;
  game_date: string;
  home_team_id: string | null;
  home_team_name: string | null;
  away_team_id: string | null;
  away_team_name: string | null;
  home_score: number | null;
  away_score: number | null;
  is_neutral_site: boolean;
  is_conference: boolean | null;
  is_postseason: boolean | null;
}

export function fetchGames(params: { date?: string; team?: string; season?: number; limit?: number; offset?: number }) {
  return fetchJson<{ season: number; games: GameResult[]; limit: number; offset: number }>(
    '/games',
    {
      date: params.date,
      team: params.team,
      season: params.season?.toString(),
      limit: params.limit?.toString(),
      offset: params.offset?.toString(),
    },
  );
}

// Score ticker — recent completed + soonest upcoming with per-game predictions
export interface UpcomingTile extends GameResult {
  predicted_margin: number;
  home_win_probability: number;
  predicted_home_score: number;
  predicted_away_score: number;
}

export interface TickerResponse {
  season: number;
  past: GameResult[];
  upcoming: UpcomingTile[];
}

export function fetchTicker(params: { season?: number; past?: number; future?: number } = {}) {
  return fetchJson<TickerResponse>('/ticker', {
    season: params.season?.toString(),
    past: params.past?.toString(),
    future: params.future?.toString(),
  });
}

// Coaches — Coach-Above-Expectation (CAE). Descriptive grade: how much a team
// out/under-performs the talent on its roster, attributed to the coach,
// aggregated across their career with empirical-Bayes shrinkage. Headline =
// `cae_shrunk`; always show the credibility band — it is NOT a predictor.
export interface CoachLeaderboardRow {
  coach_id: string;
  name: string;
  cae_shrunk: number;
  cae_raw_mean: number;
  cae_adj_shrunk: number;
  reliability: number;
  ci_low: number;
  ci_high: number;
  n_seasons: number;
  first_season: number;
  last_season: number;
  last_team_id: string | null;
  last_team_name: string | null;
  last_team_season: number | null;
}

export function fetchCoaches(
  params: { minSeasons?: number; limit?: number; season?: number } = {},
) {
  return fetchJson<{
    mode: 'career';
    min_seasons: number;
    season: number | null;
    available_seasons: number[];
    coaches: CoachLeaderboardRow[];
  }>('/coaches', {
    min_seasons: params.minSeasons?.toString(),
    limit: params.limit?.toString(),
    season: params.season?.toString(),
  });
}

// Season-mode leaderboard: that year's single-season CAE, ranked by raw
// residual. Noisier than the career board (single seasons are mostly noise) —
// framed as a "who overachieved this year" view, not a trustworthy rating.
export interface CoachSeasonLeaderboardRow {
  coach_id: string;
  name: string;
  season: number;
  team_id: string | null;
  team_name: string | null;
  actual_adjem: number;
  projection: number;
  cae_raw: number;
  cae_debiased: number;
  is_new_hc: boolean | null;
}

export function fetchCoachSeasonBoard(season: number, limit?: number) {
  return fetchJson<{
    mode: 'season';
    season: number;
    available_seasons: number[];
    coaches: CoachSeasonLeaderboardRow[];
  }>('/coaches', {
    mode: 'season',
    season: season.toString(),
    limit: limit?.toString(),
  });
}

export interface CoachRating {
  coach_id: string;
  name: string;
  cae_shrunk: number;
  cae_raw_mean: number;
  cae_adj_shrunk: number;
  cae_adj_mean: number;
  reliability: number;
  ci_low: number;
  ci_high: number;
  n_seasons: number;
  first_season: number;
  last_season: number;
}

export interface CoachSeasonRow {
  season: number;
  team_id: string | null;
  team_name: string | null;
  actual_adjem: number;
  projection: number;
  cae_raw: number;
  cae_debiased: number;
  is_new_hc: boolean | null;
}

export function fetchCoachDetail(id: string) {
  return fetchJson<{ rating: CoachRating | null; seasons: CoachSeasonRow[] }>(`/coaches/${id}`);
}

// The TeamDetail coach card. Rating fields are null when the coach never
// landed in the scored backtest (coachdict match but no CAE rating).
export interface TeamCoachCard {
  coach_id: string;
  name: string;
  is_new_hc: boolean | null;
  cae_shrunk: number | null;
  reliability: number | null;
  ci_low: number | null;
  ci_high: number | null;
  n_seasons: number | null;
  first_season: number | null;
  last_season: number | null;
}

export function fetchTeamCoach(teamId: string) {
  return fetchJson<{ coach: TeamCoachCard | null }>(`/teams/${teamId}/coach`);
}
