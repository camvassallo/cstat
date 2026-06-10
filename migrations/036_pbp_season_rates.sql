-- Tier-1 PBP tag features: season-level rate forms + their percentiles.
--
-- The per-play tag rollups (paint/perimeter, transition/2nd-chance/off-TO,
-- fouls-drawn) have lived only on `player_game_stats` (per game) and were
-- surfaced raw on the player page — not aggregated to the season, not
-- percentile-ranked, so not comparable across players and redundant with the
-- shot-diet panel. This migration adds the season rollup (as RATES, never raw
-- counts) and its within-season percentiles, the substrate Tier-1 needs to be
-- (a) comparable on the site and (b) a clean model feature.
--
-- WHY RATES (not counts): NatStat's tag DENSITY varies by season (2025 ~53% of
-- player-games carry a paint tag vs 2026 ~74%) — a season effect independent of
-- the CSV-vs-API loader (verified equivalent, see docs/pbp_utilization_scope.md
-- §3). Raw counts would bake that drift into the feature; rates blunt it, and
-- the within-season PERCENT_RANK in player_percentiles removes it entirely (an
-- 80th-percentile paint-rate is 80th regardless of the season's absolute
-- density). That percentile is what the UI shows and what models should prefer.
--
-- Aggregated from player_game_stats (which holds the raw tag counts + fga/fgm/
-- minutes denominators) in compute_player_season_stats; NULL for non-PBP /
-- corruption-gated seasons (the source columns are already NULL there, so the
-- sums collapse to NULL naturally). Ships to prod (player_season_stats and
-- player_percentiles both sync).

ALTER TABLE player_season_stats
    -- Shot location: share of FGA at the rim (style) + finishing efficiency
    -- inside / outside (the NEW, non-redundant-with-diet signal).
    ADD COLUMN IF NOT EXISTS paint_rate                  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS paint_fg_pct                DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS perimeter_fg_pct            DOUBLE PRECISION,
    -- Context scoring, per-40-minutes (pace/role-robust counting-rate form).
    ADD COLUMN IF NOT EXISTS transition_pts_per40        DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS second_chance_pts_per40     DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS points_off_turnovers_per40  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS fouls_drawn_per40           DOUBLE PRECISION;

ALTER TABLE player_percentiles
    ADD COLUMN IF NOT EXISTS paint_rate_pct                  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS paint_fg_pct_pct                DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS perimeter_fg_pct_pct            DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS transition_pts_per40_pct        DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS second_chance_pts_per40_pct     DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS points_off_turnovers_per40_pct  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS fouls_drawn_per40_pct           DOUBLE PRECISION;
