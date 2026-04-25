-- Add eFG% percentile column. compute_player_percentiles already computes
-- fg/tp/ft/ts splits but not effective_fg_pct, leaving the UI's eFG% rows
-- without a percentile bar.

ALTER TABLE player_percentiles ADD COLUMN IF NOT EXISTS effective_fg_pct_pct DOUBLE PRECISION;
