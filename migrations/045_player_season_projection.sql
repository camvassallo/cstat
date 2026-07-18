-- Per-player preseason CamPom projection, persisted per (target_season, player).
--
-- Sibling to `team_preseason_projection` (023): the roster-projection pipeline
-- (`compose_all_projections` → trajectory / freshman models) already produces a
-- per-player projected `cam_gbpm_v3` for every returner, incoming transfer, and
-- committed recruit on each team's hypothetical next-season roster, but today
-- `compute-projections` only SUMS those into the team AdjEM and discards the
-- per-player numbers. This table materializes them so the `/players` page can
-- rank the upcoming (not-yet-played) season's players by projected CamPom.
--
-- `target_season` is the season the projection is FOR (composed from the base
-- season `target_season - 1` rosters). `player_id` is:
--   * returners / transfers — the player's real season-scoped `players.id` from
--     their base (or earlier, for a sat-out arrival) season, so the frontend can
--     link straight to `/players/{player_id}?season={base_season}`;
--   * freshmen — the `recruits.id` (a distinct UUID namespace, no collision).
-- No FK on `player_id` because of that mixed namespace.
--
-- `team_id` is the **base-season** team the player is projected onto (the
-- destination team for an incoming transfer). Base-season team rows exist, so
-- the FK is valid; the frontend links to `?season={base_season}`.
--
-- `source` distinguishes the three cohorts for the UI's Ret/Tfr/Fr chip. The
-- band (`projected_cam_lower/upper`) is the trajectory/freshman q10/q90; it is
-- NULL when per-player inference was unavailable and the mean fell back to the
-- player's frozen base-season cam_v3 (returners) or the replacement-level
-- freshman scalar. Re-running `compute-projections` replaces the season's rows
-- (delete-then-insert in one transaction), so it stays self-consistent.
CREATE TABLE IF NOT EXISTS player_season_projection (
    target_season       INTEGER NOT NULL,
    player_id           UUID NOT NULL,
    source              TEXT NOT NULL CHECK (source IN ('returning', 'transfer', 'freshman')),
    name                TEXT NOT NULL,
    team_id             UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    team_name           TEXT NOT NULL,
    natstat_id          TEXT,
    projected_cam_mean  REAL NOT NULL,
    projected_cam_lower REAL,
    projected_cam_upper REAL,
    class_year          TEXT,
    primary_archetype   TEXT,
    composite_rank      INTEGER,
    star_rating         SMALLINT,
    computed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (target_season, player_id)
);

CREATE INDEX IF NOT EXISTS idx_player_season_projection_rank
    ON player_season_projection (target_season, projected_cam_mean DESC);
