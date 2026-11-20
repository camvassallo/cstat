-- Which feed wrote a box-score row, so CollegeBasketballData can fill the
-- games NatStat has no box score for while NatStat stays primary
-- (docs/natstat_migration_plan.md §4.5 step 3).
--
-- `source` is 'natstat' (every existing row, and every row the NatStat perf
-- ingest writes) or 'cbbd' (written by `cstat-ingest gap-fill` and the
-- nightly's `gap_fill` step from the `cbbd_*` shadow tables). When NatStat's
-- own box for a game side arrives later, the perf ingest deletes that side's
-- 'cbbd' rows before writing its own, so the two are never summed together.
--
-- `games.score_source` is 'cbbd' when the gap-fill set a game's final score
-- and status (NatStat left it unplayed or stuck at an in-progress status like
-- `H2 05:11`); NULL means the score is NatStat's or the game is unplayed. A
-- NatStat final score clears it.
--
-- Additive: constant defaults, so no table rewrite.

ALTER TABLE player_game_stats ADD COLUMN source TEXT NOT NULL DEFAULT 'natstat';
ALTER TABLE team_game_stats ADD COLUMN source TEXT NOT NULL DEFAULT 'natstat';
ALTER TABLE games ADD COLUMN score_source TEXT;
