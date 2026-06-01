-- Harden the season-centered CAE columns added in 026.
--
-- 026 added them as plain nullable DOUBLE PRECISION to already-populated
-- tables, but the query structs in queries.rs decode them as non-Option f64
-- (matching the NOT NULL contract of the other CAE columns from 025). SQLx
-- cannot decode SQL NULL into f64, so in the window where 026 is applied but
-- `compute_cae.py --write` hasn't re-run (e.g. a fresh deploy before the data
-- sync), every /api/coaches query would 500 on the NULL rows.
--
-- Backfill any NULLs to 0 and enforce NOT NULL DEFAULT 0 so the decode is
-- always valid. compute_cae always writes explicit values, so the default
-- only ever covers that transient window; thereafter the real values stand.

UPDATE coach_season_cae SET cae_centered = 0 WHERE cae_centered IS NULL;
ALTER TABLE coach_season_cae
    ALTER COLUMN cae_centered SET DEFAULT 0,
    ALTER COLUMN cae_centered SET NOT NULL;

UPDATE coach_ratings SET cae_centered_mean = 0 WHERE cae_centered_mean IS NULL;
UPDATE coach_ratings SET cae_centered_shrunk = 0 WHERE cae_centered_shrunk IS NULL;
ALTER TABLE coach_ratings
    ALTER COLUMN cae_centered_mean SET DEFAULT 0,
    ALTER COLUMN cae_centered_mean SET NOT NULL,
    ALTER COLUMN cae_centered_shrunk SET DEFAULT 0,
    ALTER COLUMN cae_centered_shrunk SET NOT NULL;
