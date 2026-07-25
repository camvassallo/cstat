-- Non-portal, non-draft program exits — the fourth departure channel the
-- roster projection was missing.
--
-- Before this table the projection could only drop a prior-season player for
-- three reasons: `players.class_year = 'Sr'` (graduation), a `transfers` row
-- (247 portal), or a `draft_entrants` row (NBA draft). Anything else fell
-- through to `returning` and inflated the target season. Issue #215: Mario
-- Saint-Supery left Gonzaga in July 2026 to sign a four-year deal with
-- Valencia (ACB/EuroLeague) — a freshman, never in the portal, never an NBA
-- entrant, so the 2027 projection kept a 92nd-percentile CamPom guard on the
-- roster.
--
-- Deliberately generic rather than an `overseas_signings` table: the same
-- blind spot swallows medical retirements, dismissals, players who simply
-- quit, and non-D1 moves the 247 feed never lists. All of them are "gone, and
-- no other feed will tell us."
--
-- `year` is the BASE cstat-season the player is leaving — same convention as
-- `draft_entrants.year`. The projection for target season `year + 1` reads
-- these rows. Matching to a roster player happens at projection time by
-- normalized (name, team), so no resolved player FK is stored here; that keeps
-- the capture writable before the join is known, exactly like `draft_entrants`.
--
-- Every row is a FIRM departure. There is no `declared`-style uncertainty
-- status: unlike the NBA draft there is no withdrawal deadline to wait on, so
-- a rumor should simply not be recorded until it is confirmed. The `reason`
-- column is display-only — presence of the row is what removes the player, so
-- an unrecognized reason string still behaves correctly.

CREATE TABLE IF NOT EXISTS player_departures (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    year INTEGER NOT NULL,
    player_name TEXT NOT NULL,
    current_team TEXT NOT NULL,
    -- Display-only vocabulary: 'pro_overseas', 'pro_other', 'retired',
    -- 'dismissed', 'left_program' (catch-all). Not behavior-bearing.
    reason TEXT NOT NULL DEFAULT 'left_program',
    -- Where they went, free-text, for the UI chip: 'Valencia (ACB)', 'G League
    -- Ignite', NULL when unknown or not applicable (retirement).
    destination TEXT,
    -- Provenance: a URL or outlet slug for the report this row was taken from.
    source TEXT,
    -- Optional human note for anything the columns above don't carry.
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (year, player_name, current_team)
);

CREATE INDEX IF NOT EXISTS idx_player_departures_year ON player_departures (year);
