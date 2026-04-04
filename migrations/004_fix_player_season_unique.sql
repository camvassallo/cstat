-- Allow players who transferred mid-season to have separate stats per team
ALTER TABLE player_season_stats
    DROP CONSTRAINT player_season_stats_player_id_season_key;

ALTER TABLE player_season_stats
    ADD CONSTRAINT player_season_stats_player_team_season_key UNIQUE (player_id, team_id, season);
