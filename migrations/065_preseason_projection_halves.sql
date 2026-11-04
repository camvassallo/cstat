-- The AdjO and AdjD halves of each team's served preseason projection.
--
-- `compute_adjusted_efficiency` uses the projection as a prior on the
-- in-season ratings (#436). With only `projected_adj_em` stored, it had to
-- split that margin evenly around the league average, so every projected team
-- entered the season with the same offense/defense shape: the morning after
-- opening night the top projected teams held the top defenses and ranked 25th
-- to 40th on offense, whatever their actual profile.
--
-- These are the reconciled midpoint halves the Future page already displays
-- (`roster_projection::score_projection_split`), so
-- `projected_adj_o - projected_adj_d = projected_adj_em` exactly. They are on
-- the BASE season's absolute scale; a reader comparing them with another
-- season's ratings subtracts the base season's league means first.
--
-- NULL on rows written before this migration and when either half's model
-- fails; readers fall back to the even split.
ALTER TABLE team_preseason_projection
    ADD COLUMN IF NOT EXISTS projected_adj_o REAL,
    ADD COLUMN IF NOT EXISTS projected_adj_d REAL;
