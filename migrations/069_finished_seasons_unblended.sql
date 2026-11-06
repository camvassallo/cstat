-- A finished season's rating is what happened: the preseason prior tapers to
-- zero by a full season (`compute::tapered_prior_weight`), and a season with no
-- games left carries no prior at all. `compute_adjusted_efficiency` now writes
-- that, but only for a season it recomputes, so this brings the finished ones
-- in line without a recompute. A season still being played is left alone.
UPDATE team_season_stats t
   SET blended_adj_offense           = t.adj_offense,
       blended_adj_defense           = t.adj_defense,
       blended_adj_efficiency_margin = t.adj_efficiency_margin,
       blended_sos                   = t.sos,
       blended_sos_rank              = t.sos_rank,
       preseason_weight              = NULL
 WHERE t.blended_adj_efficiency_margin IS NOT NULL
   AND t.adj_efficiency_margin IS NOT NULL
   AND NOT EXISTS (
       SELECT 1 FROM games g
        WHERE g.season = t.season AND g.status IS DISTINCT FROM 'Final'
   );
