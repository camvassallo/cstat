-- Player on/off splits (PBP item "A"). The headline player-value narrative
-- KenPom / EvanMiya lead with: a team's offensive & defensive rating per 100
-- possessions WITH a player on the floor vs WITHOUT him. Derived from the same
-- possession-aware stint data P3 built; this is a season rollup so it can ship
-- to prod (the per-stint `lineup_stints` it's built from stays local-only).
--
-- Convention (see docs/pbp_methodology.md "On/off splits"):
--   * ON  = the player's own valid on-floor stints (he is one of the five).
--   * OFF = his team's remaining valid stints in the SAME games — i.e. team
--     totals minus his ON totals, restricted to games he actually appeared in.
--     Restricting to games-played isolates rotation (bench time) from
--     availability (DNPs / injuries); an unplayed game contributes nothing.
--   * ortg / drtg = points per 100 possessions, the same scale as team AdjO /
--     AdjD and the lineup_aggregates rates. net = ortg - drtg.
--   * net_on_off = on_net_rtg - off_net_rtg — the on/off swing.
--
-- Inherits the same box-minute clamp + 'replay'/'onfloor' source flag as
-- lineup_aggregates (it reads the same validity-filtered game-lineups), so a
-- replay-derived split carries the same accuracy caveat in the UI. Possessions
-- are DOUBLE PRECISION to preserve the fractional 0.44*FTA term; minutes is
-- on-floor wall clock (seconds / 60). Regenerated each compute (delete by
-- season, re-insert), so no natural-key constraint is needed. Ships to prod.
CREATE TABLE IF NOT EXISTS player_on_off (
    season                  INT  NOT NULL,
    team_id                 UUID NOT NULL REFERENCES teams(id),
    player_id               UUID NOT NULL REFERENCES players(id),
    -- Games the player logged at least one valid on-floor stint in.
    games                   INT  NOT NULL DEFAULT 0,

    -- ON: the player on the floor.
    on_minutes              DOUBLE PRECISION NOT NULL DEFAULT 0,
    on_possessions_for      DOUBLE PRECISION NOT NULL DEFAULT 0,
    on_possessions_against  DOUBLE PRECISION NOT NULL DEFAULT 0,
    on_points_for           INT  NOT NULL DEFAULT 0,
    on_points_against       INT  NOT NULL DEFAULT 0,
    on_ortg                 DOUBLE PRECISION,
    on_drtg                 DOUBLE PRECISION,
    on_net_rtg              DOUBLE PRECISION,

    -- OFF: same games, player on the bench.
    off_minutes             DOUBLE PRECISION NOT NULL DEFAULT 0,
    off_possessions_for     DOUBLE PRECISION NOT NULL DEFAULT 0,
    off_possessions_against DOUBLE PRECISION NOT NULL DEFAULT 0,
    off_points_for          INT  NOT NULL DEFAULT 0,
    off_points_against      INT  NOT NULL DEFAULT 0,
    off_ortg                DOUBLE PRECISION,
    off_drtg                DOUBLE PRECISION,
    off_net_rtg             DOUBLE PRECISION,

    -- on_net_rtg - off_net_rtg. NULL when either side logged no possessions.
    net_on_off              DOUBLE PRECISION,
    source                  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_player_on_off_player ON player_on_off (season, player_id);
CREATE INDEX IF NOT EXISTS idx_player_on_off_team   ON player_on_off (season, team_id);
