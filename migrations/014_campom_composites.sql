-- CamPom: composite player-valuation columns derived from torvik_player_stats.
-- Computed by `compute_campom` in cstat-core; see docs/campom_methodology.md.
-- Columns live on the same row as their inputs (1:1 with torvik_player_stats).

ALTER TABLE torvik_player_stats
    -- Derived input
    ADD COLUMN IF NOT EXISTS min_per DOUBLE PRECISION,

    -- Step intermediates (kept so the parity gate can diff every stage)
    ADD COLUMN IF NOT EXISTS min_factor DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS mp_factor DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS gp_weight DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS adj_gbpm DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS conf_sos DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS sos_adj DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS adj_gbpm_sos DOUBLE PRECISION,

    -- Tier 1: original (no GP, no SOS)
    ADD COLUMN IF NOT EXISTS cam_gbpm DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_o_gbpm DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_d_gbpm DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS min_adj_gbpm DOUBLE PRECISION,

    -- Tier 2: GP-adjusted
    ADD COLUMN IF NOT EXISTS cam_gbpm_v2 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_o_gbpm_v2 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_d_gbpm_v2 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS min_adj_gbpm_v2 DOUBLE PRECISION,

    -- Tier 3: SOS + GP adjusted (canonical)
    ADD COLUMN IF NOT EXISTS cam_gbpm_v3 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_o_gbpm_v3 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_d_gbpm_v3 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS min_adj_gbpm_v3 DOUBLE PRECISION;

-- The existing `minutes_per_game` column is misnamed: ingestion writes Torvik's
-- `Min_per` (column 4 of the source CSV — share of team minutes, 0–100) into it.
-- Backfill the new semantically-correct `min_per` column from existing rows so
-- we don't need to re-ingest just to run CamPom. The misnomer on
-- `minutes_per_game` is left as-is for a follow-up cleanup; CamPom reads
-- `min_per` and derives actual MP from `total_minutes / games_played`.
UPDATE torvik_player_stats
   SET min_per = minutes_per_game
 WHERE min_per IS NULL
   AND minutes_per_game IS NOT NULL;

-- Rankings query path: ORDER BY cam_gbpm_v3 DESC, filtered by season.
CREATE INDEX IF NOT EXISTS idx_torvik_player_stats_cam_gbpm_v3
    ON torvik_player_stats (season, cam_gbpm_v3 DESC NULLS LAST);
