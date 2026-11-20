-- Shadow copies of CollegeBasketballData's games, box scores and play-by-play
-- coverage, written by the nightly's `cbbd_shadow` step and read only by
-- `cstat-ingest source-parity`. Nothing the site serves reads these tables:
-- they exist to measure CBBD against the NatStat-fed tables before any feed
-- moves (docs/natstat_migration_plan.md §4.2-4.3).
--
-- Keyed on CBBD's own game and team ids, as its responses are. A game's ESPN
-- event id (`espn_event_id`, CBBD's `sourceId`) is what joins it to ours,
-- through `game_broadcasts.source_event_id`.
--
-- Plays are kept as one summary row per game, not one row per play: the
-- comparison needs counts and coverage, and a season of raw plays is ~2.7M
-- rows on the prod database for a step nothing serves. The raw plays arrive
-- with the play-by-play cutover.
--
-- Prod-written: excluded from scripts/sync_to_prod.sh.

CREATE TABLE cbbd_games (
    cbbd_game_id     INTEGER PRIMARY KEY,
    espn_event_id    TEXT,
    season           INTEGER NOT NULL,
    start_date       TIMESTAMPTZ NOT NULL,
    status           TEXT,
    neutral_site     BOOLEAN,
    conference_game  BOOLEAN,
    home_team_id     INTEGER,
    home_team        TEXT,
    home_points      INTEGER,
    home_elo_start   INTEGER,
    home_elo_end     INTEGER,
    away_team_id     INTEGER,
    away_team        TEXT,
    away_points      INTEGER,
    away_elo_start   INTEGER,
    away_elo_end     INTEGER,
    -- When the shadow step first saw the game, and first saw its box score.
    -- Never overwritten, so `source-parity` can say which feed had a game's
    -- box score first (NatStat's side is `team_game_stats.created_at`).
    first_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    first_box_at     TIMESTAMPTZ,
    fetched_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX cbbd_games_espn_event_id ON cbbd_games (espn_event_id);
CREATE INDEX cbbd_games_start_date ON cbbd_games (start_date);

CREATE TABLE cbbd_team_game_stats (
    cbbd_game_id   INTEGER NOT NULL REFERENCES cbbd_games (cbbd_game_id) ON DELETE CASCADE,
    cbbd_team_id   INTEGER NOT NULL,
    team           TEXT NOT NULL,
    is_home        BOOLEAN NOT NULL,
    points         INTEGER,
    fgm            INTEGER,
    fga            INTEGER,
    tpm            INTEGER,
    tpa            INTEGER,
    ftm            INTEGER,
    fta            INTEGER,
    off_rebounds   INTEGER,
    def_rebounds   INTEGER,
    total_rebounds INTEGER,
    assists        INTEGER,
    steals         INTEGER,
    blocks         INTEGER,
    turnovers      INTEGER,
    fouls          INTEGER,
    possessions    INTEGER,
    fetched_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cbbd_game_id, cbbd_team_id)
);

CREATE TABLE cbbd_player_game_stats (
    cbbd_game_id     INTEGER NOT NULL REFERENCES cbbd_games (cbbd_game_id) ON DELETE CASCADE,
    cbbd_athlete_id  INTEGER NOT NULL,
    espn_athlete_id  TEXT,
    cbbd_team_id     INTEGER NOT NULL,
    is_home          BOOLEAN NOT NULL,
    name             TEXT NOT NULL,
    starter          BOOLEAN,
    minutes          INTEGER,
    points           INTEGER,
    fgm              INTEGER,
    fga              INTEGER,
    tpm              INTEGER,
    tpa              INTEGER,
    ftm              INTEGER,
    fta              INTEGER,
    off_rebounds     INTEGER,
    def_rebounds     INTEGER,
    total_rebounds   INTEGER,
    assists          INTEGER,
    steals           INTEGER,
    blocks           INTEGER,
    turnovers        INTEGER,
    fouls            INTEGER,
    fetched_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cbbd_game_id, cbbd_athlete_id)
);

CREATE TABLE cbbd_game_play_summary (
    cbbd_game_id         INTEGER PRIMARY KEY REFERENCES cbbd_games (cbbd_game_id) ON DELETE CASCADE,
    plays                INTEGER NOT NULL,
    shots                INTEGER NOT NULL,
    shots_with_location  INTEGER NOT NULL,
    plays_full_on_floor  INTEGER NOT NULL,
    fetched_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
