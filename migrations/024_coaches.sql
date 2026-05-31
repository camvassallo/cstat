-- Head-coach entity + per-team-season mapping, sourced from barttorvik's
-- `coachdict.json` (one file, all seasons 1893–2027; documented in
-- `docs/torvik-api-guide.md` §4). Foundation for two things:
--   (1) the PR E "new head coach this offseason" flag (uncertainty-band tint
--       on projections) — `is_new_hc` below;
--   (2) the multi-year Coach-Above-Expectation metric (see
--       `docs/coach_above_expectation_design.md`), which a later PR computes
--       into a `coach_ratings` table off this mapping.
--
-- Two tables, mirroring the entity/mapping split used elsewhere:
--   `coaches`        — one row per distinct coach (the leaderboard entity).
--   `coach_seasons`  — one row per (season, team) from coachdict, joined to
--                      our season-scoped `teams` UUID where we can match.
--
-- NAME IS IDENTITY (deliberate, with a documented limit). coachdict uses a
-- consistent full-name string per coach, so the name string is the natural
-- key — crucially this keeps **Rick Pitino and Richard Pitino as distinct
-- rows** (father/son, both present in the data), and "Phil Martelli" vs
-- "Phil Martelli Jr." distinct. The cost: two different real people who share
-- an identical name across eras would collapse into one entity. At cstat's
-- modern-season footprint that collision is vanishingly unlikely; if it ever
-- surfaces, add a disambiguation suffix to `canonical_name` in the ingest.

CREATE TABLE IF NOT EXISTS coaches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The coach's name exactly as coachdict spells it. UNIQUE — this is the
    -- dedup key (see "NAME IS IDENTITY" above). Never collapse on surname.
    canonical_name TEXT NOT NULL UNIQUE,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS coach_seasons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    coach_id UUID NOT NULL REFERENCES coaches(id),

    -- cstat-season year (spring calendar year, matches coachdict + Torvik).
    season INTEGER NOT NULL,

    -- The raw coachdict team name (e.g. "North Carolina", "Texas A&M Corpus
    -- Chris"). Kept verbatim as the source-of-record + forensics for the team
    -- join, and as the natural key with `season`.
    coachdict_team_name TEXT NOT NULL,

    -- Resolved cstat team for this (team, season), via the shared
    -- `team_match_score` reconciliation. NULL when coachdict lists a team we
    -- don't carry for that season (e.g. a season we haven't ingested, or a
    -- non-D-I/transition program). Season-scoped UUID — see `teams`.
    team_id UUID REFERENCES teams(id),

    -- Cross-season team key, denormalized from the matched team so coach
    -- tenure ("Randy Bennett at Saint Mary's, 2022–2026") joins across the
    -- season-scoped `teams.id` rows without a second lookup. NULL when unmatched.
    team_natstat_id TEXT,

    -- Did this coach differ from the prior season's coach for the same team?
    -- = `coachdict[season][team] != coachdict[season-1][team]`. This is the
    -- PR E offseason coaching-change flag. NULL when the prior season has no
    -- coachdict entry for the team (can't tell) — distinct from FALSE (known
    -- same coach). Computed from the FULL coachdict (all years), so it's
    -- populated even for the earliest ingested season.
    is_new_hc BOOLEAN,

    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- One coach per team per season — coachdict's natural key.
    UNIQUE (season, coachdict_team_name)
);

-- Coach tenure / career aggregation (the CAE leaderboard groups by coach).
CREATE INDEX IF NOT EXISTS coach_seasons_coach_idx
    ON coach_seasons (coach_id, season);

-- "Who coached team T in season S" + the team-detail coach card.
CREATE INDEX IF NOT EXISTS coach_seasons_team_idx
    ON coach_seasons (team_id)
    WHERE team_id IS NOT NULL;

-- Cross-season tenure by program (joins season-scoped team rows by the
-- stable natstat_id). Partial — only matched rows carry the key.
CREATE INDEX IF NOT EXISTS coach_seasons_team_natstat_idx
    ON coach_seasons (team_natstat_id, season)
    WHERE team_natstat_id IS NOT NULL;

-- Fast filter for the PR E new-coach cohort in a given season.
CREATE INDEX IF NOT EXISTS coach_seasons_new_hc_idx
    ON coach_seasons (season)
    WHERE is_new_hc;
