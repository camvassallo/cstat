-- Issue #243: keep the Torvik-side player name on `torvik_player_stats`.
--
-- The table stored only `torvik_pid` and `team_name`, so a row whose
-- `player_id` failed to link was un-diagnosable and un-relinkable without
-- re-fetching barttorvik's season CSV — the reason 1,814 NULL links sat
-- unnoticed. Persisting the source name makes "which players did we drop?"
-- a query rather than a network round-trip, and gives the linkage invariant
-- (`invariants::torvik_rows_unlinked`) something nameable to report.
--
-- Nullable and unbackfilled: existing rows fill in on the next
-- `cstat-ingest torvik --year YYYY` for that season.
ALTER TABLE torvik_player_stats ADD COLUMN player_name TEXT;

COMMENT ON COLUMN torvik_player_stats.player_name IS
    'Player name exactly as barttorvik supplies it, kept so a NULL player_id is diagnosable without re-fetching the source CSV (issue #243).';
