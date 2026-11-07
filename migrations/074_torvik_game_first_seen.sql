-- Torvik's per-game ratings as first scraped. `torvik_player_game_stats` is
-- overwritten every night, and the ratings for a finished game change on a later
-- scrape, so that table cannot say what was knowable at the time (#466).
--
-- Insert-once: nothing updates this table. `first_seen_at` is the capture time,
-- and a row only means "known before later games" when that is close to
-- `game_date`. A backfill of an old season lands here too, long after the fact.
CREATE TABLE torvik_game_first_seen (
    pid           INTEGER NOT NULL,
    game_uid      TEXT NOT NULL,
    season        INTEGER NOT NULL,
    game_date     DATE NOT NULL,
    team          TEXT,
    minutes_pct   DOUBLE PRECISION,
    usage         DOUBLE PRECISION,
    bpm           DOUBLE PRECISION,
    obpm          DOUBLE PRECISION,
    dbpm          DOUBLE PRECISION,
    possessions   DOUBLE PRECISION,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (pid, game_uid)
);

CREATE INDEX idx_torvik_game_first_seen_season ON torvik_game_first_seen (season, game_date);
