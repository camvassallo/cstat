-- Preseason team-strength projection, persisted per (season, team).
--
-- The Phase B roster-impact projection (`compose_all_projections` →
-- `predict_roster_impact`) is season-wide and expensive (it composes every
-- team's returning ∪ portal ∪ recruit roster in one pass), and today
-- `/api/projections` recomputes it live on every call. The preseason × pit
-- predict blend (ROADMAP §6) needs each team's projected AdjEM cheaply, per
-- predict request, so we materialize it here once per season via the
-- `cstat-ingest compute-projections --year` step and read it back in the
-- predict route.
--
-- `season` is the TARGET season the projection is FOR (computed from the
-- base season `season - 1` rosters). `team_id` is the **target-season**
-- team UUID — the projection pipeline keys ProjectedRoster by the
-- base-season UUID, so the ingest step resolves base → target via
-- `teams.natstat_id` before writing, letting the predict route (which holds
-- target-season UUIDs) look up directly without a cross-season hop.
--
-- `projected_adj_em` is the floor/ceiling midpoint (the headline number the
-- projections page ranks on); floor/ceiling are kept for the band and for
-- future use (e.g. uncertainty-aware blending). Re-running the step
-- overwrites the season's rows (UPSERT), so it stays self-consistent with
-- the latest models.
CREATE TABLE IF NOT EXISTS team_preseason_projection (
    season           INTEGER NOT NULL,
    team_id          UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    projected_adj_em REAL NOT NULL,
    floor_adj_em     REAL,
    ceiling_adj_em   REAL,
    computed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (season, team_id)
);

CREATE INDEX IF NOT EXISTS idx_team_preseason_projection_season
    ON team_preseason_projection (season);
