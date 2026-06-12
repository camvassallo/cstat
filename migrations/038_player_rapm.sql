-- Per-season regularized adjusted plus-minus (RAPM) — the context-adjusted
-- companion to the raw player_on_off display ("Adj on/off (RAPM)" in the UI).
-- Methodology, spike evidence, and the narrowed display-only scope decision
-- live in docs/rapm_methodology.md: a possession-weighted ridge regression of
-- per-stint scoring margin (per 100) on the on-floor indicators of all ten
-- players, fit per season over the opponent-paired stints in lineup_stints
-- (sources replay / onfloor / replay_shadow — NOT the unpaired natstat units).
--
-- Conventions (doc section 4.1):
--   * o_rapm = points per 100 added on offense vs an average player (higher
--     better); d_rapm = points per 100 ALLOWED while defending (lower better);
--     net_rapm = o_rapm - d_rapm. League scoring level and home-court live in
--     the (unstored) regression intercept/HCA terms, so 0 means "average".
--   * paired_possessions = the player's total offensive + defensive
--     possessions across both-side-5-man stints — the fit sample, NOT the
--     same accounting as player_on_off's on_possessions. The UI applies a
--     display floor (~250) on this rather than the table gating rows.
--   * lambda / prior record the fit configuration (zero prior ships; the
--     CamPom prior variant exists in the spike harness only).
--
-- Computed offline by `training/rapm.py` (Python owns the sparse solve, like
-- archetypes), season-scoped atomic swap. Ships to prod with the other
-- season rollups.

CREATE TABLE player_rapm (
    season INTEGER NOT NULL,
    player_id UUID NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    team_id UUID REFERENCES teams(id),
    o_rapm DOUBLE PRECISION NOT NULL,
    d_rapm DOUBLE PRECISION NOT NULL,
    net_rapm DOUBLE PRECISION NOT NULL,
    paired_possessions DOUBLE PRECISION NOT NULL,
    stint_count INTEGER NOT NULL,
    lambda DOUBLE PRECISION NOT NULL,
    prior TEXT NOT NULL DEFAULT 'zero',
    fitted_at TIMESTAMP NOT NULL DEFAULT now(),
    PRIMARY KEY (season, player_id)
);

CREATE INDEX idx_player_rapm_team ON player_rapm (season, team_id);
