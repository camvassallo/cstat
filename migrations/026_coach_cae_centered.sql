-- Season-centered CAE: a COMPARISON-ONLY view of Coach-Above-Expectation.
--
-- The headline `cae_raw` (= actual − roster projection) is absolute
-- over-expectation: it carries both real era signal (a season where coaching
-- collectively added value over baseline talent) AND the projection's
-- per-season calibration noise, mixed inseparably. Centering subtracts each
-- season's mean residual, removing both — so it is valid ONLY for ranking
-- coaches against each other on an era-neutral footing, NOT as a measure of
-- "how much" a coach added (which would wrongly erase the real era component).
--
-- Stored beside the raw + projection-quartile-debiased views, never as the
-- headline. Populated by training/compute_cae.py.

ALTER TABLE coach_season_cae
    ADD COLUMN cae_centered DOUBLE PRECISION;

ALTER TABLE coach_ratings
    ADD COLUMN cae_centered_mean   DOUBLE PRECISION,
    ADD COLUMN cae_centered_shrunk DOUBLE PRECISION;
