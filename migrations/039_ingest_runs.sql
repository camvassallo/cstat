-- Per-step ingest run ledger. One row per step of each scheduled/nightly
-- ingest invocation, grouped by `run_id`. This is the audit trail and the
-- source of truth for feed-freshness — `GET /api/health/ingest` reports the
-- most recent successful `ended_at` per `step`, and the alerting path fires
-- when a step's last success is older than the staleness threshold.
--
-- Written directly by the (Railway-hosted) nightly orchestrator against prod,
-- so it is a runtime-populated table like `api_cache`: it is EXCLUDED from
-- `scripts/sync_to_prod.sh` (a local full-sync must not truncate the
-- production ledger).
CREATE TABLE IF NOT EXISTS ingest_runs (
    id           BIGSERIAL PRIMARY KEY,
    -- Groups every step belonging to a single nightly invocation.
    run_id       UUID        NOT NULL,
    season       INTEGER     NOT NULL,
    -- Pipeline step: 'games', 'player_perfs', 'team_perfs', 'forecasts',
    -- 'torvik', 'torvik_games', 'compute'. Free-text so new steps don't need
    -- a migration; the health route groups on it.
    step         TEXT        NOT NULL,
    -- 'ok' | 'failed' | 'skipped'.
    status       TEXT        NOT NULL,
    -- Rows the step upserted/touched (NULL when not meaningful, e.g. compute).
    rows_touched BIGINT,
    -- NatStat/Torvik API calls the step consumed, for rate-budget headroom
    -- tracking (NULL when not tracked).
    api_calls    BIGINT,
    started_at   TIMESTAMPTZ NOT NULL,
    ended_at     TIMESTAMPTZ NOT NULL,
    -- Populated on 'failed'/'skipped' with the reason.
    error        TEXT,
    notes        TEXT
);

-- Freshness lookups: "most recent successful run per step" and per-run audits.
CREATE INDEX IF NOT EXISTS idx_ingest_runs_step_ended
    ON ingest_runs (step, ended_at DESC);
CREATE INDEX IF NOT EXISTS idx_ingest_runs_run_id
    ON ingest_runs (run_id);
