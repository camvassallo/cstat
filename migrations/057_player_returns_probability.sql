-- Per-player return probability for a contested eligibility case.
--
-- A `contested` row (051) is weighted at a flat 0.5: present in the ceiling,
-- absent from the floor, blended at the midpoint. That is the right answer
-- only when nothing is known. By October 2026 most cases had a known current
-- state — an injunction in force, or one stayed on appeal with the player
-- sitting out exhibitions — and the flat 0.5 priced a player who cannot play
-- today the same as one who is playing under a court order.
--
-- `probability` is that player's chance of playing, strictly between 0 and 1,
-- set from his current legal state (data/returns/README.md). NULL keeps the
-- 0.5. Only meaningful on a `contested` row: a `granted` row is a returner
-- outright, and a settled denial is a deleted row.

ALTER TABLE player_returns
    ADD COLUMN IF NOT EXISTS probability REAL
    CHECK (probability IS NULL OR (probability > 0 AND probability < 1));
