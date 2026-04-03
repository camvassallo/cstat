-- Team-level box scores per game (from NatStat teamperfs endpoint)
CREATE TABLE IF NOT EXISTS team_game_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id),
    game_id UUID NOT NULL REFERENCES games(id),
    season INT NOT NULL,
    game_date DATE NOT NULL,
    opponent_id UUID REFERENCES teams(id),
    is_home BOOLEAN,
    win BOOLEAN,
    league TEXT,

    -- Box score
    minutes INT,
    points INT,
    fgm INT,
    fga INT,
    tpm INT,
    tpa INT,
    ftm INT,
    fta INT,
    off_rebounds INT,
    total_rebounds INT,
    assists INT,
    steals INT,
    blocks INT,
    turnovers INT,
    fouls INT,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (team_id, game_id)
);

CREATE INDEX idx_tgs_team ON team_game_stats (team_id, season, game_date);
CREATE INDEX idx_tgs_game ON team_game_stats (game_id);
