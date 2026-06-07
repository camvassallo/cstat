-- Index play_by_play.player_id so the compute player-dedup step can reassign a
-- duplicate player's PBP rows to the surviving primary before deleting the dup.
-- Without it, that reassignment (and any player-scoped PBP lookup) seq-scans the
-- multi-million-row table. Partial (NOT NULL) because team/game-level event rows
-- carry a null player_id and never need to be reassigned. On prod the table is
-- empty (PBP is local-only), so this builds instantly there.
CREATE INDEX IF NOT EXISTS idx_play_by_play_player_id
    ON play_by_play (player_id)
    WHERE player_id IS NOT NULL;
