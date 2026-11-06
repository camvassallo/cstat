-- A second source's pregame spread for each game, to catch a line stated from
-- the wrong side (#448).
--
-- `game_lines` comes from one sportsbook feed, so its spread and moneyline can
-- be wrong together, which no check inside the feed can see: on 2026-11-02 it
-- had Florida A&M a 28.5-point favourite over Florida State, who won by 43.
-- The record panel drops a game's spread and moneyline when the two sources
-- disagree on the favourite by a wide margin (`record::lines_disputed`).
--
-- A separate table because `lines` rewrites `game_lines`, and the check should
-- survive that without either step knowing about the other.
CREATE TABLE IF NOT EXISTS game_line_checks (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- The side `home_spread` is stated from, as in `game_lines`. Compare with
    -- `games.home_team_id` and flip when they differ.
    home_team_id UUID NOT NULL REFERENCES teams(id),
    -- Sportsbook convention: -4.5 means that team is favoured by 4.5.
    home_spread DOUBLE PRECISION NOT NULL,
    -- Which book the second source listed.
    provider TEXT,

    checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_game_line_checks_season_date
    ON game_line_checks (season, game_date);
