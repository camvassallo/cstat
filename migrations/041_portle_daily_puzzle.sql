-- Server-authoritative daily puzzle pin for Portle (issue #181).
--
-- The daily answer used to be chosen client-side (a rendezvous hash over the
-- player pool). That made it fragile in two ways: it was keyed on a surrogate
-- UUID that a data rebuild re-mints (resetting the whole sequence), and two
-- clients on skewed data/versions could compute *different* answers for the
-- same day. This table makes the pick authoritative and immutable: the API pins
-- one answer per (mode, season, date) on first request and freezes it, so every
-- client fetches the identical puzzle and it never moves once set — even if the
-- eligible pool later changes (a player's CamPom slips under a mode threshold,
-- a filter tweak, a recompute).
--
-- The stored key is `natstat_id`, the stable per-season NatStat identity (never
-- a `gen_random_uuid` surrogate) — so the pin survives re-ingests and syncs.
--
-- Runtime-populated ON PROD by the API request path (like `api_cache` /
-- `ingest_runs`), so it is EXCLUDED from `sync_to_prod.sh` — a local sync must
-- never truncate the pins prod has already frozen for live players.
CREATE TABLE IF NOT EXISTS portle_daily_puzzle (
    mode        TEXT    NOT NULL,           -- 'p5' | 'starters' | 'campom10' | 'all'
    season      INTEGER NOT NULL,
    puzzle_date DATE    NOT NULL,           -- the player's LOCAL calendar date
    natstat_id  TEXT    NOT NULL,           -- stable identity of the pinned answer
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (mode, season, puzzle_date)
);
