-- Cold-start presentation contract (PR 3a): distinguish a real current-season
-- archetype assignment from a prior-season SEED carried over for a player who
-- hasn't yet cleared the >=10 GP gate this season.
--
-- Why: at the >=10 GP / >=10 MPG gate, a new season has ZERO archetype rows
-- until ~Nov 29 (first player to 10 GP; median Dec 13). The stability sweep
-- proved no gate makes an early current-season label accurate (N=5 ~52%), so we
-- do NOT lower the gate. Instead, a returning/transferring player keeps their
-- most-recent prior-season archetype (a completed-season fact, immune to an
-- early off-night) until they clear this season's gate and get a real
-- assignment. These three columns let the serve path and UI tell the two apart.
--
-- provisional   — TRUE for a prior-season seed, FALSE for a real current-season
--                 assignment. Existing rows are all real assignments → default FALSE.
-- source        — 'current' (nearest-centroid assign against the frozen model) or
--                 'prior_season' (copied from an earlier season's real label).
-- source_season — the season the label was copied FROM, for provisional rows
--                 (NULL for current-season assignments); lets the UI say e.g.
--                 "2026 archetype" rather than a bare "provisional".
ALTER TABLE player_archetypes
    ADD COLUMN provisional   BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN source        TEXT    NOT NULL DEFAULT 'current',
    ADD COLUMN source_season  INTEGER;

-- Serve paths that want only real labels (class summaries, exemplars, team
-- archetype index) will filter on provisional; index the common case.
CREATE INDEX IF NOT EXISTS idx_player_archetypes_provisional
    ON player_archetypes (season, provisional);
