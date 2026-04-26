-- Drop unused bpm/obpm/dbpm columns left behind after PR #25 replaced cstat's
-- broken BPM/OBPM/DBPM with Torvik GBPM passthrough. The columns have been
-- explicitly NULLed in `compute_individual_ratings` and have no remaining
-- consumers (verified across crates, web, training).
ALTER TABLE player_season_stats
    DROP COLUMN IF EXISTS bpm,
    DROP COLUMN IF EXISTS obpm,
    DROP COLUMN IF EXISTS dbpm;

ALTER TABLE player_percentiles
    DROP COLUMN IF EXISTS bpm_pct;
