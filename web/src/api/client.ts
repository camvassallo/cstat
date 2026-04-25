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
  def_rebound_pct: number | null;
  def_rebound_pct_rank: number | null;
  opp_ft_rate: number | null;
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
  fg_pct: number | null;
  tp_pct: number | null;
  ft_pct: number | null;
  effective_fg_pct: number | null;
  true_shooting_pct: number | null;
  usage_rate: number | null;
  bpm: number | null;
  offensive_rating: number | null;
  defensive_rating: number | null;
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
  return fetchJson<{ team: TeamProfile; schedule: ScheduleEntry[]; roster: RosterEntry[] }>(
    `/teams/${id}`,
    { season: season?.toString() },
  );
}

// Players
export interface PlayerRow {
  player_id: string;
  name: string;
  team_id: string | null;
  team_name: string | null;
  conference: string | null;
  position: string | null;
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
  bpm: number | null;
  obpm: number | null;
  dbpm: number | null;
  offensive_rating: number | null;
  defensive_rating: number | null;
  net_rating: number | null;
  player_sos: number | null;
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
  bpm: number | null;
  obpm: number | null;
  dbpm: number | null;
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
  bpm_pct: number | null;
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
      limit: params.limit?.toString(),
      offset: params.offset?.toString(),
    },
  );
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

export function fetchPlayerDetail(id: string, season?: number) {
  return fetchJson<{
    player: PlayerProfile;
    season_stats: PlayerSeasonStats | null;
    percentiles: Percentiles | null;
    game_log: GameLogEntry[];
    league_averages: LeagueAverages | null;
    torvik_stats: TorkvikStats | null;
  }>(`/players/${id}`, { season: season?.toString() });
}

export interface ComparePlayer {
  player: PlayerProfile;
  season_stats: PlayerSeasonStats | null;
  percentiles: Percentiles | null;
  game_log: GameLogEntry[];
  torvik_stats: TorkvikStats | null;
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
export interface PredictionResult {
  home_team: string;
  away_team: string;
  predicted_margin: number;
  home_win_probability: number;
  predicted_winner: string;
}

export function fetchPrediction(home: string, away: string, neutral: boolean) {
  return fetchJson<PredictionResult>('/predict', {
    home,
    away,
    neutral: neutral ? 'true' : undefined,
  });
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
