-- In-house Elo (NatStat migration plan §4.4 / §4.5 step 6): one row per team
-- per played game, with the rating going into the game and coming out of it.
-- Computed from our own `games` by `cstat_core::team_elo` (a `compute_all`
-- step and `cstat-ingest team-elo`), so it needs no outside feed. Replaces
-- NatStat's `team_elo_games` log once the game models are retrained on it.
--
-- `team_key` is the cross-season team identity the rating carries along —
-- `teams.natstat_id` today, `espn_id` once cross-season joins move (§4.7). It
-- is stored so "this team's latest rating on or before a date" is one index
-- probe rather than a join through every season's `teams` row.
--
-- A non-Division I opponent has no `teams` row, so it has no row here; its
-- fixed rating is a constant in the code.
CREATE TABLE team_game_elo (
    game_id     UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    team_id     UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    team_key    TEXT NOT NULL,
    season      INTEGER NOT NULL,
    game_date   DATE NOT NULL,
    elo_before  DOUBLE PRECISION NOT NULL,
    elo_after   DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (game_id, team_id)
);

CREATE INDEX idx_team_game_elo_key_date ON team_game_elo (team_key, game_date DESC, game_id DESC);
CREATE INDEX idx_team_game_elo_season ON team_game_elo (season);
