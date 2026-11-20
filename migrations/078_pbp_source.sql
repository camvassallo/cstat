-- Which feed a game's play-by-play came from (docs/natstat_migration_plan.md
-- §4.5 step 4). From 2027 on, CollegeBasketballData's plays replace NatStat's
-- for every game CBBD has (`cstat-ingest cbbd-plays`, the nightly's
-- `cbbd_plays` step), and NatStat's loader skips any game holding 'cbbd' rows,
-- so one game's plays are always one source's and never both.
--
-- Additive: every existing row is NatStat's (the live feed or the CSV
-- export), which the default records without rewriting the table.
--
-- On 'cbbd' rows the on-floor columns hold our player UUIDs, comma-separated,
-- with `cbbd:<athlete id>` for a player not yet mapped to a `players` row
-- (`cstat_core::pbp_replay` reads them as exact lineups). On 'natstat' rows
-- they hold NatStat player codes, as before.

ALTER TABLE play_by_play ADD COLUMN source TEXT NOT NULL DEFAULT 'natstat';
