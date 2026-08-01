-- Player display names (issue #243 follow-up).
--
-- `players.name` is NatStat's legal name, and it is the join key five
-- different resolvers match on (transfers, recruits, draft, awards, and the
-- Torvik matcher itself). It must stay stable, so the presentation name lives
-- in its own column and the API serves `COALESCE(display_name, name)`.
--
-- NULL means "no better name than `name`", which is the case for ~96% of
-- players. `compute_display_names` rewrites the column from scratch on every
-- run, so it is derived state, never hand-edited.
--
-- WHAT GOES IN IT, and why the rule is narrow. Torvik carries the name people
-- actually use, but it is NOT uniformly the better source: on the 1,043
-- player-seasons where the two disagree substantively, 247Sports (a third,
-- independent feed) sides with NatStat 44-to-20. Torvik is frequently the one
-- with the typo — "Jeffery Solarin" for Jeffrey, "Ezra Ausur" for Ausar,
-- "Martez Robinson" for Martaz. Adopting it wholesale would trade one class of
-- wrong names for another.
--
-- So only two things land here:
--   1. Generational suffixes, restored mechanically. NatStat drops "Jr." /
--      "III" / "IV"; Torvik keeps them. This is safe *by construction* — the
--      rule only fires when the two names are identical after stripping the
--      suffix, so it cannot introduce a spelling that wasn't already agreed.
--      ~2,000 players, including Jaren Jackson Jr., Marvin Bagley III and
--      Wade Taylor IV, all of whom currently render bare.
--   2. Curated overrides from `data/player_display_names.json`, for the
--      marquee cases where the legal name is not the known one (Obadiah ->
--      Obi Toppin, Temetrius -> Ja Morant). Human-checked, deliberately small.
ALTER TABLE players ADD COLUMN display_name TEXT;

COMMENT ON COLUMN players.display_name IS
    'Presentation name when it differs from the legal `name`: a restored generational suffix or a curated override. NULL means `name` is already correct. Derived by compute_display_names; never hand-edit (issue #243).';

-- Search hits both columns, so the trigram/ILIKE path over display_name wants
-- the same treatment `name` gets. Cheap: the column is NULL for most rows.
CREATE INDEX IF NOT EXISTS idx_players_display_name ON players (display_name)
    WHERE display_name IS NOT NULL;
