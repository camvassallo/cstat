-- The date window each ingest run actually covered.
--
-- The M5b backfill self-heal needs to answer "which game dates have we already
-- ingested?" so it can widen a defaulted window back over any nights the cron
-- missed. Without this it could only approximate, using `MAX(ended_at)` — the
-- run's wall-clock finish time. That proxy holds only for default-window runs;
-- it breaks exactly when a human is involved, which is the expected state
-- during an outage:
--
--   cron dies after 11-05, nights 11-06/07 skipped. On 11-08 the operator runs
--   a manual `nightly --from 11-07 --to 11-08` (range slightly wrong, or just
--   probing the fix). That writes games/ok stamped ended_at = 11-08, so the next
--   cron sees "last success 11-08", finds no gap, and never heals 11-06. The
--   games are lost silently, under a green heartbeat.
--
-- Recording the window lets the heal scan real coverage instead of inferring it:
-- `first_uncovered_ingest_date` walks back day by day looking for the earliest
-- date no *complete* run covered, so in the case above it finds 11-06 regardless
-- of the 11-08 row that landed after it. Note a high-water mark (MAX(window_end))
-- would NOT fix that example — it would still read 11-08 — which is why the scan
-- is a gap search rather than a frontier.
--
-- "Complete" means every window-scoped box-score step (games, player_perfs,
-- team_perfs) succeeded for that run: `games` records ok before `player_perfs`
-- can abort the run, so a half-finished run must not mark its window covered.
--
-- These record what a run actually COVERED, which is not the same as the range
-- it was asked for. The cron fires 09:30 UTC with a yesterday..today window, but
-- date D's games don't tip until ~D 23:00 UTC — a run on D ingests none of D's
-- games. So the writer clamps the stamped end to `today_utc() - 1`; a run with
-- nothing complete in its range (e.g. `--from today --to today`) stamps no
-- window at all. Without that clamp every run claims its own run-day, and every
-- outage silently drops exactly one date: the last good run's day.
--
-- Nullable: rows written before this migration have no window. They are ignored
-- rather than treated as covering nothing — the scan floors at the earliest
-- window it knows about, so a ledger of only legacy rows simply yields "no gap"
-- instead of trying to re-ingest the whole lookback.
ALTER TABLE ingest_runs ADD COLUMN IF NOT EXISTS window_start DATE;
ALTER TABLE ingest_runs ADD COLUMN IF NOT EXISTS window_end   DATE;

-- The coverage scan: successful box-score steps, newest windows first.
CREATE INDEX IF NOT EXISTS idx_ingest_runs_step_window_end
    ON ingest_runs (step, window_end DESC)
    WHERE status = 'ok';
