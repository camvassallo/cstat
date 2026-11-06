-- NatStat's per-game Elo log: one row per team per game, with the rating after
-- that game and the change it caused. The pre-game rating is
-- `elo_after - elo_change`, so this is the point-in-time source for a past game.
--
-- Keyed on NatStat's own team and game codes rather than our UUIDs: both are
-- stable across seasons and databases, and a rating carries from one season
-- into the next, so reading "the latest row for this team" needs no season join.
CREATE TABLE team_elo_games (
    team_code    TEXT NOT NULL,
    game_code    TEXT NOT NULL,
    season       INTEGER NOT NULL,
    game_date    DATE NOT NULL,
    season_games INTEGER,
    elo_after    DOUBLE PRECISION NOT NULL,
    elo_change   DOUBLE PRECISION NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_code, game_code)
);

CREATE INDEX idx_team_elo_games_team_date ON team_elo_games (team_code, game_date DESC);
CREATE INDEX idx_team_elo_games_season ON team_elo_games (season, game_date);
