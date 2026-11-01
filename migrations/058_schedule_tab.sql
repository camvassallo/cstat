-- Schedule tab (issue #429): tip time and venue city on `games`, and the
-- materialized projection for every upcoming game.
--
-- NatStat's `gamedate` carries a time alongside the date (`2026-11-02
-- 19:00:00`), in US Eastern wall-clock time like every NatStat timestamp. The
-- ingest used it only as a fallback for the date and threw the time away.
--
-- `tip_time` is that wall-clock time, paired with `game_date` (also an ET
-- date), so the two together are the ET tipoff. A TIME rather than a
-- TIMESTAMPTZ because that is all the source says, and converting through a
-- zone here would invent a DST rule the feed never stated. NULL means the
-- feed has no time: NatStat writes `00:00:00` for a game whose time is not
-- set (37% of the 2026-27 schedule on 2026-11-01), and that is stored as NULL
-- rather than as midnight.
--
-- `venue_location` is the feed's `venue.location` ("Ann Arbor, MI",
-- "Rome, Italy"). Only the venue name and code were stored before.

ALTER TABLE games
    ADD COLUMN IF NOT EXISTS tip_time TIME,
    ADD COLUMN IF NOT EXISTS venue_location TEXT;

-- Materialized projection for every UPCOMING game whose two teams have both
-- played, written by the nightly after `compute_all`.
--
-- The Schedule tab shows a whole day's slate, up to ~150 games, and projecting
-- each one live per page view is the #266 problem again. It is also
-- unnecessary: an upcoming game's projection reads only season-aggregate
-- inputs (team stats, roster aggregate, rolling form), which change only when
-- the nightly recomputes them, so the row written here equals what a live call
-- would return until the next run.
--
-- A row OUTLIVES its game's tipoff on purpose. A game played after tonight's
-- run keeps this row until the next nightly sweeps it into `game_projections`,
-- and in that window this is exactly the pre-game projection: it was computed
-- before the game, from inputs that did not yet include it. The writer prunes
-- a row once `game_projections` holds the game, or once the game's teams,
-- venue or conference flag no longer match what was projected.
--
-- Games with a team that has not played are not stored: the model has no
-- season stats for that team, and the API answers them from the preseason
-- projection, which is a subtraction and needs no materializing.
CREATE TABLE IF NOT EXISTS upcoming_game_projections (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- Everything below is from the HOME team's perspective.
    home_team_id UUID NOT NULL REFERENCES teams(id),
    away_team_id UUID NOT NULL REFERENCES teams(id),
    is_neutral BOOLEAN NOT NULL,
    is_conference BOOLEAN NOT NULL,

    projected_margin DOUBLE PRECISION NOT NULL,
    home_win_prob DOUBLE PRECISION NOT NULL,
    projected_home_score INT NOT NULL,
    projected_away_score INT NOT NULL,

    -- The date the early-season preseason blend was weighted for. The blend
    -- decays daily, so this is what says which day's weight a row carries.
    blend_date DATE NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_upcoming_game_projections_season_date
    ON upcoming_game_projections (season, game_date);
