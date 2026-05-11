-- 247Sports transfer portal data, fetched from the ipa.247sports.com REST API.
-- Replaces the embedded `data/transfers/{year}.json` files: those were a static
-- top-N scrape; this table holds the full portal (2,620 rows for 2026) with
-- per-row incremental refresh keyed on `last_update_date`.
--
-- One row per (year, tfs_key). Multi-destination crystal-ball cases (~0.2% of
-- rows; up to 2 destinations) are kept in `raw_player.transfer.destination[]`
-- via JSONB; the flattened `destination_*` columns hold the primary commit
-- (the destination with `transferred = true`, or the highest-percentage
-- candidate if none have committed).
--
-- Status / eligibility values are kept as TEXT with CHECK constraints rather
-- than native PostgreSQL enums. Native ENUM saves a few bytes per row but
-- adding upstream values requires `ALTER TYPE ADD VALUE` and a deploy; CHECK
-- on TEXT lets us widen the allowed set with a one-line migration when 247
-- introduces a new state. The vocab is theirs, not ours.

CREATE TABLE IF NOT EXISTS transfers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Portal class year (calendar year of the spring portal cycle).
    -- 2026 = spring-2026 cycle = moves into the 2026-27 / cstat-season-2027.
    year INTEGER NOT NULL,

    -- 247Sports' stable player ID (`player.key` in the API response).
    -- Composite UNIQUE with `year` because the same player can re-enter the
    -- portal in a future year (rare but happens — see grad transfers).
    tfs_key BIGINT NOT NULL,

    -- Identity
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULL,
    full_name TEXT GENERATED ALWAYS AS (first_name || ' ' || last_name) STORED,
    avatar_url TEXT,
    player_profile_url TEXT,

    -- Physical / position
    height TEXT,                                   -- "6-10"
    weight INTEGER,                                -- 250
    position TEXT,                                 -- "C"
    position_group_name TEXT,                      -- "Center"
    position_key INTEGER,
    position_group_key INTEGER,
    position_rank INTEGER,

    -- Ratings (composite, 0-1 scale)
    rating REAL,
    transfer_rating REAL,
    high_school_rating REAL,
    star_rating SMALLINT,

    -- Ranks
    rank INTEGER,
    transfer_rank INTEGER,
    high_school_rank INTEGER,
    state_rank INTEGER,
    rank_trend INTEGER,                            -- positive = climbed; negative = dropped

    -- Status (enum kept as TEXT + CHECK; see header comment)
    status TEXT NOT NULL
        CHECK (status IN ('Entered', 'Committed', 'Withdrawn')),
    institution_status TEXT
        CHECK (institution_status IS NULL OR institution_status IN ('HS', 'T')),
    status_date TIMESTAMPTZ,

    -- Eligibility (separate enum from status)
    eligibility_type TEXT
        CHECK (eligibility_type IS NULL
               OR eligibility_type IN ('Immediate', 'Withdrawn', 'PendingAppeal', 'TBD')),
    eligibility_years SMALLINT,

    -- Dates (all timestamptz; 247 returns ISO8601 with Z)
    start_date TIMESTAMPTZ,
    end_date TIMESTAMPTZ,
    transfer_date TIMESTAMPTZ,
    transfer_commit_datetime TIMESTAMPTZ,
    last_update_date TIMESTAMPTZ NOT NULL,         -- drives incremental refresh

    -- Source school (always exactly one)
    source_institution TEXT,
    source_institution_key INTEGER,
    source_logo_url TEXT,

    -- Primary destination (NULL when status = 'Entered' and no commits yet).
    -- See header comment for multi-destination handling.
    destination_institution TEXT,
    destination_institution_key INTEGER,
    destination_logo_url TEXT,
    destination_transferred BOOLEAN,
    destination_percentage SMALLINT,

    -- Full API response — preserves the full destination[] array, all image
    -- asset variants, and any field 247 adds in the future without requiring
    -- a re-fetch. Query via -> / ->> when needed.
    raw_player JSONB NOT NULL,

    -- Bookkeeping
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    cstat_player_id UUID REFERENCES players(id),   -- resolved post-ingest via name+team join

    UNIQUE (year, tfs_key)
);

-- Common query paths: listing for a year + status filter, sorted by rank.
CREATE INDEX IF NOT EXISTS transfers_year_rank_idx
    ON transfers (year, rank);
CREATE INDEX IF NOT EXISTS transfers_year_status_idx
    ON transfers (year, status);

-- Source / destination team lookups (join surfaces for "who's leaving/coming to X").
CREATE INDEX IF NOT EXISTS transfers_source_idx
    ON transfers (year, source_institution_key);
CREATE INDEX IF NOT EXISTS transfers_destination_idx
    ON transfers (year, destination_institution_key);

-- Name lookups for join-to-cstat (case-insensitive prefix match via lower()).
CREATE INDEX IF NOT EXISTS transfers_full_name_lower_idx
    ON transfers (year, lower(full_name));

-- Incremental-refresh driver: SELECT WHERE last_update_date > <our last cursor>.
CREATE INDEX IF NOT EXISTS transfers_last_update_idx
    ON transfers (year, last_update_date DESC);

-- Reverse-join index for queries like `players p JOIN transfers t ON
-- t.cstat_player_id = p.id` (player detail page surfaces "this player is in
-- the portal"). Postgres doesn't auto-index the referencing side of a FK,
-- only the referenced PK. Partial — most rows stay NULL until resolve_cstat_joins runs.
CREATE INDEX IF NOT EXISTS transfers_cstat_player_id_idx
    ON transfers (cstat_player_id)
    WHERE cstat_player_id IS NOT NULL;
