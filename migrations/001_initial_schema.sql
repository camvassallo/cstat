-- Initial schema for cstat

-- Teams
CREATE TABLE IF NOT EXISTS teams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    natstat_id TEXT NOT NULL,
    name TEXT NOT NULL,
    short_name TEXT,
    conference TEXT,
    division TEXT,
    season INT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (natstat_id, season)
);

CREATE INDEX idx_teams_season ON teams (season);
CREATE INDEX idx_teams_conference ON teams (conference, season);

-- Players
CREATE TABLE IF NOT EXISTS players (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    natstat_id TEXT NOT NULL,
    name TEXT NOT NULL,
    team_id UUID REFERENCES teams(id),
    season INT NOT NULL,
    position TEXT,
    height_inches INT,
    weight_lbs INT,
    class_year TEXT,
    jersey_number TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (natstat_id, season)
);

CREATE INDEX idx_players_team ON players (team_id, season);
CREATE INDEX idx_players_name ON players (name);

-- Games
CREATE TABLE IF NOT EXISTS games (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    natstat_id TEXT,
    season INT NOT NULL,
    game_date DATE NOT NULL,
    home_team_id UUID REFERENCES teams(id),
    away_team_id UUID REFERENCES teams(id),
    home_score INT,
    away_score INT,
    is_neutral_site BOOLEAN NOT NULL DEFAULT false,
    is_conference BOOLEAN,
    is_postseason BOOLEAN,
    venue TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (natstat_id)
);

CREATE INDEX idx_games_season_date ON games (season, game_date);
CREATE INDEX idx_games_home_team ON games (home_team_id, season);
CREATE INDEX idx_games_away_team ON games (away_team_id, season);

-- Player game stats (box scores)
CREATE TABLE IF NOT EXISTS player_game_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    player_id UUID NOT NULL REFERENCES players(id),
    game_id UUID NOT NULL REFERENCES games(id),
    team_id UUID NOT NULL REFERENCES teams(id),
    season INT NOT NULL,
    game_date DATE NOT NULL,
    opponent_id UUID REFERENCES teams(id),
    is_home BOOLEAN,
    is_neutral BOOLEAN,

    -- Minutes
    minutes DOUBLE PRECISION,

    -- Scoring
    points INT,
    fgm INT,
    fga INT,
    fg_pct DOUBLE PRECISION,
    tpm INT,
    tpa INT,
    tp_pct DOUBLE PRECISION,
    ftm INT,
    fta INT,
    ft_pct DOUBLE PRECISION,

    -- Rebounds
    off_rebounds INT,
    def_rebounds INT,
    total_rebounds INT,

    -- Playmaking & turnovers
    assists INT,
    turnovers INT,
    ast_to_ratio DOUBLE PRECISION,

    -- Defense
    steals INT,
    blocks INT,
    fouls INT,

    -- Advanced
    offensive_rating DOUBLE PRECISION,
    defensive_rating DOUBLE PRECISION,
    usage_rate DOUBLE PRECISION,
    game_score DOUBLE PRECISION,
    plus_minus INT,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (player_id, game_id)
);

CREATE INDEX idx_pgs_player ON player_game_stats (player_id, season, game_date);
CREATE INDEX idx_pgs_team ON player_game_stats (team_id, season, game_date);
CREATE INDEX idx_pgs_game ON player_game_stats (game_id);

