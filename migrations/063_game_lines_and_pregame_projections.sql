-- Betting lines per game, and the projection each game carried at tipoff: the
-- two halves of grading the Schedule tab's predictions (MAE, straight-up,
-- against the spread, moneyline).

-- Closing-ish lines from NatStat's dedicated `/pointspread`, `/moneyline` and
-- `/overunder` endpoints, written by the nightly `lines` step over a window
-- that reaches a week forward, and by `cstat-ingest lines` for a backfill.
--
-- Not `game_forecasts`, which already has line columns. Those came from the
-- nested `forecast` object of `/forecasts`, which NatStat dropped from the
-- 2026 season onward (one line out of 5,702 games), and the history they do
-- hold has no usable favourite: `spread_favorite_team_id` is NULL on every
-- row, because the feed named the favourite by a numeric team id the parser
-- tried to read as a team code, and the sign of `spread` does not say which
-- side it belongs to. This table states every line from a named side,
-- `home_team_id`, so a reader never has to infer the favourite.
--
-- That side is the one the FEED lists at home, not necessarily the `games`
-- row's: readers flip when the two differ. The nightly writes lines before
-- `compute_all`, whose `correct_swapped_games` can relabel a game's home and
-- away that same night, so a line oriented to the `games` row at write time
-- would be inverted for good once the game left the next night's window.
--
-- Each of the three markets carries its own `*_as_of`, the source's timestamp
-- for that quote. Lines move until tipoff; the writer keeps the latest quote
-- and never replaces it with an older one, so a re-run over a cached page or a
-- backfill over a stretch the nightly already covered cannot roll a line back.
CREATE TABLE IF NOT EXISTS game_lines (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- The side the values below are stated from: the team the feed lists at
    -- home. Compare with `games.home_team_id` and flip when they differ.
    home_team_id UUID NOT NULL REFERENCES teams(id),

    -- Points from `home_team_id`'s side, sportsbook convention: -4.5 means
    -- that team is favoured by 4.5, +4.5 that its opponent is.
    home_spread DOUBLE PRECISION,
    spread_as_of TIMESTAMP,

    -- American odds, `home_moneyline` for `home_team_id`.
    home_moneyline INT,
    away_moneyline INT,
    moneyline_as_of TIMESTAMP,

    -- Combined points.
    total DOUBLE PRECISION,
    total_as_of TIMESTAMP,

    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_game_lines_season_date ON game_lines (season, game_date);

-- The projection each game was shown before it tipped, frozen.
--
-- Neither existing projection table can be graded. `game_projections` is
-- rewritten every night from season-aggregate inputs that by then include the
-- game itself, so its numbers for a played game already know the result.
-- `upcoming_game_projections` is honest but drops a game once it is played.
--
-- The nightly copies every game that has not tipped yet into this table: the
-- `upcoming_game_projections` row when there is one (basis 'model'), otherwise
-- the preseason projection the Schedule tab shows for a team that has not
-- played (basis 'preseason'). A row is overwritten on each run until its tip,
-- and never after, so what remains is the last projection made before the
-- game. A game rescheduled to a later date is unfrozen by the same rule.
--
-- Written only on prod, by the nightly that sees each game before it tips; a
-- laptop cannot rebuild it after the fact, so `sync_to_prod.sh` excludes it.
CREATE TABLE IF NOT EXISTS pregame_projections (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    season INT NOT NULL,
    game_date DATE NOT NULL,

    -- Everything below is from the HOME team's perspective.
    home_team_id UUID NOT NULL REFERENCES teams(id),
    away_team_id UUID NOT NULL REFERENCES teams(id),
    is_neutral BOOLEAN NOT NULL,

    basis TEXT NOT NULL CHECK (basis IN ('model', 'preseason')),
    projected_margin DOUBLE PRECISION NOT NULL,
    home_win_prob DOUBLE PRECISION NOT NULL,
    -- NULL for a preseason projection, which is a strength difference with no
    -- tempo and so no score.
    projected_home_score INT,
    projected_away_score INT,

    captured_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pregame_projections_season_date
    ON pregame_projections (season, game_date);
