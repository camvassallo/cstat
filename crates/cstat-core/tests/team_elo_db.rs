//! The in-house Elo against a real database: refresh `team_game_elo`, then
//! check the stored ratings chain and a second refresh writes nothing.
//!
//! Gated `#[ignore]`: needs a database with games and migration 080 applied.
//!   DATABASE_URL=... cargo test -p cstat-core --test team_elo_db -- --ignored --nocapture

use chrono::NaiveDate;
use cstat_core::team_elo::{rating_entering, refresh_team_elo};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type Row = (String, i32, NaiveDate, Uuid, f64, f64);

#[tokio::test]
#[ignore]
async fn stored_ratings_chain_and_refresh_is_idempotent() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect");

    let first = refresh_team_elo(&pool).await.expect("refresh");
    println!("{first}");
    let again = refresh_team_elo(&pool).await.expect("refresh again");
    assert_eq!(again.written, 0, "a second refresh rewrote rows");
    assert_eq!(again.deleted, 0, "a second refresh deleted rows");

    // Every rating going into a game is the team's rating out of its previous
    // game, carried over when that game was in an earlier season: nothing is
    // read from the game itself or from a later one.
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT team_key, season, game_date, game_id, elo_before, elo_after
           FROM team_game_elo ORDER BY team_key, game_date, game_id",
    )
    .fetch_all(&pool)
    .await
    .expect("rows");
    assert_eq!(rows.len(), again.rows);
    let mut chained = 0;
    for w in rows.windows(2) {
        let (prev, cur) = (&w[0], &w[1]);
        if prev.0 != cur.0 {
            assert_eq!(cur.4, 1500.0, "{} starts at {}", cur.0, cur.4);
            continue;
        }
        let want = rating_entering(prev.5, prev.1, cur.1);
        assert!(
            (cur.4 - want).abs() < 1e-9,
            "{} {}: before {} but the previous game left {want}",
            cur.0,
            cur.2,
            cur.4
        );
        chained += 1;
    }
    println!("{chained} chained ratings checked");

    // Every finished game with a Division I side has its rows.
    let missing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM games g
          WHERE g.status = 'Final' AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
            AND ((g.home_team_id IS NOT NULL AND NOT EXISTS (
                    SELECT 1 FROM team_game_elo e WHERE e.game_id = g.id AND e.team_id = g.home_team_id))
              OR (g.away_team_id IS NOT NULL AND NOT EXISTS (
                    SELECT 1 FROM team_game_elo e WHERE e.game_id = g.id AND e.team_id = g.away_team_id)))",
    )
    .fetch_one(&pool)
    .await
    .expect("coverage");
    assert_eq!(missing, 0);
}
