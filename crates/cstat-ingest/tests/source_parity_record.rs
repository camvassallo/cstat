//! The stored NatStat-vs-CBBD parity record (migration 079,
//! docs/natstat_migration_plan.md §5).
//!
//! Pins what the nightly's `source_parity` step counts and stores: NatStat's
//! own box rows are compared with CBBD's; rows, scores and games the gap-fill
//! wrote from CBBD are counted as `filled_by_cbbd` and never as agreement
//! (comparing CBBD with itself would read as perfect parity); each day's row
//! is replaced on a re-run, and a disagreement that has since resolved is
//! deleted rather than left behind.
//!
//! Isolation follows `gap_fill_replace.rs`: one pinned connection and TEMP
//! tables shadowing every real table touched, each asserted to resolve into
//! `pg_temp*` before anything runs. Skips when `DATABASE_URL` is unset.

use chrono::NaiveDate;
use cstat_ingest::source_parity::{self, DayCounts};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const BOX_COLS: &str = "points int, fgm int, fga int, tpm int, tpa int, ftm int, fta int, \
    off_rebounds int, def_rebounds int, total_rebounds int, assists int, steals int, \
    blocks int, turnovers int, fouls int";

fn ddl() -> Vec<String> {
    vec![
        "CREATE TEMP TABLE games (id uuid PRIMARY KEY, natstat_id text, game_date date, \
            home_team_id uuid, away_team_id uuid, home_score int, away_score int, \
            status text, score_source text)"
            .into(),
        "CREATE TEMP TABLE teams (id uuid PRIMARY KEY, name text)".into(),
        "CREATE TEMP TABLE players (id uuid PRIMARY KEY, name text)".into(),
        "CREATE TEMP TABLE game_broadcasts (game_id uuid, source_event_id text, \
            source_home_is_away boolean)"
            .into(),
        "CREATE TEMP TABLE cbbd_games (cbbd_game_id int PRIMARY KEY, espn_event_id text, \
            status text, home_points int, away_points int, first_box_at timestamptz, \
            start_date timestamptz, home_team text, away_team text)"
            .into(),
        "CREATE TEMP TABLE cbbd_game_play_summary (cbbd_game_id int, plays int, shots int, \
            shots_with_location int, plays_full_on_floor int)"
            .into(),
        format!(
            "CREATE TEMP TABLE team_game_stats (game_id uuid, team_id uuid, \
             source text NOT NULL DEFAULT 'natstat', created_at timestamp DEFAULT now(), \
             {BOX_COLS})"
        ),
        format!(
            "CREATE TEMP TABLE cbbd_team_game_stats (cbbd_game_id int, is_home boolean, {BOX_COLS})"
        ),
        "CREATE TEMP TABLE player_game_stats (player_id uuid, game_id uuid, team_id uuid, \
            starter boolean, minutes float8, points int, total_rebounds int, \
            source text NOT NULL DEFAULT 'natstat')"
            .into(),
        "CREATE TEMP TABLE cbbd_player_game_stats (cbbd_game_id int, is_home boolean, \
            name text, starter boolean, minutes int, points int, off_rebounds int, \
            def_rebounds int, total_rebounds int, turnovers int, fouls int)"
            .into(),
        "CREATE TEMP TABLE play_by_play (game_id uuid, source text NOT NULL DEFAULT 'natstat')"
            .into(),
        "CREATE TEMP TABLE source_parity_days (game_date date PRIMARY KEY, \
            computed_at timestamptz NOT NULL DEFAULT now(), run_id uuid, games int NOT NULL, \
            matched int NOT NULL, missing_in_cbbd int NOT NULL, compared int NOT NULL, \
            identical int NOT NULL, team_box_diff int NOT NULL, player_diff int NOT NULL, \
            score_compared int NOT NULL, score_disagreements int NOT NULL, \
            cbbd_only int NOT NULL, natstat_only int NOT NULL, filled_by_cbbd int NOT NULL, \
            cbbd_unmatched int NOT NULL)"
            .into(),
        "CREATE TEMP TABLE source_parity_games (game_id uuid PRIMARY KEY, \
            game_date date NOT NULL, computed_at timestamptz NOT NULL DEFAULT now(), \
            run_id uuid, matchup text NOT NULL, espn_event_id text, cbbd_game_id int, \
            our_score text, cbbd_score text, score_match boolean, \
            team_diffs text[] NOT NULL DEFAULT '{}', player_diffs text[] NOT NULL DEFAULT '{}')"
            .into(),
    ]
}

