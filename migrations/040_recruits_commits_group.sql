-- Widen the recruits.institution_group vocab to include 'commits'.
--
-- The original ingest (#020) sourced only the 247 composite *rankings* HTML,
-- which by construction lists ranked players only — unranked / international /
-- G-League commits never entered the table, so those teams' "Future" pages
-- were missing real commits (issue #175).
--
-- The fix adds a second, cookie-free ingest pass over the national commits
-- feed (`/season/{year}-basketball/commits/`), which lists every commit
-- regardless of ranking. Those rows are tagged institution_group='commits'
-- as a provenance marker: the feed mixes HS / prep / international / G-League
-- cohorts with no per-row group, and the marker lets the two ingest passes
-- converge without clobbering each other — the composite pass promotes a row
-- to a ranked cohort ('highschool' etc.) and owns it thereafter, while the
-- commits pass only ever refreshes rows still tagged 'commits'
-- (see ingest::recruits::upsert_commit's `WHERE institution_group = 'commits'`).
--
-- CHECK on TEXT (not native ENUM), same as #020, so widening is a one-line add.

ALTER TABLE recruits
    DROP CONSTRAINT IF EXISTS recruits_institution_group_check;

ALTER TABLE recruits
    ADD CONSTRAINT recruits_institution_group_check
    CHECK (institution_group IN ('highschool', 'juco', 'prep', 'commits'));
