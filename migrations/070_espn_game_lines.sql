-- Betting lines from a second book: the record panel's primary line (#448).
--
-- `game_lines` comes from one sportsbook feed, so its spread and moneyline can
-- be wrong together, which no check inside the feed can see: on 2026-11-02 it
-- had Florida A&M a 28.5-point favourite over Florida State, who won by 43.
-- This table is read first, falls back to `game_lines` by market, and a game
-- whose two spreads name opposite favourites by a wide margin loses its spread
-- and moneyline (`record::lines_disputed`).
--
-- A separate table so that either source can stop or fail without touching
-- the other. Spreads and prices are this book's, and it publishes prices on
-- every market, so units need no assumed -110 where these are present.
CREATE TABLE IF NOT EXISTS espn_game_lines (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- The side the values are stated from, as in `game_lines`. Compare with
    -- `games.home_team_id` and flip when they differ.
    home_team_id UUID NOT NULL REFERENCES teams(id),

    -- Sportsbook convention: -4.5 means that team is favoured by 4.5.
    home_spread DOUBLE PRECISION,
    home_spread_odds INT,
    away_spread_odds INT,

    home_moneyline INT,
    away_moneyline INT,

    total DOUBLE PRECISION,
    over_odds INT,
    under_odds INT,

    -- Which book the source listed.
    provider TEXT,

    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_espn_game_lines_season_date
    ON espn_game_lines (season, game_date);
