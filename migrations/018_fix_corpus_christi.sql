-- Migration 017 backfilled TAMC's short_name from torvik_player_stats.team_name,
-- which Torvik truncates at 22 chars ("Texas A&M Corpus Chris"). The JSON source
-- of truth (data/team_short_names.json) carries the correct full form, so the
-- NatStat re-ingest already writes "Texas A&M Corpus Christi" — this just
-- corrects the column for environments where 017 ran before the JSON was fixed.

UPDATE teams
SET short_name = 'Texas A&M Corpus Christi',
    updated_at = now()
WHERE natstat_id = 'TAMC'
  AND short_name = 'Texas A&M Corpus Chris';
