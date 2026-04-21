-- Add FT rate to player_season_stats and rate-stat percentile columns.

ALTER TABLE player_season_stats ADD COLUMN IF NOT EXISTS ft_rate DOUBLE PRECISION;

ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS orb_pct_pct DOUBLE PRECISION;
ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS drb_pct_pct DOUBLE PRECISION;
ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS stl_pct_pct DOUBLE PRECISION;
ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS blk_pct_pct DOUBLE PRECISION;
ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS ft_rate_pct DOUBLE PRECISION;
