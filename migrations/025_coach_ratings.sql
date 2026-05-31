-- Coach-Above-Expectation (CAE) ratings, computed off the `coaches` /
-- `coach_seasons` mapping (migration 024) joined to the roster-only team
-- projection backtest. Methodology + feasibility verdict in
-- `docs/coach_above_expectation_design.md`; the offline compute job is
-- `training/compute_cae.py` (cae_feasibility.py thresholds act as guards).
--
-- CAE is a **descriptive** grade: how much a team out/under-performs the
-- talent on its roster, attributed to the coach, aggregated across the
-- coach's career with empirical-Bayes shrinkage. It is NOT a predictor
-- (the predictive-lift test was refuted twice — see the design doc §3).
--
--   CAE_season  = actual_team_AdjEM − roster-only projection (phase_b).
--                 The HEADLINE rating uses this raw residual, framed as
--                 "coach×program over-expectation" (design §2): at 5 seasons
--                 coach and program can't be cleanly separated, and raw carries
--                 the only statistically-significant persistence (split-half
--                 +0.114). A projection-quartile-de-biased variant is stored
--                 alongside as the conservative, prestige-adjusted view (it
--                 strips the program component — see `cae_*adj*` columns and
--                 `training/compute_cae.py`; quartiles cut on phase_b not the
--                 outcome, so it avoids the actual-quartile regression artifact
--                 of project_projection_q1_bias_refuted).
--   CAE_career  = EB_shrink(mean over the coach's seasons), k ≈ 6.4
--                 season-equivalents (raw). Heavily shrunk: a 1–2 season coach
--                 is mostly prior (≈0). Always shown with a credibility band.
--
-- Two tables, mirroring the entity/mapping split of 024:
--   `coach_season_cae` — one row per scored (coach, team, season): the raw
--                        and de-biased residual feeding the career rating
--                        and the per-season sparkline.
--   `coach_ratings`    — one career-level row per coach: the shrunk grade,
--                        reliability, and credibility interval (leaderboard).

-- Per team-season CAE residual (the sparkline + the career aggregation input).
CREATE TABLE IF NOT EXISTS coach_season_cae (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    coach_id UUID NOT NULL REFERENCES coaches(id),

    -- cstat-season year and the cross-season program key (denormalized from
    -- the matched team, same as coach_seasons.team_natstat_id).
    season INTEGER NOT NULL,
    team_natstat_id TEXT NOT NULL,

    -- The realized team strength and the roster-only projection it is graded
    -- against (phase_b from the projections backtest).
    actual_adjem DOUBLE PRECISION NOT NULL,
    projection DOUBLE PRECISION NOT NULL,

    -- cae_raw      = actual − projection (signed; + = beat the roster).
    -- cae_debiased = cae_raw minus the projection-quartile mean residual
    --                (removes phase_b's low-end miscalibration). This is the
    --                value the career rating aggregates.
    cae_raw DOUBLE PRECISION NOT NULL,
    cae_debiased DOUBLE PRECISION NOT NULL,

    computed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- One score per coach per season (a coach holds one job per season).
    UNIQUE (coach_id, season)
);

CREATE INDEX IF NOT EXISTS coach_season_cae_coach_idx
    ON coach_season_cae (coach_id, season);

-- Career-level shrunk rating (the /coaches leaderboard entity).
CREATE TABLE IF NOT EXISTS coach_ratings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    coach_id UUID NOT NULL REFERENCES coaches(id) UNIQUE,

    -- Number of scored seasons feeding the rating (the credibility weight).
    n_seasons INTEGER NOT NULL,

    -- cae_raw_mean    = unshrunk mean of the per-season raw residuals.
    -- cae_shrunk      = EB estimate = (n / (n + k)) · cae_raw_mean, shrunk
    --                   toward 0 (the population mean), k ≈ 6.4. This is the
    --                   HEADLINE rating; default-sort the leaderboard by it,
    --                   never by the unshrunk mean.
    cae_raw_mean DOUBLE PRECISION NOT NULL,
    cae_shrunk DOUBLE PRECISION NOT NULL,

    -- Prestige-adjusted (projection-quartile-de-biased) parallel of the above,
    -- shrunk with its own k (≈10.4). Strips the program component; a
    -- conservative lower bound, surfaced for transparency / a future
    -- prestige-adjusted leaderboard toggle. NOT the default sort.
    cae_adj_mean DOUBLE PRECISION NOT NULL,
    cae_adj_shrunk DOUBLE PRECISION NOT NULL,

    -- reliability = n / (n + k) ∈ [0,1]; the shrinkage weight, surfaced so a
    -- thin-tenure rating reads as low-confidence.
    reliability DOUBLE PRECISION NOT NULL,

    -- 95% credibility interval on the shrunk rating (posterior ± 1.96·sd).
    ci_low DOUBLE PRECISION NOT NULL,
    ci_high DOUBLE PRECISION NOT NULL,

    -- Inclusive season span of the scored tenure (for display: "2022–2026").
    first_season INTEGER NOT NULL,
    last_season INTEGER NOT NULL,

    computed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Leaderboard default sort (shrunk rating, descending).
CREATE INDEX IF NOT EXISTS coach_ratings_shrunk_idx
    ON coach_ratings (cae_shrunk DESC);
