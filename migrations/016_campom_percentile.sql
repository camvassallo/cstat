-- 0-100 percentile companion for the canonical site-wide CamPom score
-- (cam_gbpm_v3_psos). Computed across qualified players (>=10 GP, >=10 MPG)
-- in compute_campom. Stored as a fraction 0-1; the API rounds to %.

ALTER TABLE torvik_player_stats
    ADD COLUMN IF NOT EXISTS cam_gbpm_v3_psos_pct DOUBLE PRECISION;
