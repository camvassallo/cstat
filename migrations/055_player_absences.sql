-- Two curated-capture gaps that both showed up on the 2027 preseason board.
--
-- 1. `player_absences` — players who are on a roster for the target season
--    but will not play in it (issue #217). A torn Achilles in July, a medical
--    redshirt announced in April: public, one-off, and invisible to every feed
--    cstat reads. The roster-impact calibrator has no schedule, opponent or
--    form features that could absorb the absence the way the game model does,
--    so an out-for-the-season player contributed his full projected CAM to his
--    team (Solo Ball, UConn, 10.3; Brandon McCoy Jr., Michigan, 7.0).
--
--    Deliberately NOT a `player_departures` row: the player has not left the
--    program and is expected back the season after, so recording him as an
--    exit would write him off in every downstream surface. The projection
--    removes him from the scored roster wherever he would otherwise land — a
--    returner, a portal arrival, or an incoming recruit — and lists him on the
--    team as out for the season.
--
--    `year` is the BASE season, the same convention as `player_departures`,
--    `player_returns` and `draft_entrants`: a 2026 row affects the 2027
--    projection. `current_team` is the team he would play FOR in the target
--    season (his destination when he is a transfer or a recruit), because that
--    is the roster he is missing from. Matched at projection time by
--    normalized (name, team); no player FK stored.
--
-- 2. `player_returns.destination` — a 5-in-5 returner who took his extra year
--    at a NEW school without appearing in the 247 portal feed. Out-of-
--    eligibility players signing after a court order often never enter the
--    portal, so a `granted` row keyed by his base team projected him back at a
--    school he had left (Jordan Pope: Texas row, plays for Texas A&M). When set,
--    the row is a move: he departs `current_team` and arrives at
--    `destination`, with the row's status following him there.

CREATE TABLE IF NOT EXISTS player_absences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    year INTEGER NOT NULL,
    player_name TEXT NOT NULL,
    current_team TEXT NOT NULL,
    -- Display-only description ("torn ACL", "wrist surgery, medical redshirt").
    injury TEXT NOT NULL,
    -- Provenance: a URL for the report (RotoWire player page preferred).
    source TEXT,
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (year, player_name, current_team)
);

CREATE INDEX IF NOT EXISTS idx_player_absences_year ON player_absences (year);

ALTER TABLE player_returns ADD COLUMN IF NOT EXISTS destination TEXT;