const SHADOWED: [&str; 13] = [
    "games",
    "teams",
    "players",
    "game_broadcasts",
    "cbbd_games",
    "cbbd_game_play_summary",
    "team_game_stats",
    "cbbd_team_game_stats",
    "player_game_stats",
    "cbbd_player_game_stats",
    "play_by_play",
    "source_parity_days",
    "source_parity_games",
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

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

/// Whose box rows a game side carries.
#[derive(Clone, Copy, PartialEq)]
enum Ours {
    NatStat,
    /// Written by the gap-fill from CBBD.
    Cbbd,
    None,
}

struct Game<'a> {
    date: NaiveDate,
    espn: &'a str,
    cbbd_id: i32,
    home: (i32, &'a str),
    away: (i32, &'a str),
    ours: Ours,
    /// CBBD has a box score.
    cbbd_box: bool,
    /// Our home fouls when they should differ from CBBD's 18.
    our_home_fouls: i32,
    /// CBBD's score when it differs from ours.
    cbbd_score: Option<(i32, i32)>,
    score_source: Option<&'a str>,
}

impl Default for Game<'_> {
    fn default() -> Self {
        Game {
            date: day(14),
            espn: "",
            cbbd_id: 0,
            home: (70, "Home"),
            away: (60, "Away"),
            ours: Ours::NatStat,
            cbbd_box: true,
            our_home_fouls: 18,
            cbbd_score: None,
            score_source: None,
        }
    }
}

/// Seed one game in both sources: one player a side, identical box lines
/// apart from what the [`Game`] says differs. Returns our game id.
async fn seed(pool: &PgPool, g: Game<'_>) -> Uuid {
    let id = Uuid::new_v4();
    let (home, away) = (Uuid::new_v4(), Uuid::new_v4());
    for (team, name) in [(home, g.home.1), (away, g.away.1)] {
        sqlx::query("INSERT INTO teams VALUES ($1, $2)")
            .bind(team)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO games VALUES ($1, $2, $3, $4, $5, $6, $7, 'Final', $8)")
        .bind(id)
        .bind(format!("ns-{}", g.espn))
        .bind(g.date)
        .bind(home)
        .bind(away)
        .bind(g.home.0)
        .bind(g.away.0)
        .bind(g.score_source)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO game_broadcasts VALUES ($1, $2, false)")
        .bind(id)
        .bind(g.espn)
        .execute(pool)
        .await
        .unwrap();
    let (ch, ca) = g.cbbd_score.unwrap_or((g.home.0, g.away.0));
    sqlx::query(
        "INSERT INTO cbbd_games VALUES ($1, $2, 'final', $3, $4, now(), \
         ($5::date + time '19:00') AT TIME ZONE 'America/New_York', $6, $7)",
    )
    .bind(g.cbbd_id)
    .bind(g.espn)
    .bind(ch)
    .bind(ca)
    .bind(g.date)
    .bind(g.home.1)
    .bind(g.away.1)
    .execute(pool)
    .await
    .unwrap();

    for (team, is_home, pts) in [(home, true, g.home.0), (away, false, g.away.0)] {
        let player = format!("{} Guard", if is_home { g.home.1 } else { g.away.1 });
        if g.cbbd_box {
            sqlx::query(
                "INSERT INTO cbbd_team_game_stats VALUES \
                 ($1, $2, $3, 25, 60, 5, 15, 10, 12, 99, 99, 99, 15, 5, 3, 99, 99)",
            )
            .bind(g.cbbd_id)
            .bind(is_home)
            .bind(pts)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO cbbd_player_game_stats VALUES \
                 ($1, $2, $3, true, 40, $4, 10, 20, 30, 12, 18)",
            )
            .bind(g.cbbd_id)
            .bind(is_home)
            .bind(&player)
            .bind(pts)
            .execute(pool)
            .await
            .unwrap();
        }
        let source = match g.ours {
            Ours::NatStat => "natstat",
            Ours::Cbbd => "cbbd",
            Ours::None => continue,
        };
        let fouls = if is_home { g.our_home_fouls } else { 18 };
        sqlx::query(
            "INSERT INTO team_game_stats (game_id, team_id, source, points, fgm, fga, tpm, \
             tpa, ftm, fta, off_rebounds, def_rebounds, total_rebounds, assists, steals, \
             blocks, turnovers, fouls) \
             VALUES ($1, $2, $3, $4, 25, 60, 5, 15, 10, 12, 10, 20, 30, 15, 5, 3, 12, $5)",
        )
        .bind(id)
        .bind(team)
        .bind(source)
        .bind(pts)
        .bind(fouls)
        .execute(pool)
        .await
        .unwrap();
        let pid = Uuid::new_v4();
        sqlx::query("INSERT INTO players VALUES ($1, $2)")
            .bind(pid)
            .bind(&player)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO player_game_stats VALUES ($1, $2, $3, true, 40.0, $4, 30, $5)")
            .bind(pid)
            .bind(id)
            .bind(team)
            .bind(pts)
            .bind(source)
            .execute(pool)
            .await
            .unwrap();
    }
    id
}

async fn stored_day(pool: &PgPool, d: NaiveDate) -> DayCounts {
    sqlx::query_as(
        "SELECT games, matched, missing_in_cbbd, compared, identical, team_box_diff, \
                player_diff, score_compared, score_disagreements, cbbd_only, natstat_only, \
                filled_by_cbbd, cbbd_unmatched \
         FROM source_parity_days WHERE game_date = $1",
    )
    .bind(d)
    .fetch_one(pool)
    .await
    .expect("stored day")
}

#[tokio::test]
async fn records_natstat_against_cbbd_and_leaves_the_gap_fill_out() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };

    // NatStat and CBBD agree on everything.
    seed(
        &pool,
        Game {
            espn: "1",
            cbbd_id: 1,
            home: (70, "Alpha"),
            away: (60, "Beta"),
            ..Default::default()
        },
    )
    .await;
    // NatStat's home fouls are 26, CBBD's player sum 18.
    let fouls = seed(
        &pool,
        Game {
            espn: "2",
            cbbd_id: 2,
            home: (81, "Gamma"),
            away: (75, "Delta"),
            our_home_fouls: 26,
            ..Default::default()
        },
    )
    .await;
    // Box rows the gap-fill wrote from CBBD: identical to CBBD by
    // construction, so they must not count as agreement.
    seed(
        &pool,
        Game {
            espn: "3",
            cbbd_id: 3,
            home: (66, "Eps"),
            away: (64, "Zeta"),
            ours: Ours::Cbbd,
            ..Default::default()
        },
    )
    .await;
    // A final score the gap-fill set from CBBD, no box on our side.
    seed(
        &pool,
        Game {
            espn: "4",
            cbbd_id: 4,
            home: (90, "Eta"),
            away: (50, "Theta"),
            ours: Ours::None,
            cbbd_box: false,
            score_source: Some("cbbd"),
            ..Default::default()
        },
    )
    .await;
    // The next day: the scores disagree, and neither source has a box.
    let score = seed(
        &pool,
        Game {
            date: day(15),
            espn: "5",
            cbbd_id: 5,
            home: (80, "Iota"),
            away: (70, "Kappa"),
            ours: Ours::None,
            cbbd_box: false,
            cbbd_score: Some((80, 71)),
            ..Default::default()
        },
    )
    .await;

    let report = source_parity::run(&pool, day(14), day(16)).await.unwrap();
    let written = source_parity::record(&pool, &report, None).await.unwrap();
    assert_eq!(written, 2, "the fouls game and the score game");

    assert_eq!(
        stored_day(&pool, day(14)).await,
        DayCounts {
            games: 4,
            matched: 4,
            missing_in_cbbd: 0,
            compared: 2,
            identical: 1,
            team_box_diff: 1,
            player_diff: 0,
            // Game 4's score is CBBD's own, so it is not compared.
            score_compared: 3,
            score_disagreements: 0,
            cbbd_only: 1,
            natstat_only: 0,
            filled_by_cbbd: 2,
            cbbd_unmatched: 0,
        }
    );
    let d15 = stored_day(&pool, day(15)).await;
    assert_eq!((d15.games, d15.compared), (1, 0));
    assert_eq!((d15.score_compared, d15.score_disagreements), (1, 1));
    // A day with no games still gets its row.
    assert_eq!(stored_day(&pool, day(16)).await, DayCounts::default());

    // game_id, score_match, team_diffs, our_score, cbbd_score
    type StoredGame = (
        Uuid,
        Option<bool>,
        Vec<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<StoredGame> = sqlx::query_as(
        "SELECT game_id, score_match, team_diffs, our_score, cbbd_score \
         FROM source_parity_games ORDER BY game_date",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                fouls,
                Some(true),
                vec!["home.fouls 26/18".to_string()],
                Some("81-75".into()),
                Some("81-75".into())
            ),
            (
                score,
                Some(false),
                Vec::new(),
                Some("80-70".into()),
                Some("80-71".into())
            ),
        ]
    );

    let notes = report.ledger_notes();
    assert!(
        notes.contains("Gamma vs Delta (ESPN 2): home.fouls 26/18"),
        "{notes}"
    );
    assert!(
        notes.contains("Iota vs Kappa (ESPN 5): score 80-70/80-71"),
        "{notes}"
    );

    // NatStat corrects the fouls: a re-run replaces the day and drops the
    // resolved disagreement.
    sqlx::query("UPDATE team_game_stats SET fouls = 18 WHERE game_id = $1")
        .bind(fouls)
        .execute(&pool)
        .await
        .unwrap();
    let report = source_parity::run(&pool, day(14), day(16)).await.unwrap();
    assert_eq!(
        source_parity::record(&pool, &report, None).await.unwrap(),
        1
    );
    let d14 = stored_day(&pool, day(14)).await;
    assert_eq!((d14.compared, d14.identical, d14.team_box_diff), (2, 2, 0));
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT game_id FROM source_parity_games")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, vec![score]);
    let days: i64 = sqlx::query_scalar("SELECT count(*) FROM source_parity_days")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(days, 3);

    let trend = source_parity::trend(&pool, 30).await.unwrap();
    assert!(trend.contains("2026-11-14..2026-11-16 (3 days)"), "{trend}");
    assert!(
        trend.contains("Iota vs Kappa (ESPN 5): score 80-70/80-71"),
        "{trend}"
    );
}

