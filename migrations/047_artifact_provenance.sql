-- Which model artifact produced each Layer 3 derived product (issue #238).
--
-- #223/#237 gave every *trained* node an `input_provenance` stamp in its model
-- meta, so "is this model current?" is answerable against the database. Layer 3
-- has no meta to stamp: `team_preseason_projection`, the per-team backtest
-- dumps, and `coach_season_cae` / `coach_ratings` are produced by CLI runs that
-- write rows and files, not models. Nothing recorded which model generation
-- they came from.
--
-- That gap has a specific shape. `projections-backtest` scores with the
-- per-target-season LOSO models in `models/roster_impact_loso/`, and
-- `compute_cae.py` grades coaches against that backtest's dump. Those LOSO
-- files are **gitignored**, so they never appear in `git status`, and
-- `roster_adjo` exports no per-season ONNX at all — so a backtest can silently
-- run against a LOSO set drawn from a different frame than the committed
-- serving model, producing CAE grades scored against a projection generation
-- that no longer ships. Nothing errors; the numbers stay plausible. That is the
-- #218 failure shape one layer over.
--
-- Deliberately generic rather than columns on `team_preseason_projection`: the
-- backtest dump is a FILE and the CAE grades live in two other tables, so a
-- per-table column would need three different mechanisms. `(artifact,
-- artifact_key)` covers all of them, and new Layer 3 products need no migration.
--
--   artifact      logical product — 'team_preseason_projection',
--                 'projections_backtest_dump', 'coach_season_cae'
--   artifact_key  the slice within it: a season as text, a dump filename, or
--                 'all' for products written as one indivisible unit. Text
--                 rather than typed because those three are not the same shape.
--   provenance    what produced it. By convention:
--                   { "models": { "<name>": { "onnx_sha256": …,
--                                             "input_provenance": {…},
--                                             "oof_provenance": {…} } },
--                     "produced_by": "<cli command>" }
--                 `input_provenance` is copied verbatim from the model meta, so
--                 a Layer 3 row can be compared against the live database by
--                 the same code path that checks Layer 1 and Layer 2.
--
-- `computed_at` is metadata ABOUT the row, never hashed into a fingerprint —
-- the #223 design excludes timestamps precisely so a deterministic re-run is
-- not mistaken for drift (#222 made the trainers reproducible; a no-op retrain
-- must be provable as a no-op).
--
-- Not in `sync_to_prod.sh`'s EXCLUDED list, so it syncs by default. That is
-- correct and load-bearing: `team_preseason_projection` reaches prod by data
-- sync, so its provenance has to travel with it or prod would hold rows whose
-- origin is only recorded on one laptop.
--
-- Staleness here is REPORT-ONLY by design. `check_provenance.py` reads this
-- table but a Layer 3 mismatch must never block the API boot the way the Layer
-- 2 stamp mismatch does: a stale `team_preseason_projection` is a
-- data-freshness problem, and prod refusing to serve over it would be strictly
-- worse than serving it.
CREATE TABLE IF NOT EXISTS artifact_provenance (
    artifact     TEXT NOT NULL,
    artifact_key TEXT NOT NULL,
    provenance   JSONB NOT NULL,
    computed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (artifact, artifact_key)
);

CREATE INDEX IF NOT EXISTS idx_artifact_provenance_artifact
    ON artifact_provenance (artifact);
