-- Materialized pre-game projections for completed games (#266).
--
-- `GET /api/teams/{id}` used to project every game on a team's schedule at
-- request time. Each completed game routes through the point-in-time feature
-- path, whose first step is a full-season GROUP BY over
-- `torvik_player_game_stats`; a 40-game schedule with 14 neutral sites (which
-- predict twice, for order-invariance) cost 54 of those rebuilds and 846 total
-- database round-trips, capping the endpoint at ~3 rps and adding latency to
-- every other route through pool contention.
--
-- The nightly writes this table instead, batching the expensive rebuild by
-- cutoff date — every game played on date D shares one point-in-time cohort —
-- so the whole season costs ~150 rebuilds a night rather than ~5,600 per page
-- view. `team_detail` then reads one indexed row per schedule game and only
-- projects live for games that have not been played yet.
--
-- NOT append-only, despite what "a completed game's projection never changes"
-- suggests. Only the CamPom channel of the pit feature vector is genuinely
-- point-in-time; team stats, roster aggregates and rolling form are read from
-- the season-aggregate tables, so a past game's projection still moves as the
-- season fills in. The writer therefore rewrites the whole season each run,
-- and `computed_at` is what says how current a row is.
CREATE TABLE IF NOT EXISTS game_projections (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- The point-in-time cutoff the projection was built from: game_date - 1
    -- day, so the model sees pre-game state and not the game itself. Stored
    -- rather than re-derived because it is the honesty claim the API surfaces
    -- as `is_pre_game_projection`, and a row whose cutoff can't be computed
    -- (a NaiveDate::MIN game_date) must not be written at all.
    as_of_date DATE NOT NULL,

    -- Frame: everything below is from the HOME team's perspective. The API
    -- flips to the requested team's perspective on read, the same way the
    -- live path does.
    home_team_id UUID NOT NULL REFERENCES teams(id),
    away_team_id UUID NOT NULL REFERENCES teams(id),

    -- Prediction inputs that are properties of the matchup rather than the
    -- teams. Persisted so a row can be spotted as stale if a game's venue or
    -- conference flag is later corrected by ingest.
    is_neutral BOOLEAN NOT NULL,
    is_conference BOOLEAN NOT NULL,

    -- Served values, post preseason blend, matching what the live path
    -- returns for the same matchup.
    projected_margin DOUBLE PRECISION NOT NULL,
    home_win_prob DOUBLE PRECISION NOT NULL,
    projected_home_score INT NOT NULL,
    projected_away_score INT NOT NULL,

    computed_at TIMESTAMP NOT NULL DEFAULT now()
);

-- The read path: one lookup per team-season, covering every game on the
-- schedule from either side of the matchup.
CREATE INDEX IF NOT EXISTS idx_game_projections_home
    ON game_projections (home_team_id, season);
CREATE INDEX IF NOT EXISTS idx_game_projections_away
    ON game_projections (away_team_id, season);

-- The writer's own sweep, and the freshness check.
CREATE INDEX IF NOT EXISTS idx_game_projections_season_date
    ON game_projections (season, game_date);
