-- Read-only audit of `recruits.star_rating`: how many stars each class carries,
-- which transport wrote them, and whether they agree with the composite rating
-- stored on the same row.
--
--   psql "$DATABASE_URL" -f scripts/audit_recruit_stars.sql
--
-- Writes nothing. Safe against prod.
--
-- Background. The column has two writers. The HTML scrape counts the star
-- glyphs 247 renders, which is ground truth by construction. The JSON feeds
-- (`rdb/v1/recruits/` and `rdb/v1/commits/`, the default transport since
-- 2026-08-19) originally copied their own `compositeStarRating` field, which is
-- on some other scale: on captured class-of-2026 rows the rankings feed calls
-- national rank 76 (composite 0.9738) a 5-star while the commits feed calls
-- rank 69 (composite 0.9763) a 4-star, and the rendered rankings page shows
-- every player from rank 51 down as a 4-star. `tfs_recruits::composite_star_rating`
-- now bands the composite rating instead, on both JSON feeds.
--
-- Query 3 is the one that pins the band constants against 12 classes of
-- HTML-scraped history; run it before trusting the 4/5 floor.

\set ON_ERROR_STOP on
\pset pager off

-- Transport is recoverable per row: `raw_player` is the serialized RecruitRow,
-- whose `raw_source` field holds either the original JSON object or the
-- original `<li>` fragment.
CREATE TEMP VIEW audit_recruits AS
SELECT
    r.year,
    r.full_name,
    r.composite_rank,
    r.composite_rating,
    r.star_rating,
    r.committed_school,
    r.commit_status,
    CASE
        WHEN LEFT(LTRIM(r.raw_player ->> 'raw_source'), 1) = '{' THEN 'json'
        WHEN LEFT(LTRIM(r.raw_player ->> 'raw_source'), 1) = '<' THEN 'html'
        ELSE 'unknown'
    END AS transport,
    -- The band `composite_star_rating` applies. Keep in lockstep with
    -- COMPOSITE_STAR_BANDS in crates/cstat-ingest/src/tfs_recruits.rs.
    CASE
        WHEN r.composite_rating IS NULL THEN NULL
        WHEN r.composite_rating >= 0.9900 THEN 5
        WHEN r.composite_rating >= 0.8900 THEN 4
        WHEN r.composite_rating >= 0.7900 THEN 3
        ELSE 2
    END AS banded_star,
    -- The forensic copy of the feed row, re-parsed — NULL for an HTML-scraped
    -- row, whose `raw_source` is an `<li>` fragment and would fail the cast.
    -- Guarded here rather than at the call site so no query can trip over it.
    CASE
        WHEN LEFT(LTRIM(r.raw_player ->> 'raw_source'), 1) = '{'
        THEN (r.raw_player ->> 'raw_source')::jsonb
    END AS raw_json
FROM recruits r;

\echo
\echo '== 1. Stars per class, by transport =========================================='
\echo '   A class whose 5-star count jumps into the dozens is the symptom. For'
\echo '   scale: the 247 composite names roughly 30 five-stars per basketball'
\echo '   class, so `five` should sit near 30 no matter how deep the class goes.'
\echo

SELECT
    year,
    transport,
    COUNT(*)                                            AS n_rows,
    COUNT(*) FILTER (WHERE star_rating = 5)             AS five,
    COUNT(*) FILTER (WHERE star_rating = 4)             AS four,
    COUNT(*) FILTER (WHERE star_rating = 3)             AS three,
    COUNT(*) FILTER (WHERE star_rating <= 2)            AS two_or_less,
    COUNT(*) FILTER (WHERE star_rating IS NULL)         AS unrated,
    ROUND(MAX(composite_rating) FILTER (WHERE star_rating = 4)::numeric, 4)
                                                        AS best_4star,
    ROUND(MIN(composite_rating) FILTER (WHERE star_rating = 5)::numeric, 4)
                                                        AS worst_5star
FROM audit_recruits
GROUP BY year, transport
ORDER BY year DESC, transport;

\echo
\echo '== 2. Worst-ranked "5-star" per class ========================================'
\echo '   The composite is rating-banded, so the worst 5-star should sit just'
\echo '   above the best 4-star. A class where they overlap — or where the worst'
\echo '   5-star ranks in the triple digits — was written from the feed field.'
\echo

