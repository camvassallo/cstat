-- Play-by-play-derived per-(player, game) aggregates (P2a). Additive columns
-- on player_game_stats, populated by compute_pbp_aggregates from the local-only
-- play_by_play table. Unlike raw PBP, these SHIP TO PROD — they're small and
-- every served PBP surface (shot mix, transition rate, fouls drawn) reads them.
--
-- Semantics: NULL = no play-by-play available for this player-game (pre-2012 /
-- not loaded); a non-NULL value (including 0) = PBP was present and this is the
-- derived count. Shot-location from the `paint` tag; context points from the
-- `brk` / `2ch` / `offto` tags; fouls drawn from the `FOULED` tag (which marks
-- the player who DREW the foul, distinct from who shot the FTs).
--
-- Counts are computed over source-deduplicated plays (NatStat occasionally
-- emits a play twice — see docs/pbp_methodology.md "Data-quality notes").

ALTER TABLE player_game_stats
    ADD COLUMN IF NOT EXISTS paint_fga             INT,
    ADD COLUMN IF NOT EXISTS paint_fgm             INT,
    ADD COLUMN IF NOT EXISTS perimeter_fga         INT,
    ADD COLUMN IF NOT EXISTS perimeter_fgm         INT,
    ADD COLUMN IF NOT EXISTS transition_pts        INT,
    ADD COLUMN IF NOT EXISTS second_chance_pts     INT,
    ADD COLUMN IF NOT EXISTS points_off_turnovers  INT,
    ADD COLUMN IF NOT EXISTS fouls_drawn           INT;
