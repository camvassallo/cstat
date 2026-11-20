-- ESPN ids beside the NatStat ids, so CollegeBasketballData rows (which carry
-- ESPN's ids) join to our teams, players and games without name matching
-- every night (docs/natstat_migration_plan.md §3). Additive: every existing
-- UUID and `natstat_id` stays the key it is today.
--
-- Written by `cstat-ingest map-espn-ids` and the nightly's `espn_ids` step,
-- which never overwrite a set value. Unique per season where set, like
-- `natstat_id`: the entity rows are season-scoped, and ESPN's ids are not (a
-- team's and an athlete's id carry across seasons; that is what will make
-- `players.espn_id` the cross-season key, §4.7).

ALTER TABLE teams ADD COLUMN espn_id TEXT;
ALTER TABLE players ADD COLUMN espn_id TEXT;
ALTER TABLE games ADD COLUMN espn_id TEXT;

CREATE UNIQUE INDEX teams_espn_id_season ON teams (espn_id, season) WHERE espn_id IS NOT NULL;
CREATE UNIQUE INDEX players_espn_id_season ON players (espn_id, season) WHERE espn_id IS NOT NULL;
CREATE UNIQUE INDEX games_espn_id_season ON games (espn_id, season) WHERE espn_id IS NOT NULL;
