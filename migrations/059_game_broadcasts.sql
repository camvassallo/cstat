-- Schedule tab, second source (issue #430): the TV network for each game, and
-- a tip time for the games the primary schedule feed lists without one.
--
-- A table of its own rather than columns on `games`, so the two feeds stay
-- separable: the nightly schedule refresh diffs `games` against the primary
-- feed and must not see this source's values as drift, and the read side
-- decides which time wins (this source's confirmed time, else the primary
-- feed's) in one place, `queries::get_schedule_games`.
--
-- One row per matched game. Written by the nightly `broadcasts` step over a
-- forward window; a game the source stops listing keeps its last row, which
-- is harmless (a network does not go stale in a way that misleads), and a
-- deleted game takes its row with it.
CREATE TABLE IF NOT EXISTS game_broadcasts (
    game_id         UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season          INTEGER NOT NULL,
    -- Every outlet the source lists, national first, joined with " / ".
    -- NULL when the game is listed but not carried anywhere.
    network         TEXT,
    -- US Eastern wall clock, like `games.tip_time`. NULL when the source
    -- itself flags the time as unconfirmed (its placeholder is midnight).
    tip_time        TIME,
    -- The source's own event id, for tracing a bad match back to its listing.
    source_event_id TEXT NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_game_broadcasts_season ON game_broadcasts (season);
