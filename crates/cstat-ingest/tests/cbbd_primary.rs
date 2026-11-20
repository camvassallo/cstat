//! CBBD as the primary box source (docs/natstat_migration_plan.md §4.5 step 5).
//!
//! The gap-fill creates the `games` and `players` rows NatStat never listed,
//! from the CBBD shadow tables, and NatStat adopts them when it lists the same
//! game or player later instead of adding a second copy. These tests run the
//! real `gap_fill::run` and the two adoption helpers end to end and pin:
//!
//! - a CBBD final on no game of ours, between two of our teams, is created with
//!   its box, its newcomers created as players carrying their ESPN ids, and a
//!   second run writes nothing more;
//! - a CBBD final that may already be a game of ours (the same two teams a
//!   couple of days off, NatStat's wrong-date case) is not created;
//! - NatStat's later copy of a created game adopts it, in NatStat's
//!   orientation when the two disagree, box `is_home` flipped with it; and
//!   NatStat's player adopts the created player of the same name and team.
//!
//! Isolation follows `gap_fill_replace.rs`: one pinned connection and TEMP
//! tables shadowing every real table touched, each asserted to resolve into
//! `pg_temp*` before anything runs. Skips when `DATABASE_URL` is unset.

use chrono::NaiveDate;
use cstat_ingest::cache::ApiCache;
use cstat_ingest::gap_fill;
use cstat_ingest::ingest::games::{FeedGame, adopt_cbbd_games, adopt_cbbd_player, cbbd_twins};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const SEASON: i32 = 2027;

const STATS: &str = "minutes int, points int, fgm int, fga int, tpm int, tpa int, ftm int, \
    fta int, off_rebounds int, def_rebounds int, total_rebounds int, assists int, steals int, \
    blocks int, turnovers int, fouls int";

fn ddl() -> Vec<String> {
    vec![
        "CREATE TEMP TABLE teams (id uuid PRIMARY KEY, name text, season int, espn_id text)".into(),
        "CREATE TEMP TABLE games (id uuid PRIMARY KEY, natstat_id text UNIQUE, season int, \
            game_date date, home_team_id uuid, away_team_id uuid, home_score int, away_score int, \
            status text, is_neutral_site bool NOT NULL DEFAULT false, is_conference bool, \
            tip_time time, espn_id text, score_source text, home_half1 int, home_half2 int, \
            away_half1 int, away_half2 int, updated_at timestamp DEFAULT now(), \
            UNIQUE (espn_id, season))"
            .into(),
        "CREATE TEMP TABLE players (id uuid PRIMARY KEY, natstat_id text NOT NULL, name text, \
            team_id uuid, season int, espn_id text, updated_at timestamp DEFAULT now(), \
            UNIQUE (natstat_id, season), UNIQUE (espn_id, season))"
            .into(),
        format!(
            "CREATE TEMP TABLE team_game_stats (team_id uuid, game_id uuid, season int, \
             game_date date, opponent_id uuid, is_home bool, win bool, {STATS}, \
             source text NOT NULL DEFAULT 'natstat')"
        ),
        format!(
            "CREATE TEMP TABLE player_game_stats (player_id uuid, game_id uuid, team_id uuid, \
             season int, game_date date, opponent_id uuid, is_home bool, starter bool, \
             {}, source text NOT NULL DEFAULT 'natstat')",
            STATS.replace("minutes int", "minutes float8")
        ),
        "CREATE TEMP TABLE cbbd_games (cbbd_game_id int PRIMARY KEY, espn_event_id text, \
            season int, start_date timestamptz, status text, neutral_site bool, \
            conference_game bool, home_team_id int, away_team_id int, home_team text, \
            away_team text, home_points int, away_points int)"
            .into(),
        format!(
            "CREATE TEMP TABLE cbbd_player_game_stats (cbbd_game_id int, is_home bool, \
             name text, espn_athlete_id text, starter bool, {STATS})"
        ),
        "CREATE TEMP TABLE game_broadcasts (game_id uuid, source_event_id text)".into(),
        "CREATE TEMP TABLE play_by_play (game_id uuid, score_home int, score_vis int, \
            score_diff int, onfloor_home text, onfloor_vis text)"
            .into(),
        "CREATE TEMP TABLE api_cache (id uuid, endpoint text, params_hash text, \
            response_body jsonb, fetched_at timestamp, expires_at timestamp, \
            UNIQUE (endpoint, params_hash))"
            .into(),
    ]
}

const SHADOWED: [&str; 10] = [
    "teams",
    "games",
    "players",
    "team_game_stats",
    "player_game_stats",
    "cbbd_games",
    "cbbd_player_game_stats",
    "game_broadcasts",
    "play_by_play",
    "api_cache",
];

