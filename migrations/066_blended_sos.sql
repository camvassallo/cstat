-- Strength of schedule measured on the blended ratings.
--
-- `sos` averages opponents' raw `adj_efficiency_margin`, and `compute_campom`
-- reads it, so it stays as it is. In the first weeks of a season a raw rating
-- is one or two games of noise, which made the column useless beside the
-- blended board it was shown on. These two are the same average over the
-- opponents' blended ratings, for the board alone.
ALTER TABLE team_season_stats
    ADD COLUMN IF NOT EXISTS blended_sos DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS blended_sos_rank INT;
