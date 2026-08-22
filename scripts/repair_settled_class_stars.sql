-- One-time repair for `recruits.star_rating` on rows the JSON transport
-- INSERTED into already-settled classes.
--
--   psql "$DATABASE_URL" -f scripts/repair_settled_class_stars.sql
--
-- Wraps its work in a transaction and ROLLS BACK by default. Read the report it
-- prints, then set `repair.commit` to `on` (see the bottom) to actually write.
--
-- Why re-ingest cannot do this. `class_is_live` stops the feed OVERWRITING a
-- settled class, and that gate held — the HTML-scraped stars are intact, which
-- is why `cstat-ingest recruits --year 2026` was enough to repair the live
-- classes. The gate says nothing about INSERTS, though: a settled-class row the
-- scrape never captured has no stored value to protect, so the feed's own
-- `compositeStarRating` landed on it unchallenged. A backfill across every class
-- on 2026-08-20 added ~234 such rows. They are stuck: settled classes now
-- COALESCE, so the feed will never revisit them, and they are the one place the
-- star bug outlived the live-class repair.
--
-- Scope, and why it is safe. This touches ONLY rows carrying a JSON
-- `raw_source` in a settled class. Those stars were never ground truth — they
-- came from the field `docs/247_api.md` documents as wrong — so re-banding them
-- from `composite_rating` is a correction, not a rollback. HTML-scraped rows
-- are ground truth (they count 247's rendered glyphs) and are left alone, as
-- are live classes, which the feed already rewrote correctly.
--
-- Training. 27 of these rows join to a real player and so sit in the freshman
-- model's training frame, where `recruit_star_rating` is monotone increasing —
-- all 27 inflated by exactly one star, all in classes 2014-2015, 0.59% of a
-- 4,598-row frame. Small enough not to force a retrain on its own; worth fixing
-- so the next one starts clean. See docs/model_dependency_graph.md before
-- deciding.

\set ON_ERROR_STOP on
\pset pager off

-- The settled/live boundary, mirroring `ingest::recruits::class_is_live`:
-- a class is live from `current_natstat_season()` up. The season rolls in
-- November, so this is the same date rule as `lib.rs::season_for_date`.
SELECT CASE
    WHEN EXTRACT(MONTH FROM CURRENT_DATE) >= 11 THEN EXTRACT(YEAR FROM CURRENT_DATE) + 1
    ELSE EXTRACT(YEAR FROM CURRENT_DATE)
END::int AS current_season \gset
\echo 'Classes below this season are settled and in scope:' :current_season

BEGIN;

CREATE TEMP VIEW target AS
SELECT r.id, r.year, r.full_name, r.composite_rating, r.star_rating,
       r.cstat_player_id,
       CASE
           WHEN r.composite_rating IS NULL THEN NULL
           WHEN r.composite_rating >= 0.9900 THEN 5
           WHEN r.composite_rating >= 0.9350 THEN 4
           WHEN r.composite_rating >= 0.8100 THEN 3
           ELSE 2
       END AS banded_star
FROM recruits r
WHERE r.year < :current_season
  -- JSON transport only. Legacy rows key the fragment as `raw_html`; both are
  -- checked so an HTML row can never be mistaken for a JSON one.
  AND LEFT(LTRIM(COALESCE(r.raw_player ->> 'raw_source', r.raw_player ->> 'raw_html')), 1) = '{'
  AND r.star_rating IS NOT NULL
  AND r.composite_rating IS NOT NULL;

\echo
\echo '== Rows that will change, by class and direction ============================'
SELECT year,
       star_rating AS stored,
       banded_star AS corrected,
       COUNT(*)                                             AS rows,
       COUNT(*) FILTER (WHERE cstat_player_id IS NOT NULL)  AS in_training_frame
FROM target
WHERE star_rating <> banded_star
GROUP BY year, star_rating, banded_star
ORDER BY year, star_rating DESC;

\echo
\echo '== Totals ==================================================================='
SELECT COUNT(*)                                          AS json_settled_rows,
       COUNT(*) FILTER (WHERE star_rating <> banded_star) AS changing,
       COUNT(*) FILTER (WHERE star_rating > banded_star)  AS deflating_an_inflated_star,
       COUNT(*) FILTER (WHERE star_rating < banded_star)  AS promoting
FROM target;

UPDATE recruits r
SET star_rating = t.banded_star
FROM target t
WHERE r.id = t.id AND r.star_rating <> t.banded_star;

\echo
\echo '== Post-update check: settled JSON rows still disagreeing (want 0) =========='
SELECT COUNT(*) AS still_disagreeing
FROM recruits r
WHERE r.year < :current_season
  AND LEFT(LTRIM(COALESCE(r.raw_player ->> 'raw_source', r.raw_player ->> 'raw_html')), 1) = '{'
  AND r.star_rating IS NOT NULL
  AND r.composite_rating IS NOT NULL
  AND r.star_rating <> CASE
      WHEN r.composite_rating >= 0.9900 THEN 5
      WHEN r.composite_rating >= 0.9350 THEN 4
      WHEN r.composite_rating >= 0.8100 THEN 3
      ELSE 2 END;

-- Dry run by default. To commit, run with:
--   psql "$DATABASE_URL" -v commit=on -f scripts/repair_settled_class_stars.sql
--
-- Branch on the variable's VALUE, not on whether it is defined. `\if :{?commit}`
-- on its own is a trap: `:{?name}` asks only whether the variable is SET, so
-- `-v commit=off` — the spelling anyone reaching for a forced dry run would
-- type — takes the commit branch and writes. Defaulting it and testing the
-- value makes `off`, `false` and `0` all mean what they say.
\if :{?commit}
\else
    \set commit off
\endif

\if :commit
    \echo '>> committing'
    COMMIT;
\else
    \echo '>> DRY RUN — rolling back. Re-run with `-v commit=on` to write.'
    ROLLBACK;
\endif
