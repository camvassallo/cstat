-- Partial-season absences (issue #219). `player_absences` (055) records a
-- player who misses the WHOLE target season. A player out for part of it with
-- a reported return — JT Toppin, Texas Tech, ACL, targeting late December — is
-- a different claim: he will be on the team, for a known share of the season.
--
-- `availability` is that share, strictly between 0 and 1. NULL keeps the 055
-- meaning (out for the season). When set, the projection keeps the player on
-- the roster and scores the team as the availability-weighted mix of its
-- rosters with and without him, through the same floor/ceiling machinery the
-- uncertain draft and eligibility cohorts already use.

ALTER TABLE player_absences
    ADD COLUMN IF NOT EXISTS availability REAL
    CHECK (availability IS NULL OR (availability > 0 AND availability < 1));