SELECT
    year,
    transport,
    MAX(composite_rank) FILTER (WHERE star_rating = 5)  AS deepest_5star_rank,
    MIN(composite_rank) FILTER (WHERE star_rating = 4)  AS best_4star_rank
FROM audit_recruits
GROUP BY year, transport
ORDER BY year DESC, transport;

\echo
\echo '== 3. Empirical band boundaries, from HTML-scraped rows only ================='
\echo '   This is the calibration source for COMPOSITE_STAR_BANDS. Each floor'
\echo '   should sit between `worst` for that star and `best` for the one below.'
\echo '   If the 5-star `worst` here is not close to 0.9900, change the constant'
\echo '   in tfs_recruits.rs to match this — the DB is the authority, not the'
\echo '   published figure.'
\echo

SELECT
    star_rating,
    COUNT(*)                                    AS n_rows,
    ROUND(MIN(composite_rating)::numeric, 4)    AS worst,
    ROUND(MAX(composite_rating)::numeric, 4)    AS best
FROM audit_recruits
WHERE transport = 'html'
  AND star_rating IS NOT NULL
  AND composite_rating IS NOT NULL
GROUP BY star_rating
ORDER BY star_rating DESC;

\echo
\echo '== 4. Rows whose stored star disagrees with their own composite rating ======='
\echo '   Counted per class and transport. Under the fixed parser a JSON class'
\echo '   should show zero. HTML rows are ground truth — a nonzero count there'
\echo '   means the band constants are wrong, not the rows.'
\echo

SELECT
    year,
    transport,
    COUNT(*)                                        AS disagreeing,
    COUNT(*) FILTER (WHERE star_rating > banded_star) AS stored_too_high,
    COUNT(*) FILTER (WHERE star_rating < banded_star) AS stored_too_low
FROM audit_recruits
WHERE star_rating IS NOT NULL
  AND banded_star IS NOT NULL
  AND star_rating <> banded_star
GROUP BY year, transport
ORDER BY year DESC, transport;

\echo
\echo '== 5. The 30 worst offenders, named =========================================='
\echo

SELECT
    year,
    full_name,
    composite_rank                              AS rank,
    ROUND(composite_rating::numeric, 4)         AS rating,
    star_rating                                 AS stored,
    banded_star                                 AS should_be,
    -- What the JSON feed itself claimed, where the row came from one.
    COALESCE(
        raw_json ->> 'compositeStarRating',
        raw_json #>> '{ranking,compositeStarRating}'
    )                                           AS feed_said,
    committed_school
FROM audit_recruits
WHERE transport = 'json'
  AND star_rating IS NOT NULL
  AND banded_star IS NOT NULL
  AND star_rating <> banded_star
ORDER BY star_rating - banded_star DESC, composite_rating DESC
LIMIT 30;

\echo
\echo '== 6. Blast radius: teams holding a phantom 5-star in a live class ==========='
\echo '   `star_rating` is served to the freshman and trajectory projection'
\echo '   models as `recruit_star_rating`, a monotone-increasing feature. Every'
\echo '   team below is carrying an inflated freshman projection, and through'
\echo '   the roster-impact calibrator an inflated preseason AdjEM.'
\echo

SELECT
    year,
    committed_school,
    COUNT(*) FILTER (WHERE star_rating = 5)                     AS stored_5stars,
    COUNT(*) FILTER (WHERE banded_star = 5)                     AS real_5stars,
    COUNT(*) FILTER (WHERE star_rating = 5 AND banded_star < 5) AS phantom_5stars
FROM audit_recruits
WHERE committed_school IS NOT NULL
  AND commit_status <> 'Uncommitted'
GROUP BY year, committed_school
HAVING COUNT(*) FILTER (WHERE star_rating = 5 AND banded_star < 5) > 0
ORDER BY phantom_5stars DESC, year DESC
LIMIT 40;

\echo
\echo '== Repair ===================================================================='
\echo '   No backfill migration is needed. Classes at or after'
\echo '   current_natstat_season() are live, so the feed overwrites them:'
\echo '     cargo run --bin cstat-ingest -- recruits --year 2026'
\echo '     cargo run --bin cstat-ingest -- recruits --year 2027'
\echo '   rewrites every star from the fixed parser. Settled classes were never'
\echo '   overwritten by the JSON transport (class_is_live gates it to COALESCE),'
\echo '   so their HTML-scraped stars stand — leave them alone.'
\echo '   Re-run this script afterwards; queries 4 and 6 should come back empty.'
\echo
