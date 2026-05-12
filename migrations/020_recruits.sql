-- 247Sports composite high-school recruit rankings, scraped from the public
-- compositerecruitrankings HTML endpoint. Sister table to `transfers` (#019)
-- and shares the same enum-via-CHECK pattern, JSONB escape valve, and
-- UNNEST-batched cstat join strategy.
--
-- One row per (year, recruit_key). `year` is the recruiting class year
-- (= spring of HS graduation = matches 247's URL convention). Class-of-2026
-- recruits first appear in cstat-season 2027 box scores — same offset-by-one
-- as transfers (also keyed on the spring portal-cycle year).
--
-- `institution_group` is one of `highschool` / `juco` / `prep`. Empirically
-- the composite-rankings endpoint returns identical content for all three
-- values when called with our cookie set, so v1 ingest is HS-only; the enum
-- vocab is kept so the schema is ready when we wire up separate juco/prep
-- endpoints. CHECK on TEXT (not native ENUM) so widening a value is a
-- one-line migration.
--
-- The load-bearing downstream consumer is the Phase 5c returning-player growth
-- model: `composite_rank` / `composite_rating` / `star_rating` join to
-- `players.id` (via `cstat_player_id`, resolved post-arrival) so the model can
-- test whether scout consensus adds signal beyond observed freshman box-score
-- output.

CREATE TABLE IF NOT EXISTS recruits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Recruiting class year (calendar year of HS graduation).
    -- 2026 = class of 2026 = first appears in cstat-season 2027.
    year INTEGER NOT NULL,

    -- 247Sports' stable player ID, extracted from the player profile URL
    -- (`/player/{slug}-{KEY}/`). Composite UNIQUE with `year` for parallelism
    -- with transfers; in practice keys don't recycle across class years.
    recruit_key BIGINT NOT NULL,

    -- Which composite view this row was scraped from. v1 ingest is HS-only;
    -- the `juco` / `prep` enum values are reserved for when we find the
    -- separate endpoints for those cohorts (see comment in tfs_recruits.rs).
    institution_group TEXT NOT NULL
        CHECK (institution_group IN ('highschool', 'juco', 'prep')),

    -- Identity
    first_name TEXT,
    last_name TEXT,
    full_name TEXT GENERATED ALWAYS AS (
        TRIM(COALESCE(first_name, '') || ' ' || COALESCE(last_name, ''))
    ) STORED,

    -- Physical / position
    position TEXT,                                  -- "SF", "PG", "C", etc.
    height TEXT,                                    -- "6-8" — 247's native string form
    weight INTEGER,                                 -- pounds
    city TEXT,
    state TEXT,                                     -- two-letter postal or 247's 4-letter intl code (e.g. "SPAI")

    -- Origin school (high school / prep / juco — depends on institution_group)
    high_school TEXT,

    -- Rankings + composite rating (the load-bearing columns for 5c)
    composite_rank INTEGER,                         -- national rank within the institution_group
    composite_rating REAL,                          -- 0.0000–1.0000 composite score
    star_rating SMALLINT,                           -- 1–5
    previous_rank INTEGER,                          -- prior-period rank (when 247 shows .rank-column .other)
    position_rank INTEGER,                          -- rank within position
    state_rank INTEGER,                             -- rank within state/country

    -- Commitment
    committed_school TEXT,                          -- school display name (img alt) — e.g. "North Carolina"
    committed_school_slug TEXT,                     -- 247 college URL slug — e.g. "north-carolina"
    committed_team_id UUID REFERENCES teams(id),    -- resolved Pass 1 via `team_match_score`
    -- Commit-state vocab as observed in 247 HTML markers:
    --   "Signed"      → row has `<b class="checkmark">` (LOI signed)
    --   "Committed"   → row has `a.img-link` school but no checkmark
    --   "Uncommitted" → row has `.rankings-page__crystal-ball` with "N/A"
    -- Left without a CHECK constraint in v1 until we've seen the full distribution.
    commit_status TEXT,

    -- Asset URLs
    profile_url TEXT,                               -- e.g. "/player/alex-constanza-46134907/"
    photo_url TEXT,                                 -- player headshot image

    -- Full parsed row payload — preserves any field we didn't model as a column,
    -- and a copy of the raw HTML for forensics if the parser misses something.
    raw_player JSONB NOT NULL,

    -- Bookkeeping
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Resolved Pass 2 (post-arrival): `(full_name, committed_team_id, year+1)`
    -- → `players.id`. Mostly NULL until the recruit's freshman cstat-season is
    -- ingested (class-of-2026 → cstat-season 2027 box scores).
    cstat_player_id UUID REFERENCES players(id),

    UNIQUE (year, recruit_key)
);

-- Listing for a class year, sorted by rank.
CREATE INDEX IF NOT EXISTS recruits_year_rank_idx
    ON recruits (year, composite_rank NULLS LAST);

-- Per-group filtering ("just HS recruits for class of 2026").
CREATE INDEX IF NOT EXISTS recruits_year_inst_group_idx
    ON recruits (year, institution_group);

-- Team commits lookups ("who's coming to Duke in 2026"). Partial — only
-- resolved rows live here.
CREATE INDEX IF NOT EXISTS recruits_committed_team_idx
    ON recruits (committed_team_id)
    WHERE committed_team_id IS NOT NULL;

-- Case-insensitive name lookups for the Pass 2 player join.
CREATE INDEX IF NOT EXISTS recruits_full_name_lower_idx
    ON recruits (year, lower(full_name));

-- Reverse-join index for `players p JOIN recruits r ON r.cstat_player_id = p.id`
-- (player detail page surfaces "this player was a 4-star recruit in 2025").
-- Partial — most rows stay NULL until the recruit's freshman cstat-season exists.
CREATE INDEX IF NOT EXISTS recruits_cstat_player_id_idx
    ON recruits (cstat_player_id)
    WHERE cstat_player_id IS NOT NULL;
