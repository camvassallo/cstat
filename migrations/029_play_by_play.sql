-- Raw play-by-play events. The first data source with no box-score
-- equivalent: lineups, on/off splits, shot-context, foul-drawn, and assist
-- networks exist nowhere else. Design of record: docs/pbp_methodology.md.
--
-- LOCAL-ONLY: this table is deliberately NOT shipped to prod. At ~3.35M
-- rows/season (~500 MB) and ~7.5 GB across the 2012-2026 PBP-available range,
-- it would blow Railway's DB cap, and the live site reads only derived
-- aggregates (lineup +/-, paint%, on/off). scripts/sync_to_prod.sh excludes
-- it explicitly (see its EXCLUDED array). Two loaders write it — the CSV
-- bulk path (bootstrap-csv --with-pbp, backfill) and the API path
-- (cstat-ingest playbyplay, intra-season) — both normalizing into these rows.
--
-- Row identity is (game_id, seq), NOT NatStat's "Sort"/"sequence": that key
-- collides (multiple same-instant events share e.g. "1-0060"), so we assign a
-- dense 0..N ingest-order `seq` per game. sort_order is kept for reference.
-- Idempotency unit is the game: re-ingest is DELETE WHERE game_id = $1 then
-- bulk insert (PBP for a finished game is immutable). No `distance` column —
-- it is always 0 in the source (verified 2026-06-05 against the 2026 export).

CREATE TABLE IF NOT EXISTS play_by_play (
    game_id      UUID NOT NULL REFERENCES games(id),
    season       INT  NOT NULL,
    seq          INT  NOT NULL,            -- dense 0..N ingest order within the game
    sort_order   TEXT,                     -- NatStat "Sort"/"sequence" (e.g. "1-0060"); reference only, not unique
    period       INT  NOT NULL,
    clock        TEXT,                      -- game clock, e.g. "19:59.59" (null on non-action rows)
    team_id      UUID REFERENCES teams(id), -- acting team (null on game-level events)
    player_id    UUID REFERENCES players(id), -- acting player (null on team events)
    description  TEXT,
    scoring_play BOOLEAN NOT NULL DEFAULT false,
    points       INT  NOT NULL DEFAULT 0,    -- points on the play (derived from tags on the API path)
    tags         TEXT[] NOT NULL DEFAULT '{}', -- e.g. {FGA,FGM,paint,offto}
    score_home   INT,
    score_vis    INT,
    score_diff   INT,                        -- acting-team POV ("Diff"/"thediff")
    PRIMARY KEY (game_id, seq)
);

-- Every consumer (derivation, game-detail timeline) is a full-game ordered
-- scan, so the PK's (game_id, seq) ordering is the only index needed.
