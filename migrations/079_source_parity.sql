-- The NatStat-vs-CollegeBasketballData parity record (docs/natstat_migration_plan.md
-- §5): what the nightly's `source_parity` step found, kept so the three-week
-- acceptance window is a query over stored nights instead of a report someone
-- remembered to run.
--
-- Both tables are rewritten for each Eastern day the step covers (the last
-- three, `source_parity::PARITY_DAYS`), so a day's row is the last look at it,
-- after late box scores have had a chance to land. `cstat-ingest source-parity
-- --record` writes the same rows for any range; `--summary-days N` reads them.
--
-- The "ours" side is NatStat's own rows only: box rows the gap-fill wrote
-- (`source = 'cbbd'`, migration 077), a final score it set
-- (`games.score_source = 'cbbd'`) and games it created are CBBD compared with
-- itself, so they are counted in `filled_by_cbbd` and never as agreement.
--
-- Prod-written by the nightly and excluded from `scripts/sync_to_prod.sh`.

CREATE TABLE IF NOT EXISTS source_parity_days (
    game_date DATE PRIMARY KEY,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The `ingest_runs.run_id` of the nightly that wrote the row; NULL when
    -- written from the CLI.
    run_id UUID,
    -- Our games that day.
    games INT NOT NULL,
    -- Joined to a CBBD game through the ESPN event id.
    matched INT NOT NULL,
    -- Final in NatStat (its own final, not one the gap-fill set) but not
    -- final in CBBD, or not in CBBD at all.
    missing_in_cbbd INT NOT NULL,
    -- CBBD-final games where both sources have a box score: the comparison.
    compared INT NOT NULL,
    -- Compared games with no score, team-box or player-stat difference.
    identical INT NOT NULL,
    team_box_diff INT NOT NULL,
    player_diff INT NOT NULL,
    -- Games both sources have a final score for, and how many disagree.
    score_compared INT NOT NULL,
    score_disagreements INT NOT NULL,
    -- CBBD-final games with a CBBD box and no NatStat box, and the reverse.
    cbbd_only INT NOT NULL,
    natstat_only INT NOT NULL,
    -- Games carrying anything the gap-fill wrote: box rows, a final score, or
    -- the game itself.
    filled_by_cbbd INT NOT NULL,
    -- CBBD games that day with no game of ours.
    cbbd_unmatched INT NOT NULL
);

-- One row per game with a value disagreement (score, team box or player
-- stats), as of the last look at its day, for a later step to arbitrate
-- against ESPN. Values are `ours/cbbd`.
CREATE TABLE IF NOT EXISTS source_parity_games (
    game_id UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    game_date DATE NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    run_id UUID,
    matchup TEXT NOT NULL,
    espn_event_id TEXT,
    cbbd_game_id INT,
    -- "home-away" in our frame, both sources.
    our_score TEXT,
    cbbd_score TEXT,
    score_match BOOLEAN,
    -- `side.field ours/cbbd`, e.g. `home.fouls 26/27`.
    team_diffs TEXT[] NOT NULL DEFAULT '{}',
    -- `name pts ours/cbbd` / `name reb ours/cbbd`.
    player_diffs TEXT[] NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_source_parity_games_date ON source_parity_games (game_date);
