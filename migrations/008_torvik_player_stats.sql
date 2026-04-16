-- Barttorvik player season stats: advanced metrics not available from NatStat.
-- One row per player per season. Matched to cstat players by name+team+season.

CREATE TABLE IF NOT EXISTS torvik_player_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    player_id UUID REFERENCES players(id),
    torvik_pid INTEGER NOT NULL,
    season INTEGER NOT NULL,
    team_name TEXT NOT NULL,
    conf TEXT,

    -- Bio (NatStat often lacks these)
    class_year TEXT,
    height TEXT,
    jersey_number TEXT,
    player_type TEXT,
    recruiting_rank DOUBLE PRECISION,

    -- Volume
    games_played INTEGER,
    minutes_per_game DOUBLE PRECISION,
    total_minutes DOUBLE PRECISION,

    -- Efficiency / impact
    o_rtg DOUBLE PRECISION,
    d_rtg DOUBLE PRECISION,
    adj_oe DOUBLE PRECISION,
    adj_de DOUBLE PRECISION,
    usage_rate DOUBLE PRECISION,
    bpm DOUBLE PRECISION,
    obpm DOUBLE PRECISION,
    dbpm DOUBLE PRECISION,
    gbpm DOUBLE PRECISION,
    ogbpm DOUBLE PRECISION,
    dgbpm DOUBLE PRECISION,
    porpag DOUBLE PRECISION,
    dporpag DOUBLE PRECISION,
    stops DOUBLE PRECISION,

    -- Shooting
    effective_fg_pct DOUBLE PRECISION,
    true_shooting_pct DOUBLE PRECISION,
    ft_pct DOUBLE PRECISION,
    ft_rate DOUBLE PRECISION,
    two_p_pct DOUBLE PRECISION,
    tp_pct DOUBLE PRECISION,
    rim_pct DOUBLE PRECISION,
    mid_pct DOUBLE PRECISION,
    dunk_pct DOUBLE PRECISION,

    -- Shooting volume
    ftm INTEGER,
    fta INTEGER,
    two_pm INTEGER,
    two_pa INTEGER,
    tpm INTEGER,
    tpa INTEGER,
    rim_made DOUBLE PRECISION,
    rim_attempted DOUBLE PRECISION,
    mid_made DOUBLE PRECISION,
    mid_attempted DOUBLE PRECISION,
    dunks_made DOUBLE PRECISION,
    dunks_attempted DOUBLE PRECISION,

    -- Rates
    orb_pct DOUBLE PRECISION,
    drb_pct DOUBLE PRECISION,
    ast_pct DOUBLE PRECISION,
    tov_pct DOUBLE PRECISION,
    stl_pct DOUBLE PRECISION,
    blk_pct DOUBLE PRECISION,
    personal_foul_rate DOUBLE PRECISION,
    ast_to_tov DOUBLE PRECISION,

    -- Per-game counting
    ppg DOUBLE PRECISION,
    oreb_pg DOUBLE PRECISION,
    dreb_pg DOUBLE PRECISION,
    treb_pg DOUBLE PRECISION,
    ast_pg DOUBLE PRECISION,
    stl_pg DOUBLE PRECISION,
    blk_pg DOUBLE PRECISION,

    -- Meta
    nba_pick DOUBLE PRECISION,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),

    UNIQUE (torvik_pid, season)
);

CREATE INDEX idx_torvik_player_stats_player ON torvik_player_stats (player_id, season);
CREATE INDEX idx_torvik_player_stats_season ON torvik_player_stats (season);
