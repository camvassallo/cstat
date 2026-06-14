//! Invariant for `compute::reconcile_player_teams` (issue #119).
//!
//! The box-score path (`games.rs`) sets `players.team_id` first-write-wins, so a
//! single source-swapped game — NatStat occasionally tags a game's two rosters
//! onto each other's team, e.g. the 2018 Champions Classic that put Zion
//! Williamson on Kentucky — permanently mis-teams every player whose first
//! appearance was that game. `reconcile_player_teams` (compute step 2/16) makes
//! `team_id` derived: each player's team is the MODE of their `player_game_stats`
//! teams. The invariant that must then hold for every player with box games:
//!
//!   players.team_id == argmax_team count(player_game_stats rows)
//!
//! A break means either the reconcile step didn't run for that season, or the
//! first-write-wins poisoning reappeared.
//!
//! Gated `#[ignore]` — needs a local DB with `compute` run for every season. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test player_team_reconcile -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn player_team_matches_box_score_majority() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Players whose stored team_id disagrees with their box-score modal team.
    // Restricted to players who actually have box games (team_id IS NOT NULL),
    // since a roster-only player has no box majority to reconcile against.
    let rows = sqlx::query(
        r#"
        WITH modal AS (
            SELECT player_id, season, team_id,
                   ROW_NUMBER() OVER (
                       PARTITION BY player_id, season
                       ORDER BY COUNT(*) DESC, team_id
                   ) AS rn
            FROM player_game_stats
            WHERE team_id IS NOT NULL
            GROUP BY player_id, season, team_id
        )
        SELECT p.season, p.name, ct.name AS stored_team, mt.name AS box_majority_team
        FROM modal m
        JOIN players p ON p.id = m.player_id AND p.season = m.season
        JOIN teams ct ON ct.id = p.team_id
        JOIN teams mt ON mt.id = m.team_id
        WHERE m.rn = 1 AND p.team_id IS DISTINCT FROM m.team_id
        ORDER BY p.season, p.name
        "#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    if !rows.is_empty() {
        for r in &rows {
            let season: i32 = r.get("season");
            let name: String = r.get("name");
            let stored: String = r.get("stored_team");
            let majority: String = r.get("box_majority_team");
            eprintln!("  {season} {name}: stored={stored} but box-majority={majority}");
        }
    }
    assert_eq!(
        rows.len(),
        0,
        "{} player(s) have team_id != box-score majority — reconcile_player_teams \
         did not run for their season (or first-write-wins poisoning returned)",
        rows.len()
    );
}
