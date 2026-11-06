-- The CAM a reader sees while a season is being played (`compute::compute_cam_rating`).
-- NULL outside a season in progress, and for a player with no projection or a
-- full season of games, so every reader takes COALESCE(cam_rating, cam_gbpm_v3_psos)
-- and a finished season is exactly the stat. Display only: no model reads these.
ALTER TABLE torvik_player_stats
    ADD COLUMN cam_rating         DOUBLE PRECISION,
    ADD COLUMN cam_rating_o       DOUBLE PRECISION,
    ADD COLUMN cam_rating_d       DOUBLE PRECISION,
    ADD COLUMN cam_rating_prior_w DOUBLE PRECISION;
