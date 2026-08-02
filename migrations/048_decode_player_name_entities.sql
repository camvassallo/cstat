-- Issue #243: decode the HTML entities NatStat left in `players.name`.
--
-- 177 rows across 2015, 2016, 2022, 2023 and 2024 stored the escape sequence
-- verbatim rather than the character it stands for:
--
--   D&#039;Angelo Russell         -> D'Angelo Russell
--   Devonte&#039; Graham          -> Devonte' Graham
--   Ja&#39;Vier Francis           -> Ja'Vier Francis
--   Gregory &quot;GG&quot; Jackson -> Gregory "GG" Jackson
--
-- `players.name` is user-visible, so these were rendering wrong on the site.
-- The write sites now decode at ingest (`ingest::utils::decode_html_entities`,
-- applied in `players.rs`, `games.rs` and `bootstrap_csv.rs` — the NatStat CSV
-- exports carry the escaped form, which is where most of the 2015/2016 rows
-- came from), so this is a one-time correction of the rows already stored.
--
-- One statement per entity, in the same order the Rust helper's single
-- left-to-right pass resolves them: `&amp;` runs LAST so a double-escaped
-- `&amp;#039;` lands on the literal `&#039;` instead of being decoded twice.
-- A bare `&` (Texas A&M) matches no pattern and is left alone.

UPDATE players SET name = replace(name, '&#039;', ''''), updated_at = now()
 WHERE name LIKE '%&#039;%';

UPDATE players SET name = replace(name, '&#39;', ''''), updated_at = now()
 WHERE name LIKE '%&#39;%';

UPDATE players SET name = replace(name, '&#x27;', ''''), updated_at = now()
 WHERE name LIKE '%&#x27;%';

UPDATE players SET name = replace(name, '&apos;', ''''), updated_at = now()
 WHERE name LIKE '%&apos;%';

UPDATE players SET name = replace(name, '&quot;', '"'), updated_at = now()
 WHERE name LIKE '%&quot;%';

UPDATE players SET name = replace(name, '&lt;', '<'), updated_at = now()
 WHERE name LIKE '%&lt;%';

UPDATE players SET name = replace(name, '&gt;', '>'), updated_at = now()
 WHERE name LIKE '%&gt;%';

UPDATE players SET name = replace(name, '&amp;', '&'), updated_at = now()
 WHERE name LIKE '%&amp;%';
