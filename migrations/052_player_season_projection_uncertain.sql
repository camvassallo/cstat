-- Admit a fourth cohort to the per-player projection: 'uncertain'.
--
-- `player_season_projection` (migration 045) materializes the projected-season
-- player rankings the `/players?season=N+1` view reads. Its writer walks
-- `returning + arrivals + recruits` — the three buckets that make up a
-- projected roster under a *resolved* scenario — and the `source` CHECK was
-- written to match.
--
-- That left the `uncertain` bucket unrepresented, which was tolerable when its
-- only occupant was a player who had declared for the NBA draft and not yet
-- withdrawn: a genuinely 50/50 case, in a bucket that empties by late June,
-- whose members are mostly leaving. It stops being tolerable under the NCAA
-- 5-in-5 rule (issue #220), which routes a much larger and more permanent
-- population here — seniors whose extra year of eligibility is unsettled or
-- under litigation, most of whom are expected to PLAY.
--
-- With the old CHECK those players are absent from the projected rankings
-- entirely. That is the same silent-deletion bug the 5-in-5 work exists to fix,
-- just one table further downstream: the team projection widens its band to
-- acknowledge the uncertainty, and then the player page renders as though the
-- player does not exist.
--
-- Widening the constraint rather than reclassifying them as 'returning' is the
-- point — the UI needs to distinguish "projected to play" from "projected to
-- play if he is ruled eligible", which is what earns the `?` marker.
--
-- ALTER ... DROP CONSTRAINT / ADD CONSTRAINT rather than a table rewrite: the
-- existing rows all carry one of the three original values, so the new
-- constraint validates against them without a scan-and-fix step.

ALTER TABLE player_season_projection
    DROP CONSTRAINT IF EXISTS player_season_projection_source_check;

ALTER TABLE player_season_projection
    ADD CONSTRAINT player_season_projection_source_check
    CHECK (source IN ('returning', 'transfer', 'freshman', 'uncertain'));