async fn isolated_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect(&url)
        .await
        .expect("connect");
    for ddl in ddl() {
        sqlx::query(&ddl)
            .execute(&pool)
            .await
            .expect("create temp table");
    }
    for table in SHADOWED {
        let schema: String = sqlx::query_scalar(
            "SELECT n.nspname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = to_regclass($1)",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("resolve table");
        assert!(
            schema.starts_with("pg_temp"),
            "{table} resolved to {schema}, not a temp schema; refusing to run"
        );
    }
    Some(pool)
}

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

struct Seed {
    home: Uuid,
    away: Uuid,
}

/// Two teams of ours with ESPN ids (CBBD teams 1 and 2), three mapped players
/// each, and CBBD's final of game 50 / ESPN event 9001 between them on Nov 18
/// (15-11), each side adding one player we have never seen.
async fn seed(pool: &PgPool) -> Seed {
    let (home, away) = (Uuid::new_v4(), Uuid::new_v4());
    for (team, name, espn) in [(home, "Home U", "100"), (away, "Away St", "200")] {
        sqlx::query("INSERT INTO teams VALUES ($1, $2, $3, $4)")
            .bind(team)
            .bind(name)
            .bind(SEASON)
            .bind(espn)
            .execute(pool)
            .await
            .unwrap();
    }
    ApiCache::new(pool.clone())
        .set(
            "cbbd/teams",
            "",
            &serde_json::json!([{"id": 1, "sourceId": "100"}, {"id": 2, "sourceId": "200"}]),
            3600,
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO cbbd_games VALUES (50, '9001', $1, '2026-11-18 23:00:00+00', 'final', \
         false, false, 1, 2, 'Home', 'Away', 15, 11)",
    )
    .bind(SEASON)
    .execute(pool)
    .await
    .unwrap();
    let mut lines: Vec<(bool, String, String, i32)> = Vec::new();
    for (team, home_side, prefix) in [(home, true, "h"), (away, false, "a")] {
        for i in 1..=3 {
            let espn = format!("{prefix}{i}");
            let name = format!("Known {prefix} {i}");
            sqlx::query(
                "INSERT INTO players (id, natstat_id, name, team_id, season, espn_id) \
                         VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(Uuid::new_v4())
            .bind(format!("9{prefix}{i}"))
            .bind(&name)
            .bind(team)
            .bind(SEASON)
            .bind(&espn)
            .execute(pool)
            .await
            .unwrap();
            lines.push((home_side, name, espn, 3));
        }
    }
    lines.push((true, "Fresh Face".into(), "n1".into(), 6));
    lines.push((false, "New Guy".into(), "n2".into(), 2));
    for (is_home, name, espn, pts) in lines {
        sqlx::query(
            "INSERT INTO cbbd_player_game_stats VALUES (50, $1, $2, $3, true, 20, $4, 1, 2, 0, 1, \
             0, 0, 1, 2, 3, 1, 0, 0, 1, 2)",
        )
        .bind(is_home)
        .bind(name)
        .bind(espn)
        .bind(pts)
        .execute(pool)
        .await
        .unwrap();
    }
    Seed { home, away }
}

async fn scalar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn a_game_natstat_never_listed_is_created_with_its_box_and_newcomers() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };
    let s = seed(&pool).await;
    let r = gap_fill::run(&pool, day("2026-11-18"), day("2026-11-18"), false)
        .await
        .unwrap();
    assert_eq!(r.skipped, Vec::<(String, String)>::new());
    assert_eq!((r.games_created, r.players_created), (1, 2));
    assert_eq!((r.team_rows, r.player_rows), (2, 8));

    let (natstat, home, hs, aws, status, src, date): (
        String,
        Uuid,
        i32,
        i32,
        String,
        String,
        NaiveDate,
    ) = sqlx::query_as(
        "SELECT natstat_id, home_team_id, home_score, away_score, status, score_source, \
         game_date FROM games WHERE espn_id = '9001'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(natstat, "espn:9001");
    assert_eq!((home, hs, aws), (s.home, 15, 11));
    assert_eq!((status.as_str(), src.as_str()), ("Final", "cbbd"));
    assert_eq!(date, day("2026-11-18"), "the Eastern date of a 6 PM tip");

    let fresh: (String, Uuid, String) = sqlx::query_as(
        "SELECT natstat_id, team_id, name FROM players WHERE espn_id = 'n1' AND season = 2027",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        fresh,
        ("espn:n1".to_string(), s.home, "Fresh Face".to_string())
    );
    let new_guy_team: Uuid = sqlx::query_scalar("SELECT team_id FROM players WHERE espn_id = 'n2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(new_guy_team, s.away);
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM player_game_stats WHERE source = 'cbbd'"
        )
        .await,
        8
    );

    // A second run finds the game ours and boxed: nothing more is written.
    let again = gap_fill::run(&pool, day("2026-11-18"), day("2026-11-18"), false)
        .await
        .unwrap();
    assert_eq!((again.games_created, again.players_created), (0, 0));
    assert_eq!(again.already_cbbd, 1);
    assert_eq!(scalar(&pool, "SELECT count(*) FROM games").await, 1);
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM player_game_stats").await,
        8
    );
}