-- Player season stats (aggregated)
CREATE TABLE IF NOT EXISTS player_season_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    player_id UUID NOT NULL REFERENCES players(id),
    team_id UUID NOT NULL REFERENCES teams(id),
    season INT NOT NULL,

    games_played INT NOT NULL DEFAULT 0,
    games_started INT,
    minutes_per_game DOUBLE PRECISION,

    -- Per-game averages
    ppg DOUBLE PRECISION,
    rpg DOUBLE PRECISION,
    apg DOUBLE PRECISION,
    spg DOUBLE PRECISION,
    bpg DOUBLE PRECISION,
    topg DOUBLE PRECISION,
    fpg DOUBLE PRECISION,

    -- Shooting
    fg_pct DOUBLE PRECISION,
    tp_pct DOUBLE PRECISION,
    ft_pct DOUBLE PRECISION,
    effective_fg_pct DOUBLE PRECISION,
    true_shooting_pct DOUBLE PRECISION,

    -- Advanced
    offensive_rating DOUBLE PRECISION,
    defensive_rating DOUBLE PRECISION,
    net_rating DOUBLE PRECISION,
    usage_rate DOUBLE PRECISION,
    bpm DOUBLE PRECISION,
    obpm DOUBLE PRECISION,
    dbpm DOUBLE PRECISION,
    ast_pct DOUBLE PRECISION,
    tov_pct DOUBLE PRECISION,
    orb_pct DOUBLE PRECISION,
    drb_pct DOUBLE PRECISION,
    stl_pct DOUBLE PRECISION,
    blk_pct DOUBLE PRECISION,

    -- Player-specific SOS
    player_sos DOUBLE PRECISION,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (player_id, season)
);

CREATE INDEX idx_pss_team ON player_season_stats (team_id, season);

-- Team season stats
CREATE TABLE IF NOT EXISTS team_season_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id),
    season INT NOT NULL,

    wins INT NOT NULL DEFAULT 0,
    losses INT NOT NULL DEFAULT 0,

    -- Four factors (offense)
    adj_offense DOUBLE PRECISION,
    effective_fg_pct DOUBLE PRECISION,
    turnover_pct DOUBLE PRECISION,
    off_rebound_pct DOUBLE PRECISION,
    ft_rate DOUBLE PRECISION,

    -- Four factors (defense)
    adj_defense DOUBLE PRECISION,
    opp_effective_fg_pct DOUBLE PRECISION,
    opp_turnover_pct DOUBLE PRECISION,
    def_rebound_pct DOUBLE PRECISION,
    opp_ft_rate DOUBLE PRECISION,

    -- Tempo & efficiency
    adj_tempo DOUBLE PRECISION,
    adj_efficiency_margin DOUBLE PRECISION,

    -- Strength of schedule
    sos DOUBLE PRECISION,
    sos_rank INT,

    -- Ratings
    elo DOUBLE PRECISION,
    rpi DOUBLE PRECISION,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (team_id, season)
);

-- Schedule entries
CREATE TABLE IF NOT EXISTS schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id UUID NOT NULL REFERENCES games(id),
    team_id UUID NOT NULL REFERENCES teams(id),
    season INT NOT NULL,
    game_date DATE NOT NULL,
    opponent_id UUID REFERENCES teams(id),
    is_home BOOLEAN,
    is_neutral BOOLEAN,
    team_score INT,
    opponent_score INT,
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (game_id, team_id)
);

CREATE INDEX idx_schedules_team ON schedules (team_id, season, game_date);

-- Player percentile rankings
CREATE TABLE IF NOT EXISTS player_percentiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    player_id UUID NOT NULL REFERENCES players(id),
    season INT NOT NULL,

    ppg_pct DOUBLE PRECISION,
    rpg_pct DOUBLE PRECISION,
    apg_pct DOUBLE PRECISION,
    spg_pct DOUBLE PRECISION,
    bpg_pct DOUBLE PRECISION,
    fg_pct_pct DOUBLE PRECISION,
    tp_pct_pct DOUBLE PRECISION,
    ft_pct_pct DOUBLE PRECISION,
    true_shooting_pct_pct DOUBLE PRECISION,
    usage_rate_pct DOUBLE PRECISION,
    offensive_rating_pct DOUBLE PRECISION,
    defensive_rating_pct DOUBLE PRECISION,
    bpm_pct DOUBLE PRECISION,
    player_sos_pct DOUBLE PRECISION,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (player_id, season)
);

-- API response cache (for NatStat rate limit management)
CREATE TABLE IF NOT EXISTS api_cache (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint TEXT NOT NULL,
    params_hash TEXT NOT NULL,
    response_body JSONB NOT NULL,
    fetched_at TIMESTAMP NOT NULL DEFAULT now(),
    expires_at TIMESTAMP NOT NULL,
    UNIQUE (endpoint, params_hash)
);

CREATE INDEX idx_api_cache_lookup ON api_cache (endpoint, params_hash, expires_at);
