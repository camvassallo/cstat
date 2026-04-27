-- CamPom: parallel Tier-3 composite using player-level strength of schedule
-- (cstat's `player_season_stats.player_sos`, minutes-weighted opponent
-- adj-efficiency-margin) instead of conference-level SOS. Per ROADMAP §4f
-- this is a strict-upgrade candidate: same SOS adjustment mechanism, finer
-- signal (a high-major that scheduled cupcakes scores differently than a
-- mid-major that played up).
--
-- Kept as a *parallel* tier (not a replacement) so the existing v3 stays
-- parity-locked against docs/campom_2026_baseline.csv and the predict-model
-- iteration step can A/B both feature sets.

ALTER TABLE torvik_player_stats
    -- Player-level SOS adjustment: player_sos × transfer_rate, in GBPM units.
    -- Note: player_sos is ~2.5x larger magnitude than conf_sos, so the
    -- transfer rate is correspondingly smaller (default 0.15 vs 0.5).
    ADD COLUMN IF NOT EXISTS psos_adj DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS adj_gbpm_psos DOUBLE PRECISION,

    -- Tier-3 composites with player SOS in place of conference SOS.
    ADD COLUMN IF NOT EXISTS cam_gbpm_v3_psos DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_o_gbpm_v3_psos DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS cam_d_gbpm_v3_psos DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS min_adj_gbpm_v3_psos DOUBLE PRECISION;

CREATE INDEX IF NOT EXISTS idx_torvik_player_stats_cam_gbpm_v3_psos
    ON torvik_player_stats (season, cam_gbpm_v3_psos DESC NULLS LAST);