#[tokio::test]
async fn a_cbbd_final_that_may_already_be_ours_is_not_created() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };
    let s = seed(&pool).await;
    // NatStat has the game two days early, with no ESPN event on it.
    sqlx::query(
        "INSERT INTO games (id, natstat_id, season, game_date, home_team_id, away_team_id, \
         home_score, away_score, status) VALUES ($1, '1513218', $2, '2026-11-16', $3, $4, 11, 15, \
         'Final')",
    )
    .bind(Uuid::new_v4())
    .bind(SEASON)
    .bind(s.away)
    .bind(s.home)
    .execute(&pool)
    .await
    .unwrap();
    let r = gap_fill::run(&pool, day("2026-11-18"), day("2026-11-18"), false)
        .await
        .unwrap();
    assert_eq!((r.games_created, r.players_created), (0, 0));
    assert!(r.skipped[0].1.contains("same teams"), "{:?}", r.skipped);
    assert_eq!(scalar(&pool, "SELECT count(*) FROM games").await, 1);
    assert_eq!(scalar(&pool, "SELECT count(*) FROM players").await, 6);
}

#[tokio::test]
async fn natstat_adopts_the_created_game_and_player_in_its_own_orientation() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset — skipping");
        return;
    };
    let s = seed(&pool).await;
    gap_fill::run(&pool, day("2026-11-18"), day("2026-11-18"), false)
        .await
        .unwrap();
    let game: Uuid = sqlx::query_scalar("SELECT id FROM games WHERE espn_id = '9001'")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO play_by_play VALUES ($1, 2, 0, 2, 'h1;h2', 'a1;a2')")
        .bind(game)
        .execute(&pool)
        .await
        .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    // NatStat's wrong-opponent listing of the game: one team, the same final
    // the same day. Held back, not adopted. A different score is a different
    // game.
    let other = Uuid::new_v4();
    let twin = |id: &str, hs: i32, aws: i32| FeedGame {
        natstat_id: id.into(),
        game_date: day("2026-11-18"),
        home_team_id: Some(s.home),
        away_team_id: Some(other),
        home_score: Some(hs),
        away_score: Some(aws),
    };
    let twins = cbbd_twins(
        &mut conn,
        SEASON,
        &[twin("778", 15, 11), twin("779", 70, 60)],
    )
    .await
    .unwrap();
    assert_eq!(
        twins.into_iter().collect::<Vec<_>>(),
        vec!["778".to_string()]
    );

    // NatStat lists it a day late, the other way round. A game for another
    // pair, and a natstat id already stored, adopt nothing.
    let feed = [
        FeedGame {
            natstat_id: "777".into(),
            game_date: day("2026-11-19"),
            home_team_id: Some(s.away),
            away_team_id: Some(s.home),
            home_score: Some(11),
            away_score: Some(15),
        },
        twin("778", 15, 11),
    ];
    assert_eq!(adopt_cbbd_games(&mut conn, SEASON, &feed).await.unwrap(), 1);
    assert_eq!(adopt_cbbd_games(&mut conn, SEASON, &feed).await.unwrap(), 0);
    // Once NatStat holds the game, nothing is a created game to twin.
    assert!(
        cbbd_twins(&mut conn, SEASON, &[twin("778", 15, 11)])
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);

    let (natstat, home, away, hs, aws): (String, Uuid, Uuid, i32, i32) = sqlx::query_as(
        "SELECT natstat_id, home_team_id, away_team_id, home_score, away_score \
         FROM games WHERE id = $1",
    )
    .bind(game)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(natstat, "777");
    assert_eq!((home, away, hs, aws), (s.away, s.home, 11, 15));
    let away_side_home: bool =
        sqlx::query_scalar("SELECT bool_and(is_home) FROM team_game_stats WHERE team_id = $1")
            .bind(s.away)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(away_side_home, "box rows follow the game's new orientation");
    let players_home: bool =
        sqlx::query_scalar("SELECT bool_and(is_home) FROM player_game_stats WHERE team_id = $1")
            .bind(s.away)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(players_home);
    let pbp: (i32, i32, i32, String) =
        sqlx::query_as("SELECT score_home, score_vis, score_diff, onfloor_home FROM play_by_play")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pbp, (0, 2, -2, "a1;a2".to_string()));

    // NatStat's player of the same name on the same team takes the created
    // row; another team's, or another name, takes nothing.
    let created: Uuid = sqlx::query_scalar("SELECT id FROM players WHERE espn_id = 'n1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        adopt_cbbd_player(&pool, "555", SEASON, s.away, "Fresh Face")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        adopt_cbbd_player(&pool, "555", SEASON, s.home, "Someone Else")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        adopt_cbbd_player(&pool, "555", SEASON, s.home, "Fresh  Face Jr.")
            .await
            .unwrap(),
        Some(created)
    );
    let natstat: String = sqlx::query_scalar("SELECT natstat_id FROM players WHERE id = $1")
        .bind(created)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(natstat, "555");
}
