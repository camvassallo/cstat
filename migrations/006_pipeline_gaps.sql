-- Fill pipeline gaps: capture fields available from NatStat but not stored,
-- and add columns needed for compute layer rate stats.

-- Player game stats: team context needed for rate stat calculations
ALTER TABLE player_game_stats
    ADD COLUMN IF NOT EXISTS team_fga INT,
    ADD COLUMN IF NOT EXISTS team_fta INT,
    ADD COLUMN IF NOT EXISTS team_turnovers INT,
    ADD COLUMN IF NOT EXISTS team_fgm INT;

-- Games: fields available from NatStat but not being captured
ALTER TABLE games
    ADD COLUMN IF NOT EXISTS home_half1 INT,
    ADD COLUMN IF NOT EXISTS home_half2 INT,
    ADD COLUMN IF NOT EXISTS away_half1 INT,
    ADD COLUMN IF NOT EXISTS away_half2 INT;

-- Update overtime/attendance/status/venue_code which exist but are never populated
-- (they were added in migration 002 but the ingestion code wasn't mapping them)

-- Team game stats: add def_rebounds (total - off) for compute use
ALTER TABLE team_game_stats
    ADD COLUMN IF NOT EXISTS def_rebounds INT;
