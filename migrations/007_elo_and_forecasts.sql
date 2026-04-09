-- Add real ELO rating (from /elo endpoint) alongside existing elo column (which stores rank)
ALTER TABLE team_season_stats
    ADD COLUMN IF NOT EXISTS elo_rating DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS elo_rank INT;

-- Migrate existing elo column (which was rank) into elo_rank, then we can repurpose elo for rating
-- (We'll handle this in application code — elo stays as-is for backward compat, elo_rating is new)

-- Per-game forecasts from /forecasts endpoint
-- Stores pre/post-game ELO, win expectancy, betting lines per team per game
CREATE TABLE IF NOT EXISTS game_forecasts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id UUID NOT NULL REFERENCES games(id),
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- Team references
    home_team_id UUID REFERENCES teams(id),
    away_team_id UUID REFERENCES teams(id),

    -- Pre-game ELO (safe for ML features — represents state before game)
    home_elo_before DOUBLE PRECISION,
    away_elo_before DOUBLE PRECISION,

    -- Post-game ELO (NOT safe for ML features — contains game outcome info)
    home_elo_after DOUBLE PRECISION,
    away_elo_after DOUBLE PRECISION,

    -- ELO-based win expectancy (NatStat's prediction — benchmark only, not ML feature)
    home_win_exp DOUBLE PRECISION,
    away_win_exp DOUBLE PRECISION,

    -- ELO calculation details
    elo_k DOUBLE PRECISION,
    elo_adjust DOUBLE PRECISION,
    elo_points DOUBLE PRECISION,

    -- Betting lines (from Betsson, not available for every game)
    home_moneyline INT,
    away_moneyline INT,
    spread DOUBLE PRECISION,
    spread_favorite_team_id UUID REFERENCES teams(id),
    over_under DOUBLE PRECISION,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (game_id)
);

CREATE INDEX idx_game_forecasts_season ON game_forecasts (season, game_date);
CREATE INDEX idx_game_forecasts_home ON game_forecasts (home_team_id, season);
CREATE INDEX idx_game_forecasts_away ON game_forecasts (away_team_id, season);
