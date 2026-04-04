-- Rolling averages on player_game_stats (last 5 games leading into this game)
ALTER TABLE player_game_stats
    ADD COLUMN IF NOT EXISTS rolling_ppg DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS rolling_rpg DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS rolling_apg DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS rolling_fg_pct DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS rolling_ts_pct DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS rolling_game_score DOUBLE PRECISION;
