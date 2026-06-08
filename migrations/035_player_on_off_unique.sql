-- Enforce one player_on_off row per (season, player). The derivation credits a
-- player only to his own team's lineups (compute_pbp_lineups, after the
-- cross-team-attribution fix), so a season-scoped player_id maps to exactly one
-- team and (season, player_id) is the natural key.
--
-- A UNIQUE index makes the two consumers provably safe: the team-roster LEFT
-- JOIN can't fan a player's row out, and `get_player_on_off`'s fetch_optional
-- can't pick an arbitrary one of several rows. It also turns any future
-- resolution regression (a UUID leaking into another team's lineups) into a
-- loud compute-time failure instead of a silently wrong on/off panel. Replaces
-- the non-unique player index from migration 034.

-- Self-heal: drop any pre-fix cross-team rows so the unique index can apply.
-- A row survives only when its team matches the player's canonical
-- players.team_id (the box-score authority); IS DISTINCT FROM also drops rows
-- for a player with a NULL canonical team. Since player_on_off holds at most one
-- row per (season, team_id, player_id), keeping only the canonical-team row
-- leaves at most one per (season, player_id). No-op on a fresh/empty table
-- (prod), where the only rows ever inserted are already canonical-team.
DELETE FROM player_on_off oo
USING players p
WHERE p.id = oo.player_id
  AND p.season = oo.season
  AND p.team_id IS DISTINCT FROM oo.team_id;

DROP INDEX IF EXISTS idx_player_on_off_player;
CREATE UNIQUE INDEX idx_player_on_off_player ON player_on_off (season, player_id);
