-- CAM for a player who has played but has no Torvik row: Torvik counts games
-- between Division I teams, so a team that opens against a non-D1 opponent has
-- none yet. `compute::compute_projected_cam` writes his projection here, on
-- `player_season_stats` because `torvik_player_stats` has no row for him and the
-- models read that table. Display only, and NULL outside a season in progress.
ALTER TABLE player_season_stats
    ADD COLUMN projected_cam   DOUBLE PRECISION,
    ADD COLUMN projected_cam_o DOUBLE PRECISION,
    ADD COLUMN projected_cam_d DOUBLE PRECISION;
