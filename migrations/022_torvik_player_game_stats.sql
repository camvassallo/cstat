-- Per-game Torvik player stats.
--
-- The `cstat-ingest torvik` command already fetches per-game data from
-- `barttorvik.com/{year}_all_advgames.json.gz` and currently uses it only
-- for offensive/defensive rebound backfill in `player_game_stats`. This
-- table persists the full per-game row so we can compute *point-in-time*
-- CamPom and other rate stats — aggregating only games before a given
-- cutoff date.
--
-- The motivating use case is the leak-free predict-model backtest in
-- ROADMAP §"CamPom overfitting audit & point-in-time predict backtest".
-- `torvik_player_stats.cam_gbpm_v3` is a full-season aggregate; using it
-- for a December game means the model sees March player ratings. This
-- table provides the per-game fragments needed to reconstruct CamPom at
-- any historical date.
--
-- Lifecycle: rows are inserted as part of `cstat-ingest torvik --year YYYY`
-- (one fetch per season). PK is (pid, game_uid) — both Torvik IDs are
-- stable within a season. No FKs: torvik_pid isn't a PK anywhere else in
-- the schema (it's a column on `torvik_player_stats`, duplicated across
-- seasons).
--
-- Note on join to cstat: `game_uid` is Torvik's identifier and doesn't
-- align with cstat's `games.id` (UUID) or `games.natstat_id`. To map a
-- Torvik per-game row back to a cstat game, join via (pid, game_date)
-- against `torvik_player_stats` → `player_id` → `players.id` and then
-- match on `game_date`. Cross-source game-ID reconciliation is left to
-- the consumer.

CREATE TABLE IF NOT EXISTS torvik_player_game_stats (
    pid INTEGER NOT NULL,
    game_uid TEXT NOT NULL,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- Context
    team TEXT,
    opponent TEXT,
    location TEXT,
    class_year TEXT,
    height_inches INT,

    -- Box score
    minutes_pct DOUBLE PRECISION,
    o_rtg DOUBLE PRECISION,
    usage DOUBLE PRECISION,
    pts DOUBLE PRECISION,
    oreb DOUBLE PRECISION,
    dreb DOUBLE PRECISION,
    ast DOUBLE PRECISION,
    tov DOUBLE PRECISION,
    stl DOUBLE PRECISION,
    blk DOUBLE PRECISION,
    pf DOUBLE PRECISION,

    -- Shooting
    two_pm INT,
    two_pa INT,
    tpm INT,
    tpa INT,
    ftm INT,
    fta INT,
    rim_made INT,
    rim_attempted INT,
    mid_made INT,
    mid_attempted INT,
    dunks_made INT,
    dunks_attempted INT,

    -- Advanced (per-game contributions, summable to season totals)
    bpm DOUBLE PRECISION,
    obpm DOUBLE PRECISION,
    dbpm DOUBLE PRECISION,
    possessions DOUBLE PRECISION,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (pid, game_uid)
);

-- Hot paths:
--   1. "all rows for player P up to date D" (point-in-time aggregation)
--   2. "all rows for season S before date D" (bulk cutoff queries)
CREATE INDEX idx_torvik_pgs_pid_date ON torvik_player_game_stats (pid, game_date);
CREATE INDEX idx_torvik_pgs_season_date ON torvik_player_game_stats (season, game_date);
