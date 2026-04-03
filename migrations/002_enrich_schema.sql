-- Add fields available from NatStat API that the initial schema missed

-- Players: add hometown, nationality, DOB
ALTER TABLE players ADD COLUMN IF NOT EXISTS hometown TEXT;
ALTER TABLE players ADD COLUMN IF NOT EXISTS nationality TEXT;
ALTER TABLE players ADD COLUMN IF NOT EXISTS date_of_birth DATE;

-- Player game stats: add NatStat advanced metrics
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS starter BOOLEAN;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS efficiency DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS two_fg_pct DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS presence_rate DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS adj_presence_rate DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS perf_score DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS perf_score_season_avg DOUBLE PRECISION;
ALTER TABLE player_game_stats ADD COLUMN IF NOT EXISTS team_possessions INT;

-- Games: add overtime, attendance, game status
ALTER TABLE games ADD COLUMN IF NOT EXISTS overtime TEXT;
ALTER TABLE games ADD COLUMN IF NOT EXISTS attendance INT;
ALTER TABLE games ADD COLUMN IF NOT EXISTS status TEXT;
ALTER TABLE games ADD COLUMN IF NOT EXISTS venue_code TEXT;

-- Team season stats: add TCR fields from NatStat
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS tcr_rank INT;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS tcr_points DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS tcr_adjusted DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS efficiency DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS defense DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS point_diff DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS pythag_win_pct DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS luck DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS opp_win_pct DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS opp_opp_win_pct DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS road_win_pct DOUBLE PRECISION;
ALTER TABLE team_season_stats ADD COLUMN IF NOT EXISTS conference TEXT;
