-- Whether the book had `espn_game_lines.home_team_id` favoured at the open.
--
-- When the two line sources name opposite favourites, this settles most of
-- them without looking at the result or our projection: a closing line that
-- agrees with its own open stands, and one that does not is unreliable too.
-- Of the four such games in opening week 2027, Florida over Miami and Florida
-- State over Florida A&M stand, and the two where the source flipped its own
-- favourite stay ungraded. NULL (rows read before this column, or none listed)
-- counts as not agreeing.
ALTER TABLE espn_game_lines
    ADD COLUMN IF NOT EXISTS home_favored_at_open BOOLEAN;
