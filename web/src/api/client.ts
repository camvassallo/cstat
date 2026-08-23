// ---------------------------------------------------------------------------
// BRAND NAME MAPPING — read before renaming anything in this file.
//
// The site displays Camalytics' player-value metric as **CAM**, with its
// offensive and defensive halves as **CAMO** and **CAMD**. The database
// columns, API payload fields, ML feature names, and every Rust/Python symbol
// still use the original `campom` / `cam_gbpm_v3` vocabulary, and are NOT
// being renamed — the wire contract below deliberately keeps the backend
// spelling.
//
//     wire field        UI label
//     campom            CAM
//     campom_o          CAMO
//     campom_d          CAMD
//     campom_pct        CAM percentile
//
// Presentation-side naming (tiers, colors, tooltips) lives in
// `components/cam.ts`. Keep the translation at that boundary: this file speaks
// the backend's language, the components speak the brand's.
// ---------------------------------------------------------------------------

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
  // CamPom O/D decomposition (o + d = campom; d positive-good). Null outside
  // the ±30 sanity envelope — a regression guard; the compute-side SOS
  // allocation is bounded since the 2026-06-12 magnitude-share fix.
  campom_o: number | null;
  campom_d: number | null;
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
  // `*_class` is the player's ARCHETYPE (the 12 D&D-class profiles: Wizard,
  // Rogue, …), not the class year. UI labels it "Archetype"; the field keeps
  // the legacy `class` name from the DB/API. See docs/archetypes_methodology.md
  // "Naming" and the ROADMAP rename item.
  primary_class: string | null;
  secondary_class: string | null;
  // Cold-start (PR 3a/3b): true when the archetype is a prior-season seed held
  // until the player clears this season's >=10 GP gate; source_season is the
  // year it was carried over from.
  provisional?: boolean | null;
  source_season?: number | null;
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
  /// PBP on/off splits (from `player_on_off`): team net rating per 100 poss with
  /// vs without the player, and the on−off swing. `null` for a player with no
  /// PBP-derived on/off row. `on_off_source` (natstat_lineups/onfloor/replay)
  /// carries the lineup-accuracy caveat; `on_off_off_poss` is the off-court possession
  /// sample (thin for heavy-minute starters).
  net_on_off: number | null;
  on_net_rtg: number | null;
  off_net_rtg: number | null;
  on_off_source: string | null;
  on_off_off_poss: number | null;
  /// RAPM (adjusted on/off) — displayed in the roster's Adv view as
  /// RAPM / RAPM-O / RAPM-D (d = points allowed, lower-better); raw on/off
  /// stays for tooltip context. `rapm_paired_poss` feeds the ~250-poss floor.
  rapm_net: number | null;
  rapm_o: number | null;
  rapm_d: number | null;
  rapm_paired_poss: number | null;
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
  /** Stable per-season NatStat key. `player_id` is a surrogate UUID that can be
   *  re-minted by a data rebuild + resync, so anything that must stay put across
   *  data updates (e.g. Portle's daily seed, issue #181) keys on this instead. */
  natstat_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  conference: string | null;
  position: string | null;
  class_year: string | null;
  // Listed height in inches — surfaced for the Portle Height column.
  height_inches: number | null;
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
  // O/D decomposition, ±30 sanity envelope (see RosterEntry note).
  campom_o: number | null;
  campom_d: number | null;
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
  // Cold-start (PR 3a/3b): prior-season seed flag + the year it came from.
  provisional?: boolean | null;
  source_season?: number | null;
  // PBP on/off (team net per 100 poss with vs without the player). See onoff.ts.
  net_on_off: number | null;
  on_net_rtg: number | null;
  off_net_rtg: number | null;
  on_off_source: string | null;
  on_off_off_poss: number | null;
  // RAPM — served but not displayed on this grid (lives on team-context
  // surfaces: roster Adv view + PlayerDetail panel).
  rapm_net: number | null;
  rapm_paired_poss: number | null;
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

