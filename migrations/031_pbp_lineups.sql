-- Play-by-play lineups & stints (P2b). Hybrid sourcing: the exact per-play
-- on-floor five from the API when we have it, SUB-replay off the CSV otherwise.
-- Design of record: docs/pbp_methodology.md.

-- Raw API on-floor lineups, stored verbatim as the NatStat comma-separated
-- player-code strings the API embeds on every row (`game.onfloorhome` /
-- `game.onfloorvis`). API-only: the CSV export has no on-floor columns, so these
-- stay NULL for CSV-loaded rows and the derivation falls back to SUB-replay for
-- those games. Local-only (play_by_play never ships to prod). Resolution to our
-- player UUIDs happens at derivation time, keeping the raw table a faithful
-- mirror of the feed (same principle as the source-duplicate dedup).
ALTER TABLE play_by_play
    ADD COLUMN IF NOT EXISTS onfloor_home TEXT,
    ADD COLUMN IF NOT EXISTS onfloor_vis  TEXT;

-- Per-player PBP plus/minus from stint score differentials — exact when sourced
-- from API on-floor, ~86%-accurate when from SUB-replay (see methodology doc).
-- Overwrites the sparse/unreliable box-score plus_minus. Ships to prod.
ALTER TABLE player_game_stats
    ADD COLUMN IF NOT EXISTS plus_minus_pbp INT;

-- One row per team per contiguous stint (a window where that team's on-floor
-- five was constant). LOCAL-ONLY — per-stint detail is needed for ML training
-- and the future game-detail timeline, not for the live site, so it is excluded
-- from sync_to_prod alongside play_by_play. Regenerated each compute (delete by
-- season, re-insert), so no natural key constraint is needed.
CREATE TABLE IF NOT EXISTS lineup_stints (
    game_id        UUID NOT NULL REFERENCES games(id),
    season         INT  NOT NULL,
    period         INT  NOT NULL,
    start_seq      INT  NOT NULL,
    end_seq        INT  NOT NULL,
    team_id        UUID NOT NULL REFERENCES teams(id),
    lineup         UUID[] NOT NULL,        -- this team's on-floor players (sorted)
    opp_lineup     UUID[] NOT NULL,        -- the opponent's on-floor players (sorted)
    points_for     INT  NOT NULL DEFAULT 0,
    points_against INT  NOT NULL DEFAULT 0,
    source         TEXT NOT NULL           -- 'onfloor' (exact) | 'replay' (approx)
);
CREATE INDEX IF NOT EXISTS idx_lineup_stints_game ON lineup_stints (game_id);
CREATE INDEX IF NOT EXISTS idx_lineup_stints_team ON lineup_stints (season, team_id);

-- Season rollup of a team's 5-man lineups — minutes-proxy (stints), scoring,
-- and plus/minus. This is what the site reads (team "top lineups", on/off), so
-- it SHIPS TO PROD. Regenerated each compute (delete by season, re-insert).
CREATE TABLE IF NOT EXISTS lineup_aggregates (
    season         INT  NOT NULL,
    team_id        UUID NOT NULL REFERENCES teams(id),
    lineup         UUID[] NOT NULL,
    stint_count    INT  NOT NULL DEFAULT 0,
    points_for     INT  NOT NULL DEFAULT 0,
    points_against INT  NOT NULL DEFAULT 0,
    plus_minus     INT  NOT NULL DEFAULT 0,
    -- Best source seen for this lineup's stints: 'onfloor' if any exact, else
    -- 'replay'. Lets the UI flag approximate (replay-derived) lineup data.
    source         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_lineup_aggregates_team ON lineup_aggregates (season, team_id);