#[tokio::test]
async fn nightly_step_records_its_window_and_a_ledger_row() {
    let Some(pool) = isolated_pool().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    sqlx::query(
        "CREATE TEMP TABLE ingest_runs (id bigserial, run_id uuid, season int, step text, \
         status text, rows_touched bigint, started_at timestamptz, ended_at timestamptz, \
         error text, window_start date, window_end date, notes text)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let schema: String = sqlx::query_scalar(
        "SELECT n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = to_regclass('ingest_runs')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        schema.starts_with("pg_temp"),
        "ingest_runs resolved to {schema}"
    );

    let ledger = cstat_ingest::run_ledger::RunLedger::start(&pool, 2027);
    let line = source_parity::nightly_step(&pool, &ledger)
        .await
        .expect("not skipped on the real clock");
    assert!(line.starts_with("*Parity ("), "{line}");
    assert!(line.contains("0/0 identical"), "{line}");

    let (status, notes, run_id): (String, Option<String>, Uuid) = sqlx::query_as(
        "SELECT status, notes, run_id FROM ingest_runs WHERE step = 'source_parity'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "ok");
    assert!(notes.unwrap_or_default().contains(": 0 games"));
    assert_eq!(run_id, ledger.run_id());
    let (days, stamped): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE run_id = $1) FROM source_parity_days",
    )
    .bind(ledger.run_id())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (days, stamped),
        (source_parity::PARITY_DAYS, source_parity::PARITY_DAYS)
    );
}