// Projected players — per-player projected CamPom for an upcoming (not-yet-
// played) season, read from the materialized `player_season_projection` table.
// Returners/transfers link to their base-season detail page; freshmen (recruits)
// have no player page and are non-linked.
// `uncertain` is the fourth cohort (issue #220): a player who is on the
// projected roster only under the ceiling scenario — a declared-but-not-
// withdrawn NBA draft entrant, or, since the NCAA's age-based 5-in-5 rule, a
// senior whose extra year of eligibility is unsettled. Rendered with a `?`
// rather than asserted as a returner.
export type ProjectionSource = 'returning' | 'transfer' | 'freshman' | 'uncertain';

export interface ProjectedPlayer {
  player_id: string;
  name: string;
  source: ProjectionSource;
  team_id: string;
  team_name: string;
  natstat_id: string | null;
  /** Projected CamPom mean (the ranking key). */
  campom: number;
  campom_lower: number | null;
  campom_upper: number | null;
  class_year: string | null;
  primary_archetype: string | null;
  composite_rank: number | null;
  star_rating: number | null;
}

/** Fetch the projected-player ranking for `year` (e.g. 2027), CamPom-mean
 *  descending. `base_season` is the season the projection was composed from. */
export function fetchProjectedPlayers(year: number) {
  return fetchJson<{
    target_season: number;
    base_season: number;
    count: number;
    players: ProjectedPlayer[];
  }>(`/projected-players/${year}`);
}

/** Server-authoritative Portle daily answer (issue #181). The backend pins one
 *  player per (mode, season, local date) and freezes it, so every client fetches
 *  the identical puzzle and it never moves once set. Returns the stable
 *  `natstat_id` of the answer (or null when no player is eligible for that pool),
 *  which the caller resolves against the already-loaded player pool. `date` is
 *  the player's LOCAL calendar date (YYYY-MM-DD), matching the Wordle convention. */
