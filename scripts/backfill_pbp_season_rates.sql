-- One-off backfill: Tier-1 PBP season rates (migration 036) for historical
-- seasons. Replicates compute.rs::compute_pbp_aggregates' season-rate rollup
-- and compute_player_percentiles' 7 new percentile columns, verbatim, for a
-- single season passed as :season. The per-game tag columns are already
-- populated for all non-gated seasons; only the rollup post-dates them.
-- Idempotent — the next `cstat-ingest compute --year` run rewrites identically.

\set ON_ERROR_STOP on

-- 1. Clean-recompute the season rates (mirrors compute_pbp_aggregates).
UPDATE player_season_stats
SET paint_rate = NULL, paint_fg_pct = NULL, perimeter_fg_pct = NULL,
    transition_pts_per40 = NULL, second_chance_pts_per40 = NULL,
    points_off_turnovers_per40 = NULL, fouls_drawn_per40 = NULL
WHERE season = :season;

UPDATE player_season_stats pss
SET paint_rate                 = r.paint_rate,
    paint_fg_pct               = r.paint_fg_pct,
    perimeter_fg_pct           = r.perimeter_fg_pct,
    transition_pts_per40       = r.transition_pts_per40,
    second_chance_pts_per40    = r.second_chance_pts_per40,
    points_off_turnovers_per40 = r.points_off_turnovers_per40,
    fouls_drawn_per40          = r.fouls_drawn_per40
FROM (
    SELECT
        player_id, team_id,
        CASE WHEN sum(paint_fga + perimeter_fga) > 0
             THEN sum(paint_fga)::double precision / sum(paint_fga + perimeter_fga)
        END AS paint_rate,
        CASE WHEN sum(paint_fga) > 0
             THEN LEAST(1.0, sum(paint_fgm)::double precision / sum(paint_fga))
        END AS paint_fg_pct,
        CASE WHEN sum(perimeter_fga) > 0
             THEN LEAST(1.0, sum(perimeter_fgm)::double precision / sum(perimeter_fga))
        END AS perimeter_fg_pct,
        sum(transition_pts)       * 40.0 / nullif(sum(minutes), 0) AS transition_pts_per40,
        sum(second_chance_pts)    * 40.0 / nullif(sum(minutes), 0) AS second_chance_pts_per40,
        sum(points_off_turnovers) * 40.0 / nullif(sum(minutes), 0) AS points_off_turnovers_per40,
        sum(fouls_drawn)          * 40.0 / nullif(sum(minutes), 0) AS fouls_drawn_per40
    FROM player_game_stats
    WHERE season = :season AND paint_fga IS NOT NULL
    GROUP BY player_id, team_id
) r
WHERE pss.player_id = r.player_id
  AND pss.team_id = r.team_id
  AND pss.season = :season;

-- 2. The 7 new percentile columns on the existing player_percentiles rows
--    (mirrors the Tier-1 block of compute_player_percentiles, including the
--    DISTINCT ON best-row choice and the non-NULL-only rank denominator).
UPDATE player_percentiles pp
SET paint_rate_pct                 = p.paint_rate_pct,
    paint_fg_pct_pct               = p.paint_fg_pct_pct,
    perimeter_fg_pct_pct           = p.perimeter_fg_pct_pct,
    transition_pts_per40_pct       = p.transition_pts_per40_pct,
    second_chance_pts_per40_pct    = p.second_chance_pts_per40_pct,
    points_off_turnovers_per40_pct = p.points_off_turnovers_per40_pct,
    fouls_drawn_per40_pct          = p.fouls_drawn_per40_pct
FROM (
    WITH best AS (
        SELECT DISTINCT ON (player_id)
            player_id,
            paint_rate, paint_fg_pct, perimeter_fg_pct,
            transition_pts_per40, second_chance_pts_per40,
            points_off_turnovers_per40, fouls_drawn_per40
        FROM player_season_stats
        WHERE season = :season
          AND games_played >= 10
          AND minutes_per_game >= 10
        ORDER BY player_id, games_played DESC, team_id
    )
    SELECT
        player_id,
        CASE WHEN paint_rate IS NULL THEN NULL ELSE (rank() OVER (ORDER BY paint_rate) - 1.0) / nullif(count(paint_rate) OVER () - 1, 0) END AS paint_rate_pct,
        CASE WHEN paint_fg_pct IS NULL THEN NULL ELSE (rank() OVER (ORDER BY paint_fg_pct) - 1.0) / nullif(count(paint_fg_pct) OVER () - 1, 0) END AS paint_fg_pct_pct,
        CASE WHEN perimeter_fg_pct IS NULL THEN NULL ELSE (rank() OVER (ORDER BY perimeter_fg_pct) - 1.0) / nullif(count(perimeter_fg_pct) OVER () - 1, 0) END AS perimeter_fg_pct_pct,
        CASE WHEN transition_pts_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY transition_pts_per40) - 1.0) / nullif(count(transition_pts_per40) OVER () - 1, 0) END AS transition_pts_per40_pct,
        CASE WHEN second_chance_pts_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY second_chance_pts_per40) - 1.0) / nullif(count(second_chance_pts_per40) OVER () - 1, 0) END AS second_chance_pts_per40_pct,
        CASE WHEN points_off_turnovers_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY points_off_turnovers_per40) - 1.0) / nullif(count(points_off_turnovers_per40) OVER () - 1, 0) END AS points_off_turnovers_per40_pct,
        CASE WHEN fouls_drawn_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY fouls_drawn_per40) - 1.0) / nullif(count(fouls_drawn_per40) OVER () - 1, 0) END AS fouls_drawn_per40_pct
    FROM best
) p
WHERE pp.player_id = p.player_id
  AND pp.season = :season;
