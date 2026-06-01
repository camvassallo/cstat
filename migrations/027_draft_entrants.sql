-- NBA draft early-entrants — the firm/uncertain departures the roster
-- projection removes from a returning roster (Cooper Flagg leaves Duke, etc.).
--
-- Replaces the loose `data/draft/{year}_early_entrants.json` files as the
-- SERVING source. Those files were read off disk by the API at request time,
-- so they never synced to prod (sync_to_prod.sh is DB-only) — a missing file
-- silently produced a projection with every draftee miscounted as returning.
-- A table syncs with the rest of the schema and is queryable/joinable; the
-- JSON files remain the version-controlled capture that seeds it (same
-- pattern as data/transfers/*.json → the transfers table), loaded via
-- `cstat-ingest draft --dir data/draft`.
--
-- `year` is the DRAFT year = the base cstat-season the player is leaving; the
-- projection for TARGET season `year + 1` reads these rows. Matching to a
-- roster player is done at projection time by normalized (name, team), so no
-- resolved player FK is stored here.

CREATE TABLE IF NOT EXISTS draft_entrants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    year INTEGER NOT NULL,
    player_name TEXT NOT NULL,
    current_team TEXT NOT NULL,
    -- 'gone' = firm departure; 'declared' = uncertain (ceiling-only). Mirrors
    -- the DraftScenario mapping in roster_projection.rs; other values no-op.
    status TEXT NOT NULL DEFAULT 'gone',
    -- Provenance, e.g. 'tankathon' (historical) or the live-forecast capture.
    source TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (year, player_name, current_team)
);

CREATE INDEX IF NOT EXISTS idx_draft_entrants_year ON draft_entrants (year);
