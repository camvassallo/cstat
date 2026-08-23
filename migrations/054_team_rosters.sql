-- Official school-published rosters — the first forward-looking roster signal
-- cstat has ever had, and the data source `docs/redshirt_handling.md` names as
-- the blocker for every case its PR-1/PR-3 work could not reach.
--
-- Why this exists. `players` is box-score-derived: a row exists only once
-- somebody has played a game. That makes the whole roster picture backward-
-- looking, and it leaves four populations invisible to the preseason
-- projection, all of which concentrate in exactly the teams the projection is
-- already weakest on:
--
--   * redshirts staying at the same school (no portal row, no draft row, and
--     Torvik has no class_year for an unplayed season),
--   * D2/D3 up-transfers,
--   * JuCo arrivals,
--   * direct international signings.
--
-- The 247 portal feed sees some of these and resolves none of them: of the
-- 1,575 portal rows for the 2026 cycle, 170 carry a destination but no
-- `cstat_player_id`, because the arriving player has no D-I history to join
-- against. South Alabama had four such commits and still projected with zero
-- arrivals. On the live 2027 board 77 of 364 teams are below
-- `MIN_QUALIFYING_FOR_PROJECTION`.
--
-- WHAT THIS TABLE IS NOT. It is deliberately NOT wired into the roster
-- projection's scored roster, and adding it there is not a small change. The
-- roster-impact calibrator is trained by `train_roster_impact_model.py` on
-- rosters built from `player_season_stats ... games_played >= 5` — players who
-- actually played — so every training roster is free of statless bodies.
-- Feeding it roster-confirmed players with no `cam_v3` is the same train/serve
-- mismatch that got the returner-redshirt exclusion built, measured, and
-- reverted (raw MAE 6.13 -> 6.20, bias +0.22 -> +0.54, 91 team-seasons of
-- coverage lost). This table's job is to inform the CURATED captures --
-- `player_departures` and `player_returns` -- which the projection already
-- reads, and to make roster thinness legible. Scoring these players is a
-- roster-impact retrain with its own accept/reject gates, not a serving-side
-- join. See `docs/roster_impact_retrain_plan.md`.
--
-- TWO TABLES, AND THE SPLIT IS THE POINT. `team_roster_fetches` records what
-- happened when we asked a school for its roster; `team_roster_players`
-- records who was on it. A player's PRESENCE is a fact from a single row. A
-- player's ABSENCE is only meaningful relative to a fetch we are willing to
-- trust, and most of the time we are not:
--
--   * On 2026-08-23 Gonzaga published "2026-27 Men's Basketball Roster
--     (Returners)" containing FOUR players. Diffing a base-season roster
--     against that marks nine returners as departed.
--   * Campbell and Navy were still serving their 2025-26 roster in late
--     August. Diffing against that marks the entire incoming class missing and
--     silently re-adds players who left a year ago.
--
-- Both failures look exactly like a successful fetch from the outside, which
-- is why the verdict is stored per team per season rather than inferred later
-- from row counts. `status = 'ok'` is the ONLY value that licenses an
-- absence-based inference; everything else means "we have some names, trust
-- them individually, conclude nothing from who is missing."

CREATE TABLE IF NOT EXISTS team_roster_fetches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Target cstat season the roster is FOR (2027 = the 2026-27 season).
    -- Not the base season: unlike `player_departures.year` this row describes
    -- the upcoming roster itself, not the season being projected from.
    season INTEGER NOT NULL,
    -- Matches `teams.short_name`, the same natural key the curated captures
    -- use. No FK: the target season's `teams` rows do not exist until the
    -- season is ingested, which is the entire window this table is useful in.
    team_short_name TEXT NOT NULL,

    -- The fetch verdict. Only 'ok' licenses concluding anything from a
    -- player's ABSENCE; see the header.
    --   'ok'              - full roster for the target season, plausible size.
    --   'partial'         - right season, but too few players to be a whole
    --                       roster, or the page says so itself ("(Returners)").
    --                       Players are still stored and still true.
    --   'stale_season'    - the page served a different season than asked for.
    --                       No players stored: they describe the wrong year.
    --   'unsupported'     - reachable, but not a platform we can parse.
    --   'unreachable'     - DNS/TLS/HTTP failure, or no roster page found.
    status TEXT NOT NULL CHECK (status IN
        ('ok','partial','stale_season','unsupported','unreachable')),

    -- Provenance, kept so a surprising row can be re-checked by hand later.
    source_url TEXT,
    -- 'sidearm_nextgen' | 'sidearm_legacy'. NULL when we never identified one.
    platform TEXT,
    -- The page's own roster title, verbatim ("2026-27 Men's Basketball Roster
    -- (Returners)"). This is what the season gate reads and what makes a
    -- 'partial' verdict explicable at a glance.
    roster_title TEXT,
    player_count INTEGER NOT NULL DEFAULT 0,
    -- Human-readable reason, populated for every non-'ok' status.
    note TEXT,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (season, team_short_name)
);

CREATE INDEX IF NOT EXISTS idx_team_roster_fetches_season
    ON team_roster_fetches (season);

CREATE TABLE IF NOT EXISTS team_roster_players (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    fetch_id UUID NOT NULL REFERENCES team_roster_fetches (id) ON DELETE CASCADE,
    season INTEGER NOT NULL,
    team_short_name TEXT NOT NULL,

    player_name TEXT NOT NULL,
    -- Normalized on write via `cstat_core::roster_projection::normalize_player_name`
    -- so the audit's joins against curated captures and `players.name` are an
    -- index lookup rather than a per-row function call.
    normalized_name TEXT NOT NULL,

    jersey TEXT,
    -- Verbatim school label: "Fr.", "R-Jr.", "5th", "Gr.". Deliberately NOT
    -- coerced into cstat's four-value `class_year` vocabulary -- the redshirt
    -- and fifth-year markers are the whole point for the 5-in-5 capture
    -- (`docs/eligibility_5in5.md`), and they are exactly what a coercion to
    -- Fr/So/Jr/Sr would destroy.
    class_year_raw TEXT,
    position TEXT,
    height_inches INTEGER,
    weight_lbs INTEGER,
    hometown TEXT,
    high_school TEXT,
    -- The field that makes this whole ingest worth doing: "Tyler Junior
    -- College", "Concordia-Irvine", "BC Zalgiris", "Texas A&M / Kansas / Rice".
    -- Where a D2/JuCo/international arrival becomes identifiable as one.
    previous_school TEXT,

    UNIQUE (season, team_short_name, normalized_name)
);

CREATE INDEX IF NOT EXISTS idx_team_roster_players_season_team
    ON team_roster_players (season, team_short_name);
CREATE INDEX IF NOT EXISTS idx_team_roster_players_norm
    ON team_roster_players (season, normalized_name);
