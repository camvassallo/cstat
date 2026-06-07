-- P3: possession & tempo normalization for PBP lineups. The P2b stint/aggregate
-- derivations shipped raw-count only (points_for/against, plus_minus) — no
-- denominator, so a 200-possession lineup and a 5-possession lineup summed into
-- the same +/-. This adds the tempo-free unit every downstream surface needs
-- (on/off splits, the `lineup_quality` ML feature, rate-stat lineup UI).
--
-- Possessions use cstat's canonical estimate `FGA - ORB + TOV + 0.44*FTA` (the
-- same convention as tempo / AdjO / AdjD in compute_adjusted_efficiency, the
-- 0.44 FTA coefficient — NOT 0.475), counted from play_by_play tags within each
-- stint's [start_seq, end_seq] window and attributed to the acting team. Stored
-- as DOUBLE PRECISION to preserve the fractional 0.44*FTA term across rollup.
-- Design of record: docs/pbp_methodology.md.

-- Per-stint, per-side possessions + on-floor duration. lineup_stints stays
-- local-only (excluded from sync_to_prod alongside play_by_play).
ALTER TABLE lineup_stints
    ADD COLUMN IF NOT EXISTS possessions_for     DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS possessions_against DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS seconds             INT              NOT NULL DEFAULT 0;

-- Season lineup rollup — ships to prod, backs the team top-5 lineups UI. ortg /
-- drtg are points per 100 possessions, on the same scale as team AdjO / AdjD;
-- minutes is on-floor wall clock (seconds / 60).
ALTER TABLE lineup_aggregates
    ADD COLUMN IF NOT EXISTS possessions_for     DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS possessions_against DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS minutes             DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS ortg                DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS drtg                DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS net_rtg             DOUBLE PRECISION;