export function fetchPortleDaily(mode: string, season: number, date: string) {
  return fetchJson<{ mode: string; season: number; date: string; natstat_id: string | null }>(
    '/portle/daily',
    { mode, season: season.toString(), date },
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
  // The cstat season the matched player / previous-team rows belong to: the
  // portal year for a normal transfer, an earlier season for a sat-out one
  // (issue #146, e.g. Caden Pierce's Princeton 2025). Used as the ?season=
  // target for the player + previous-team links so they land on the season
  // the player actually played, not the empty portal-cycle year.
  source_season: number | null;
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
  // Source-season PBP on/off at the old school (see onoff.ts). NULL when unmatched.
  net_on_off: number | null;
  on_net_rtg: number | null;
  off_net_rtg: number | null;
  on_off_source: string | null;
  on_off_off_poss: number | null;
  // Source-season RAPM — served but not displayed on the portal grid.
  rapm_net: number | null;
  rapm_paired_poss: number | null;
  // Source-season CamPom O/D decomposition (±30 sanity envelope).
  campom_o: number | null;
  campom_d: number | null;
}

export function fetchTransfers(year: number) {
  return fetchJson<{ year: number; transfers: TransferRow[]; total: number }>(
    `/transfers/${year}`,
  );
}

// NBA Draft big board — one row per draft pick for a given draft cycle year,
// sourced from `data/draft/{year}_big_board.json` (historical years = actual
// draft order; the live year = Tankathon prospect board) and joined to cstat
// players for CamPom + archetype. `campom` / `player_id` / archetypes are null
// for picks with no college row this season (internationals, G-Leaguers, or an
// unmatched name).
export interface DraftProspect {
  draft_rank: number | null;
  name: string;
  current_team: string;
  team_id: string | null;
  team_name: string | null;
  player_id: string | null;
  campom: number | null;
  // CamPom O/D decomposition (o + d = campom; d positive-good). Null when
  // unmatched or where the split is numerically unstable (±30 envelope).
  campom_o: number | null;
  campom_d: number | null;
  // D&D-class archetype (primary / secondary) for the matched player's season.
  // Null when unmatched or the player didn't cluster that season.
  primary_archetype: string | null;
  secondary_archetype: string | null;
  // Cold-start (PR 3c): prior-season seed flag + the year it came from (present
  // only if the matched player's season is in-progress and they're sub-gate).
  provisional?: boolean | null;
  source_season?: number | null;
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
  /// Committed but never played the (completed) target season — a redshirt /
  /// non-enrollment. Only ever true for a graded past season; always false on
  /// the live upcoming projection. Excluded from the projection's scored roster
  /// and contribution sum; surfaced here so the report card can flag it.
  did_not_play?: boolean;
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
  /// Σ base-season (prior) CamPom of the returning players — the
  /// continuity denominator + "prior → projected" tooltip base.
  returning_cam_v3_sum: number;
  /// Σ *projected* next-season CamPom of the returners (trajectory
  /// forecast; the forward value the roster-flow ledger displays).
  returning_projected_cam_v3_sum: number;
  arrivals_count: number;
  /// Σ base-season (prior-school) CamPom of the incoming portal arrivals.
  arrivals_cam_v3_sum: number;
  /// Σ *projected* next-season CamPom of the arrivals (forward frame).
  arrivals_projected_cam_v3_sum: number;
  /// Number of HS recruits committed to this team (class-of-`base_season`).
  /// Each contributes a synthesized PlayerRow carrying the freshman-impact
  /// model's per-recruit projected CamPom.
  recruits_count: number;
  /// Σ *projected* freshman-season CamPom of the recruit class (forward
  /// projection from the freshman-impact model, not prior production).
  recruits_cam_v3_sum: number;
  /// Up to 5 highest-ranked recruits for UI display.
  top_recruits: ProjectedRecruit[];
  uncertain_count: number;
  /// Σ base-season CamPom of the uncertain (declared-draft) cohort —
  /// completes the last-season roster base for the % normalization
  /// (`base = returning + departures + uncertain`).
  uncertain_cam_v3_sum: number;
  departures_count: number;
  /// Σ base-season CamPom across all departures (Sr + portal-out + draft).
  departures_cam_v3_sum: number;
  /// Per-cohort Σ of the base-season CamPom O/D halves (envelope-gated per
  /// player; gated/uncovered players contribute 0 to both). Prior-season
  /// frame — the trajectory model forecasts net only, so these describe
  /// the O/D shape of the talent moving, not a forecast. Recruits have no
  /// prior season, hence no recruit pair.
  returning_cam_o_sum: number;
  returning_cam_d_sum: number;
  arrivals_cam_o_sum: number;
  arrivals_cam_d_sum: number;
  departures_cam_o_sum: number;
  departures_cam_d_sum: number;
  /// Projected next-season offensive / defensive efficiency (absolute ~105,
  /// KenPom convention — lower AdjD is better). NET+SPLIT decomposition of
  /// the headline: AdjEM = AdjO − AdjD, so these reconcile to
  /// `midpoint_adj_em` exactly. Display-only; the served net is untouched.
  /// `null` for too-thin rosters.
  projected_adj_o: number | null;
  projected_adj_d: number | null;
  /// True when (returning + arrivals + recruits) is below the projection
  /// threshold — render '—' instead of the prediction columns.
  too_thin: boolean;
  /// Team's AdjEM at the end of the base season (= year-1, the
  /// just-completed season). The shrinkage anchor for the projection
  /// (blended into `midpoint_adj_em`) and the base for the roster-flow
  /// continuity percentages.
  baseline_adj_em: number | null;
  /// Team's *actual* AdjEM for the projected season itself. Null for the
  /// live/upcoming forecast year (not played yet). Drives the historical
  /// view's "Projected vs Actual" accuracy column.
  actual_adj_em: number | null;

  /// Baseline weight used in the served blend:
  /// `midpoint ≈ baseline_weight·(last-yr AdjEM) + (1−baseline_weight)·roster`.
  /// The stable weight for continuity rosters (0.70 since #325), ramping down
  /// toward 0.55 for roster-overhaul teams (low talent retained). What it
  /// weights changed in #325: no longer last season's AdjEM but a *program
  /// anchor* — last season shrunk toward the program's three-year level by
  /// whatever this year's roster does not corroborate — which is why the
  /// weight roughly doubled while the projection leans LESS on any single
  /// season than before. Below the stable weight = "leaning on the new
  /// roster"; compare against a hair under it, since the f32 → f64 promotion
  /// means the cap never arrives as the exact decimal (see `Projected.tsx`).
  baseline_weight: number;

  // --- Conference for the season being projected. Display + search only. ---
  /// The conference this team plays in during the *projected* season — not the
  /// base season's. For a played season that's the ingested value; for the
  /// upcoming forecast the server lays the curated realignment diff over last
  /// season's league, so Gonzaga reads Pac-12 rather than West Coast. Null for
  /// an independent, an unlabelled team, or one that has left Division I.
  conference: string | null;
  /// The base (prior) season's conference, present **only when the team
  /// changed leagues**. Its presence is the "realigned" signal — the UI badges
  /// on it rather than diffing anything itself.
  prev_conference: string | null;
  /// This program stops playing Division I basketball in the projected season
  /// (e.g. Saint Francis reclassifying to Division III for 2026-27). It still
  /// has a base-season roster so it still appears, but it has no destination
  /// league — distinguishing it from a plain null `conference`.
  left_division_i: boolean;

  // --- Display-only coach grade. NOT part of any AdjEM above. ---
  // A PIT backtest (training/pit_cae_backtest.py) showed an additive coach term
  // beats the projection's noise floor but FAILS a program-persistence null, so
  // the lift is program-level bias, not coaching — the served projection stays
  // roster-only and CAE is shown here purely descriptively.
  /// The coach leading this program into the projected season. Null if
  /// unmatched.
  coach_id: string | null;
  coach_name: string | null;
  /// Career EB-shrunk Coach-Above-Expectation (coach_ratings.cae_shrunk), in
  /// AdjEM points. + = the program has historically beaten its roster
  /// projection under this coach. Null when the coach has no career rating.
  coach_cae_shrunk: number | null;
  /// n/(n+k) credibility weight ∈ [0,1]; low = thin tenure, soft grade.
  coach_cae_reliability: number | null;
  coach_n_seasons: number | null;
  /// Did this coach differ from the program's prior-season HC? (coachdict
  /// `is_new_hc`) — drives the "New HC" badge. Null = can't tell.
  coach_is_new_hc: boolean | null;
  /// For a new hire, the coach's prior-season program (e.g. "South Florida"
  /// for Hodgson → Providence). Null for a first-time/promoted D-I coach.
  coach_prev_team: string | null;
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
  /// 247's listed position (e.g. "PG", "SF", "C"). Null when unset on 247.
  position: string | null;
  // Mean predicted freshman-season CamPom from the freshman-impact
  // model. Same number as the chip on the Recruits tab; null only when
  // whole-batch inference failed (replacement-level fallback, no band).
  projected_cam_v3: number | null;
  // q10/q90 band from the freshman model. Both null on the same
  // fallback path that nulls `projected_cam_v3`.
  projected_campom_lower: number | null;
  projected_campom_upper: number | null;
  /// Committed but never played the (completed) target season — a redshirt /
  /// non-enrollment. Only true for a graded past season; always false on the
  /// live upcoming projection. Excluded from the scored roster + contribution
  /// sum; the card greys and tags it.
  did_not_play?: boolean;
}
export interface ProjectedDeparture {
  /// `left_program` covers the exits no feed reports — signed professionally
  /// abroad, retired, dismissed. Curated by hand in `player_departures`.
  kind: 'senior' | 'transferred' | 'draft_gone' | 'left_program';
  /// Sub-vocabulary for `left_program` only ('pro_overseas', 'pro_other',
  /// 'retired', 'dismissed', 'left_program'). Null on every other kind — they
  /// carry their reason in `kind` itself. Display-only.
  reason?: string | null;
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
  /// Where they went. For `transferred`, the destination institution name
  /// (247 text label); for `left_program`, the free-text pro club or league
  /// ("Valencia (ACB)"), null on a retirement. Unused by the other kinds.
  destination?: string | null;
  /// Base-season UUID of the destination team — set when destination
  /// resolved to a D-I program in base_season, null for non-D1 dests.
  /// The frontend uses this for `/teams/{id}?season={year}`; the route
  /// transparently re-resolves to the projected-season team via
  /// `natstat_id` so the cross-season hop is a single round-trip.
  destination_team_id?: string | null;
}
// Why a player is in the `uncertain` bucket. `draft_declared` is the original
// occupant — declared for the NBA draft, not yet withdrawn — and is the only
// cause for which the Tankathon mock board is evidence. `eligibility_unsettled`
// (issue #220) is a senior whose fifth year is in front of a waiver desk or a
// court; the draft board says nothing about him, so the API sends no mock
// fields and the UI must not render a mock chip.
export type UncertainCause = 'draft_declared' | 'eligibility_unsettled';

export interface ProjectedUncertain {
  player_id: string;
  name: string;
  reason: string;
  cause: UncertainCause;
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
  // O/D decomposition, ±30 sanity envelope (see RosterEntry note).
  campom_o: number | null;
  campom_d: number | null;
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
  // Cold-start (PR 3a): true when this label is a prior-season seed held until
  // the player clears this season's >=10 GP gate; source_season is the year it
  // was carried over from. Optional so older/other payloads stay compatible.
  provisional?: boolean;
  source_season?: number | null;
}

export interface SimilarPlayer {
  player_id: string;
  // The neighbour's own season. Equals the requested season on the default
  // single-season search; on `cross_year` it is the year this row is from, and
  // a cross-era list is unreadable without rendering it.
  season: number;
  name: string;
  team_id: string | null;
  team_name: string | null;
  primary_class: string;
  secondary_class: string | null;
  // Cold-start (PR 3a/3b): prior-season seed flag + the year it came from.
  provisional?: boolean | null;
  source_season?: number | null;
  distance: number;
  similarity: number;
  // Cross-year only: this neighbour is the SAME HUMAN in a different season.
  // Often the single best comp, so it is kept rather than filtered — but it
  // has to be labelled, not rendered as if it were somebody else.
  is_self: boolean;
}

/// Phase 5c projection: next-season CamPom estimate with a quantile band.
/// `null` when the player doesn't pass the qualification gate (≥5 GP /
/// ≥5 MPG) for the requested season, or when they have no prior-season
/// CamPom to project from. Honest framing: pooled LOPO MAE is ~2.1
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

/// A player's play-by-play season profile (from the `player_game_stats`
/// PBP-derived columns): shot location, scoring context, fouls drawn, on-floor
/// plus/minus. `games` is the number of games with play-by-play.
export interface PlayerPbpProfile {
  games: number;
  paint_fga: number;
  paint_fgm: number;
  perimeter_fga: number;
  perimeter_fgm: number;
  transition_pts: number;
  second_chance_pts: number;
  points_off_turnovers: number;
  fouls_drawn: number;
  /// Null when the player never appeared in a tracked 5-man stint — render
  /// "—" rather than a fabricated 0.
  plus_minus_pbp: number | null;

  // Season RATE forms + their within-season percentiles (0..1). Make the raw
  // sums above comparable across players. Null below the percentile gate
  // (low-minute players) or for corruption-gated seasons.
  paint_rate: number | null;
  paint_fg_pct: number | null;
  perimeter_fg_pct: number | null;
  transition_pts_per40: number | null;
  second_chance_pts_per40: number | null;
  points_off_turnovers_per40: number | null;
  fouls_drawn_per40: number | null;
  paint_rate_pct: number | null;
  paint_fg_pct_pct: number | null;
  perimeter_fg_pct_pct: number | null;
  transition_pts_per40_pct: number | null;
  second_chance_pts_per40_pct: number | null;
  points_off_turnovers_per40_pct: number | null;
  fouls_drawn_per40_pct: number | null;
}

export function fetchPlayerPbp(id: string, season?: number) {
  return fetchJson<{ season: number; pbp: PlayerPbpProfile | null }>(
    `/players/${id}/pbp`,
    { season: season?.toString() },
  );
}

/// A player's season on/off splits (from the PBP-derived `player_on_off`
/// rollup): team offense/defense per 100 possessions with vs without him on the
/// floor. `net_on_off` is the on−off swing. `ortg`/`drtg`/`net` are null when a
/// side logged no possessions (a player who never sat has no off-court rate).
/// `source` is `'natstat_lineups'` (exact server-computed units), `'onfloor'`
/// (exact) or `'replay'` (~86%, carries the caveat) — best source seen across
/// the player's games.
///
/// `rapm_*` is the context-adjusted companion ("Adj on/off"): a ridge-regressed
/// adjusted +/- that holds teammates and opponents constant, fixing raw
/// on/off's deep-bench garbage-time bias. Null when no fit exists (e.g. 2019);
/// display only when `rapm_paired_possessions` clears a ~250 floor.
export interface PlayerOnOff {
  games: number;
  on_minutes: number;
  on_possessions_for: number;
  on_possessions_against: number;
  on_points_for: number;
  on_points_against: number;
  on_ortg: number | null;
  on_drtg: number | null;
  on_net_rtg: number | null;
  off_minutes: number;
  off_possessions_for: number;
  off_possessions_against: number;
  off_points_for: number;
  off_points_against: number;
  off_ortg: number | null;
  off_drtg: number | null;
  off_net_rtg: number | null;
  net_on_off: number | null;
  source: string;
  rapm_o: number | null;
  rapm_d: number | null;
  rapm_net: number | null;
  rapm_paired_possessions: number | null;
  /// Season percentile (0..1) of net RAPM among display-qualified players; null when unfit.
  rapm_net_pct: number | null;
  /// Same cohort for the O and D halves. `rapm_d_pct` is inverted server-side
  /// (d_rapm is points allowed, negative = good), so high is good on both.
  rapm_o_pct: number | null;
  rapm_d_pct: number | null;
}

export function fetchPlayerOnOff(id: string, season?: number) {
  return fetchJson<{ season: number; on_off: PlayerOnOff | null }>(
    `/players/${id}/on-off`,
    { season: season?.toString() },
  );
}

export interface LineupRanking {
  lineup: string[];
  player_names: string[];
  player_classes: (string | null)[];
  team_id: string;
  team_name: string;
  stints: number;
  minutes: number;
  plus_minus: number;
  possessions_for: number;
  possessions_against: number;
  ortg: number | null;
  drtg: number | null;
  net_rtg: number | null;
  adj_ortg: number | null;
  adj_drtg: number | null;
  adj_net: number | null;
  source: string;
}

/** Cross-team lineup-combination ranking. `size` is 2 (duos), 3 (trios), or 5
 *  (full lineups). Optional `player` / `team` UUIDs filter to combos containing
 *  that player / belonging to that team. */
export function fetchLineupRankings(opts: {
  size: 2 | 3 | 5;
  season?: number;
  player?: string;
  team?: string;
  minMinutes?: number;
  limit?: number;
  /** `'minutes'` (most-used) or the default `'adj_net'` (best, opponent-adjusted). */
  order?: 'minutes' | 'adj_net';
}) {
  return fetchJson<{
    season: number;
    size: number;
    min_minutes: number;
    lineups: LineupRanking[];
  }>(`/lineups`, {
    season: opts.season?.toString(),
    size: opts.size.toString(),
    player: opts.player,
    team: opts.team,
    min_minutes: opts.minMinutes?.toString(),
    limit: opts.limit?.toString(),
    order: opts.order,
  });
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

/// `crossYear` widens the candidate pool to every ingested season. The target
/// vector still comes from `(id, season)`; each human occupies at most one slot
/// (their nearest season) and the target's own other years come back flagged
/// `is_self`. Opt-in because it costs an order of magnitude more than the
/// single-season search (~280 ms vs ~22 ms) — do not put it on a default path.
export function fetchPlayerSimilar(
  id: string,
  k = 8,
  season?: number,
  crossYear = false,
) {
  return fetchJson<{
    season: number;
    cross_year: boolean;
    players: SimilarPlayer[];
  }>(`/players/${id}/similar`, {
    k: k.toString(),
    season: season?.toString(),
    cross_year: crossYear ? 'true' : undefined,
  });
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

/// Fields every compare slot carries, resolved or not.
interface CompareSlotBase {
  /// The UUID exactly as it was requested. Player UUIDs are season-scoped, so
  /// a cross-year slot resolves to a *different* UUID than it asked for —
  /// this is the only key that lines a response entry up with its slot.
  requested_id: string;
  /// The season this slot is rendered in (its own `@season`, or the
  /// request-level season for a bare UUID).
  season: number;
  /// The name on the REQUESTED UUID's own row, season-independent. Redundant
  /// with `player.name` when the slot resolved; it is the only identity an
  /// unavailable slot has, so the column can say whose year came up empty.
  requested_name: string | null;
  /// Every season this human has a row in (`natstat_id ∪ torvik_pid`, newest
  /// first) — the real options for the slot's season picker, so a year that
  /// cannot work is never offered. Computed from the requested UUID, so it is
  /// stable as the slot's season changes.
  available_seasons: number[];
}

export interface ComparePlayerResolved extends CompareSlotBase {
  available: true;
  player: PlayerProfile;
  season_stats: PlayerSeasonStats | null;
  percentiles: Percentiles | null;
  game_log: GameLogEntry[];
  torvik_stats: TorkvikStats | null;
  archetype: PlayerArchetype | null;
}

/// A slot with no row in its season — most often "not in Division I that
/// year". Returned in place rather than dropped, so the UI can say why a
/// column is empty instead of silently rendering one fewer than was asked for.
/// Carries the same key set as a resolved entry with every STAT field empty, so
/// anything that only reads stats needs no narrowing — `available` and a null
/// `player` are what distinguish it. The two `CompareSlotBase` identity fields
/// are still populated, which is what lets the empty column name its player and
/// offer the years that would fill it.
export interface ComparePlayerUnavailable extends CompareSlotBase {
  available: false;
  player: null;
  season_stats: null;
  percentiles: null;
  game_log: [];
  torvik_stats: null;
  archetype: null;
}

export type ComparePlayer = ComparePlayerResolved | ComparePlayerUnavailable;

/// `ids` entries may be a bare UUID (rendered in `season`) or `<uuid>@<year>`
/// for a per-slot season.
export function fetchPlayerCompare(ids: string[], season?: number) {
  return fetchJson<{
    season: number;
    league_averages: LeagueAverages | null;
    /// One entry per distinct slot season, keyed by year. "vs league average"
    /// shading has to be measured against the slot's own era.
    league_averages_by_season: Record<string, LeagueAverages | null>;
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
  /// "cross_era" = the two slots named different seasons — a what-if matchup,
  /// always served from whole-season state (no point-in-time, no preseason
  /// blend), with no prior meetings and the conference flag forced off.
  prediction_basis: 'preseason' | 'blended' | 'pit' | 'leaky' | 'cross_era';
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

export interface PredictRequest {
  home: string;
  away: string;
  venue: Venue;
  /// Site-wide season. Both sides fall back to it, so a request that names
  /// nothing else is the single-season path the page has always sent.
  season?: number;
  /// Per-side season overrides. Naming two different years is a cross-era
  /// what-if: the two teams never met, so the backend forces the conference
  /// flag off, skips the preseason blend and returns no prior meetings, and
  /// labels the answer `prediction_basis: 'cross_era'`. Sending these two is
  /// mutually exclusive with `asOfDate` — point-in-time state is built inside
  /// one season and the backend rejects the combination with a 400.
  homeSeason?: number;
  awaySeason?: number;
  /// YYYY-MM-DD. Backend rebuilds CamPom v3 from per-game Torvik rows
  /// up to this date and serves the pit-trained margin/win/total
  /// bundle — see the predict-honesty audit. Omit for the legacy
  /// end-of-season prediction.
  asOfDate?: string;
}

export function fetchPrediction(req: PredictRequest) {
  return fetchJson<PredictionResult>('/predict', {
    home: req.home,
    away: req.away,
    venue: req.venue,
    season: req.season?.toString(),
    home_season: req.homeSeason?.toString(),
    away_season: req.awaySeason?.toString(),
    as_of_date: req.asOfDate,
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
  // Era-neutral comparison value (each season's mean residual removed). Use
  // for cross-coach ranking only — NOT an absolute "how much" measure.
  cae_centered_shrunk: number;
  reliability: number;
  ci_low: number;
  ci_high: number;
  n_seasons: number;
  first_season: number;
  last_season: number;
  // Display-only team-strength means over scored seasons (AdjEM is opponent-
  // adjusted, so SOS is baked in). NEVER a projection input. null when no
  // scored season resolved to a team-stats row.
  career_adj_em: number | null;
  career_adj_o: number | null;
  career_adj_d: number | null;
  // Evaluative "results + overperformance" composite: z(CAE) + z(career AdjEM)
  // over the qualified board. A lens, not a truth; null on degenerate pages.
  blend: number | null;
  last_team_id: string | null;
  last_team_name: string | null;
  last_team_season: number | null;
  // Conference of the coach's most recent (or season-scoped) team. Display +
  // search only. null when no team matched or the team carries no conference.
  conference: string | null;
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
  cae_centered: number;
  // That season's team AdjO/AdjD (display-only). actual_adjem is the AdjEM.
  adj_offense: number | null;
  adj_defense: number | null;
  // Single-season "results + overperformance" lens: z(cae_raw) + z(AdjEM) over
  // this season's board. A lens, not a truth; null on degenerate boards.
  blend: number | null;
  is_new_hc: boolean | null;
  // That season's team conference. Display + search only. null when no team
  // matched or the team carries no conference label.
  conference: string | null;
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
  // Era-neutral comparison value — cross-coach ranking only, not absolute.
  cae_centered_shrunk: number;
  cae_centered_mean: number;
  reliability: number;
  ci_low: number;
  ci_high: number;
  n_seasons: number;
  first_season: number;
  last_season: number;
  // Career-mean team strength (display-only; never a projection input).
  career_adj_em: number | null;
  career_adj_o: number | null;
  career_adj_d: number | null;
}

export interface CoachSeasonRow {
  season: number;
  team_id: string | null;
  team_name: string | null;
  // null for unresolved team rows (D-I-transition / ghost seasons).
  actual_adjem: number | null;
  // null for UNGRADED seasons — teams the roster projection dropped (too thin /
  // heavy-portal rebuild), so no CAE could be computed. Render as "not scored".
  projection: number | null;
  cae_raw: number | null;
  cae_debiased: number | null;
  cae_centered: number | null;
  // That season's team AdjO/AdjD (display-only). actual_adjem is the AdjEM.
  adj_offense: number | null;
  adj_defense: number | null;
  is_new_hc: boolean | null;
}

export function fetchCoachDetail(id: string) {
  // `name` is sourced from `coaches` directly, so it's present even for a coach
  // with no career rating (only ungraded seasons); `rating?.name` may be null.
  return fetchJson<{ name: string; rating: CoachRating | null; seasons: CoachSeasonRow[] }>(
    `/coaches/${id}`,
  );
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

/// A team's 5-man on-floor lineup with its season totals, from the
/// PBP-derived `lineup_aggregates` rollup. `source` is `'natstat_lineups'`
/// (exact server-computed units off the captured lineups object), `'onfloor'`
/// (exact API on-floor five) or `'replay'` (~86%-accurate SUB-replay off the
/// CSV).
export interface TeamLineup {
  lineup: string[];
  player_names: string[];
  /// Each player's archetype primary class, aligned by index with lineup /
  /// player_names (null = no archetype). Colors the lineup-waffle squares.
  player_classes: (string | null)[];
  stint_count: number;
  points_for: number;
  points_against: number;
  plus_minus: number;
  /// PBP-derived possessions (P3) and the tempo-free rates off them. ortg/drtg
  /// are points per 100 possessions (same scale as team AdjO/AdjD); net_rtg =
  /// ortg - drtg. null when the lineup logged no possessions of that side.
  possessions_for: number;
  possessions_against: number;
  minutes: number;
  ortg: number | null;
  drtg: number | null;
  net_rtg: number | null;
  source: string;
}

export function fetchTeamLineups(teamId: string, season?: number) {
  return fetchJson<{ season: number; lineups: TeamLineup[] }>(
    `/teams/${teamId}/lineups`,
    { season: season?.toString() },
  );
}
