-- Players the projection assumes are gone who are in fact coming back — the
-- inverse of `player_departures` (migration 046), and the first channel cstat
-- has ever had for eligibility that its `class_year` label cannot express.
--
-- Context. On 2026-06-23 the NCAA D-I Cabinet adopted an age-based eligibility
-- model, "5-in-5": five years of eligibility beginning the academic year after
-- an athlete turns 19 or graduates high school, replacing "four seasons of
-- competition within a five-year window from enrollment". It takes effect for
-- 2026-27 — cstat season 2027, the season the Future page projects today.
--
-- cstat's entire eligibility mechanism is one string comparison: a roster row
-- whose `class_year` is 'Sr' is assumed gone next season (issue #220). That
-- inference is now wrong for an unknown but large population. It is also the
-- LARGEST departure channel — 2026 carried 1,679 senior-labelled players, 43%
-- of all positive `cam_gbpm_v3` in the season — so the error is not marginal.
--
-- Two populations, and only one of them is self-healing:
--
--   * Seniors who take the extra year somewhere ELSE are already handled. They
--     appear in the 247 portal feed, and the projection's arrivals path has no
--     class filter, so they land on their new team correctly. (2026 portal
--     entrants after June 1: 53 of 56 that resolve to a cstat player were
--     `Sr`-labelled.)
--   * Seniors who take the extra year AT THE SAME SCHOOL are invisible. No
--     feed reports them: they never enter the portal, they are not draft
--     entrants, and Torvik's `class_year` for the new season does not exist
--     until games are played. They are simply deleted from their team's
--     projection.
--
-- This table is the capture for the second group, plus the litigation the rule
-- is under. Unlike `player_departures`, a row here carries a `status`, because
-- "is this player eligible" is a genuinely open question rather than a fact
-- awaiting transcription:
--
--   'granted'   — eligibility is settled and the player is on the roster.
--                 The projection treats them as a normal returner.
--   'contested' — a claim exists (a waiver filed, an injunction, a suit
--                 pending) and could go either way. The projection routes them
--                 to the `uncertain` bucket, which is already materialized in
--                 the ceiling scenario and excluded from the floor, so the
--                 team's projected band widens to span both outcomes and the
--                 UI shows a `?` rather than a false certainty.
--
-- Routing the contested case through the existing NBA-draft `declared`
-- machinery is deliberate: the shape of the problem is identical (a known
-- player, an unresolved binary, a resolution date we do not control), and it
-- means this needs no new model, no new feature, and no change to the served
-- 27-feature roster-impact vector.
--
-- `year` is the BASE cstat-season the player is returning FROM — the same
-- convention as `player_departures.year` and `draft_entrants.year`. The
-- projection for target season `year + 1` reads these rows. Matching to a
-- roster player happens at projection time by normalized (name, team), so no
-- resolved player FK is stored here.
--
-- Curate conservatively. Marking the whole senior class 'contested' would blow
-- every team's floor/ceiling band out to uselessness; a row belongs here only
-- when there is an actual report about an actual player.

CREATE TABLE IF NOT EXISTS player_returns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    year INTEGER NOT NULL,
    player_name TEXT NOT NULL,
    current_team TEXT NOT NULL,
    -- 'granted' | 'contested'. Behavior-bearing, unlike `player_departures.reason`:
    -- it selects between the `returning` and `uncertain` buckets. Constrained
    -- so a typo fails the insert rather than silently falling through to a
    -- default that projects the wrong roster.
    status TEXT NOT NULL CHECK (status IN ('granted', 'contested')),
    -- Why this player has eligibility the `class_year` label denies.
    -- Display-only vocabulary: '5in5' (the age-based rule), 'waiver',
    -- 'injunction' (court-ordered while litigation proceeds), 'medical'
    -- (medical-hardship year), 'other'.
    reason TEXT NOT NULL DEFAULT '5in5',
    -- Provenance: a URL or outlet slug for the report this row was taken from.
    source TEXT,
    -- Optional human note for anything the columns above don't carry.
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (year, player_name, current_team)
);

CREATE INDEX IF NOT EXISTS idx_player_returns_year ON player_returns (year);
