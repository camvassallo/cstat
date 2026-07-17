-- Per-run, per-table row-count snapshots for the nightly quality gate (M5a).
-- After each successful compute the nightly records the season-scoped row count
-- of every served table under its run_id; the next run compares against the most
-- recent prior snapshot and alerts on a material drop (a served table should
-- only ever grow or hold flat within a season). Runtime-written on prod like
-- `ingest_runs`; excluded from `sync_to_prod.sh`.
CREATE TABLE IF NOT EXISTS ingest_run_table_counts (
    run_id      UUID        NOT NULL,
    season      INTEGER     NOT NULL,
    table_name  TEXT        NOT NULL,
    row_count   BIGINT      NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, table_name)
);

-- The prior-run lookup is "most recent snapshot for this season, excluding the
-- current run" — ordered by recorded_at DESC, filtered by season.
CREATE INDEX IF NOT EXISTS idx_ingest_run_table_counts_season_recorded
    ON ingest_run_table_counts (season, recorded_at DESC);
