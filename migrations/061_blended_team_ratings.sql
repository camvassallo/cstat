-- Early-season ratings with the preseason projection as a prior
-- (docs/season_rollover_pages.md, P2).
--
-- The raw columns (`adj_offense` / `adj_defense` / `adj_efficiency_margin`)
-- are NOT changed: they feed the game model's features, CamPom's schedule
-- adjustment and player SOS, all trained on the raw number. These are a
-- separate, display-and-ranking product, written beside them by
-- `compute_adjusted_efficiency`.
--
-- blended_*         ratings with the preseason projection worth K games of
--                   evidence (K fitted walk-forward, see
--                   `training/experiments/experiment_rankings_preseason_prior.py`),
--                   applied inside the opponent adjustment
-- preseason_weight  the weight the projection carries for this team,
--                   K / (K + games played); NULL when the team has no
--                   projection (the blend is then the raw rating)
ALTER TABLE team_season_stats
    ADD COLUMN IF NOT EXISTS blended_adj_offense DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS blended_adj_defense DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS blended_adj_efficiency_margin DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS preseason_weight DOUBLE PRECISION;
