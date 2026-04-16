-- Add AST% and TOV% percentile columns to player_percentiles.
-- These use the real possession-based formulas already computed in player_season_stats.

ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS ast_pct_pct DOUBLE PRECISION;
ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS tov_pct_pct DOUBLE PRECISION;
