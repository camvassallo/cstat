//! Team strength as of a cutoff, checked against what it must agree with.
//!
//! At the end of a finished season the point-in-time solve has seen every game,
//! so it must land on the stored season ratings and the stored Elo. Before that
//! it must see nothing later than the cutoff.
//!
//! Gated `#[ignore]`: needs a database with a played season and `team_elo_games`
//! loaded. `PIT_DUMP=path` also writes every cutoff in the season as CSV, to
//! compare with the training frame.
//!   DATABASE_URL=... cargo test -p cstat-core --test pit_team_parity -- --ignored --nocapture

use chrono::NaiveDate;
use cstat_core::pit_team::compute_pit_teams;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::io::Write;
use uuid::Uuid;

/// Team, adjusted offense, adjusted defense, Elo.
type StoredTeam = (Uuid, Option<f64>, Option<f64>, Option<f64>);

fn season() -> i32 {
    std::env::var("PARITY_SEASON")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2025)
}

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect")
}

async fn game_dates(pool: &PgPool, season: i32) -> Vec<NaiveDate> {
    sqlx::query_scalar(
        "SELECT DISTINCT game_date FROM team_game_stats WHERE season = $1 ORDER BY game_date",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .expect("dates")
}

#[tokio::test]
#[ignore]
async fn the_last_cutoff_is_the_stored_season() {
    let pool = pool().await;
    let season = season();
    let last = *game_dates(&pool, season).await.last().expect("a season");
    let pit = compute_pit_teams(&pool, season, last).await.expect("solve");

    let stored: Vec<StoredTeam> = sqlx::query_as(
        "SELECT team_id, adj_offense::float8, adj_defense::float8, elo_rating::float8
           FROM team_season_stats WHERE season = $1 AND adj_offense IS NOT NULL",
    )
    .bind(season)
    .fetch_all(&pool)
    .await
    .expect("stored");
    assert!(stored.len() > 300, "only {} teams stored", stored.len());

    for (team, off, def, elo) in stored {
        let r = pit.ratings.get(&team).expect("a stored team is solved");
        // The stored columns are rounded to one decimal.
        assert!(
            (r.adj_offense - off.unwrap()).abs() < 0.06,
            "{team} offense"
        );
        assert!(
            (r.adj_defense - def.unwrap()).abs() < 0.06,
            "{team} defense"
        );
        if let (Some(a), Some(b)) = (pit.elo.get(&team), elo) {
            assert_eq!(*a, b, "{team} Elo");
        }
    }
}

#[tokio::test]
#[ignore]
async fn a_cutoff_sees_nothing_after_it() {
    let pool = pool().await;
    let season = season();
    let dates = game_dates(&pool, season).await;
    let cutoff = dates[dates.len() / 2];
    let pit = compute_pit_teams(&pool, season, cutoff)
        .await
        .expect("solve");

    let played: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT team_id FROM team_game_stats
          WHERE season = $1 AND game_date <= $2 AND opponent_id IS NOT NULL",
    )
    .bind(season)
    .bind(cutoff)
    .fetch_all(&pool)
    .await
    .expect("played");
    assert_eq!(pit.ratings.len(), played.len());

    // Each team's Elo at the cutoff is its rating before its next game.
    let next: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT DISTINCT ON (t.id) t.id, e.elo_after - e.elo_change
           FROM team_elo_games e
           JOIN teams t ON t.natstat_id = e.team_code AND t.season = e.season
          WHERE e.season = $1 AND e.game_date > $2
          ORDER BY t.id, e.game_date, e.season_games",
    )
    .bind(season)
    .bind(cutoff)
    .fetch_all(&pool)
    .await
    .expect("next games");
    assert!(next.len() > 200);
    let matched = next
        .iter()
        .filter(|(team, before)| pit.elo.get(team).is_some_and(|e| (e - before).abs() <= 1.0))
        .count();
    assert!(
        matched as f64 / next.len() as f64 > 0.98,
        "{matched} of {} teams carried their rating to the next game",
        next.len()
    );
}

#[tokio::test]
#[ignore]
async fn a_cutoff_is_bit_identical_across_runs() {
    let pool = pool().await;
    let season = season();
    let dates = game_dates(&pool, season).await;
    let cutoff = dates[dates.len() / 2];
    let first = compute_pit_teams(&pool, season, cutoff)
        .await
        .expect("solve");
    for _ in 0..3 {
        let again = compute_pit_teams(&pool, season, cutoff)
            .await
            .expect("solve");
        assert_eq!(first.league_avg.to_bits(), again.league_avg.to_bits());
        for (team, r) in &first.ratings {
            let a = again.ratings[team];
            assert_eq!(r.adj_offense.to_bits(), a.adj_offense.to_bits());
            assert_eq!(r.adj_defense.to_bits(), a.adj_defense.to_bits());
            assert_eq!(r.sos.map(f64::to_bits), a.sos.map(f64::to_bits));
        }
    }
}

#[tokio::test]
#[ignore]
async fn dump_every_cutoff() {
    let Ok(path) = std::env::var("PIT_DUMP") else {
        return;
    };
    let pool = pool().await;
    let season = season();
    let mut out = std::fs::File::create(path).expect("dump file");
    writeln!(out, "cutoff,team_id,adj_offense,adj_defense,sos,elo").unwrap();
    for cutoff in game_dates(&pool, season).await {
        let pit = compute_pit_teams(&pool, season, cutoff)
            .await
            .expect("solve");
        for (team, r) in &pit.ratings {
            let elo = pit.elo.get(team).map(f64::to_string).unwrap_or_default();
            let sos = r.sos.map(|s| s.to_string()).unwrap_or_default();
            writeln!(
                out,
                "{cutoff},{team},{},{},{sos},{elo}",
                r.adj_offense, r.adj_defense
            )
            .unwrap();
        }
    }
}
