-- Remove the team rows the 2027 bootstrap minted for programs that have left
-- Division I (#385).
--
-- `teamcodes` publishes an `active` flag that `ingest_teams` ignored, so
-- bootstrapping a season nobody has played created rows for three programs
-- that no longer play: Hartford and Saint Francis (NY), gone after 2022-23,
-- and Saint Francis (PA), whose last Division I season was 2025-26. They have
-- no games, so the projection skips them as too-thin and they never reach the
-- Future board — but they reach team lists, search, and any `teams`-keyed
-- join, as programs that do not exist.
--
-- The ingest no longer creates them. This clears the ones already written,
-- because a fix that leaves the reported symptom on the site has not finished.
--
-- SAFETY. Two independent guards, and the delete is deliberately NOT a
-- CASCADE:
--
--   1. Scoped to the three codes by name. A general "team-season with no
--      games and no projection" rule happens to match exactly these three
--      today, but that is a coincidence of timing — NatStat has published
--      only about a third of the 2027 schedule, so a legitimate program
--      awaiting its slate looks identical. Naming them keeps this from
--      widening as the feed fills in.
--
--   2. Every row must have no games, no schedule, no preseason projection and
--      no season stats. This is what protects HISTORY: Hartford played 31
--      games in 2023 and Saint Francis (PA) 31 in 2026, and those rows are
--      excluded by the first clause alone.
--
-- Because both guards hold, no foreign key can reference these rows — checked
-- against all 26 constraints referencing `teams` on prod, every one returning
-- zero. A plain DELETE therefore cannot cascade, and if that assumption were
-- ever wrong Postgres refuses the statement rather than quietly removing
-- dependents.
--
-- Idempotent: a no-op on a database where these rows were never created, or
-- where they have already been cleared.
DELETE FROM teams t
WHERE t.natstat_id IN ('HART', 'SFNY', 'SFPA')
  AND NOT EXISTS (
        SELECT 1 FROM games g
        WHERE g.season = t.season
          AND (g.home_team_id = t.id OR g.away_team_id = t.id))
  AND NOT EXISTS (
        SELECT 1 FROM schedules s
        WHERE s.season = t.season AND s.team_id = t.id)
  AND NOT EXISTS (
        SELECT 1 FROM team_preseason_projection p
        WHERE p.season = t.season AND p.team_id = t.id)
  AND NOT EXISTS (
        SELECT 1 FROM team_season_stats x
        WHERE x.season = t.season AND x.team_id = t.id);
