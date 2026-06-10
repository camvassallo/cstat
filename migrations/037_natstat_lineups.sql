-- Durable capture of NatStat's per-game server-computed 5-man lineup units —
-- the "lineups" object on the games;lineups hydrate — the Tier-2 cross-season
-- lineup-membership source (docs/pbp_utilization_scope.md). De-risked
-- 2026-06-10: units carry NatStat player codes, but the code SERIES does not
-- always match the one our box ingest stored (NatStat re-issues player ids),
-- so resolution is two-tier — exact code match against the game's box roster
-- first, then abbreviated-name match (`lineupplayers`, "F. Abee · J. Banks ·
-- ...") against the same game-team roster. Both are game-scoped; ambiguous
-- names (1-2.7% of team-games have a colliding initial+lastname pair) stay
-- NULL. The hydrate must be lineups-only (`games;playbyplay,lineups` 500s on
-- 2026 games).
--
-- These are CAPTURE tables, not derived ones: rows are written once by
-- `cstat-ingest lineups` and must survive compute reruns — the full backfill
-- costs ~130 hrs of rate-limited API across 12 seasons, so compute re-emits
-- lineup_stints FROM here and never regenerates this data. The complete unit
-- payload is kept in `raw` (per-unit box splits, ppp, context points) so
-- later parsing improvements never need a refetch.
--
-- LOCAL-ONLY, like raw play_by_play: scripts/sync_to_prod.sh excludes both
-- tables; prod serves only the derived lineup_aggregates / player_on_off.

-- Per-game fetch ledger: the backfill's done-set (skip-on-restart) plus an
-- explicit record of games the API has no lineups object for (~12% sampled),
-- so reruns don't re-spend budget on known-empty games.
CREATE TABLE IF NOT EXISTS natstat_lineup_games (
    game_id    UUID PRIMARY KEY REFERENCES games(id),
    season     INT  NOT NULL,
    status     TEXT NOT NULL,  -- 'ok' | 'no_lineups' (fetched, object absent) | 'error'
    units      INT  NOT NULL DEFAULT 0,
    fetched_at TIMESTAMP NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_natstat_lineup_games_season
    ON natstat_lineup_games (season, status);

CREATE TABLE IF NOT EXISTS natstat_lineups (
    game_id           UUID NOT NULL REFERENCES games(id),
    natstat_lineup_id TEXT NOT NULL,
    season            INT  NOT NULL,
    team_id           UUID REFERENCES teams(id),
    team_code         TEXT,                -- NatStat abbrev as sent ("MINN")
    player_codes      TEXT[] NOT NULL,     -- NatStat player codes, unit order
    player_ids        UUID[] NOT NULL,     -- per-slot resolution; NULL where the code has no players row
    resolved          BOOLEAN NOT NULL,    -- five slots, all resolved
    possessions       REAL,
    points            INT,
    points_d          INT,
    plusminus         INT,
    raw               JSONB NOT NULL,      -- full unit payload (box splits, oppp/dppp, ptspaint/ptsbrk/pts2ch/ptsoffto)
    PRIMARY KEY (game_id, natstat_lineup_id)
);

CREATE INDEX IF NOT EXISTS idx_natstat_lineups_season ON natstat_lineups (season);
CREATE INDEX IF NOT EXISTS idx_natstat_lineups_team ON natstat_lineups (team_id, season);
