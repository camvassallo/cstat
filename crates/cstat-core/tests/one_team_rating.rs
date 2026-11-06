//! A team has one rating on the site. The rankings board, the team page and
//! the Schedule tab all read `queries::team_ratings_cte`; this holds them to
//! the same rating and rank, for a season in progress (teams that have not
//! played included), a finished one, and one computed before the blend existed.
//!
//! Gated `#[ignore]`: needs a local DB with `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test one_team_rating -- --ignored --nocapture

use std::collections::HashMap;

use cstat_core::projection::{self, PreseasonOnlyRegime, Venue};
use cstat_core::queries::{self, TeamSortField};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn every_surface_reads_the_same_rating_and_rank() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let seasons: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT season FROM games ORDER BY season")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(!seasons.is_empty(), "no seasons to check");

    for season in seasons {
        let board =
            queries::get_team_rankings(&pool, season, TeamSortField::AdjEfficiencyMargin, None)
                .await
                .unwrap();
        let schedule: HashMap<Uuid, _> = queries::get_team_rating_inputs(&pool, season)
            .await
            .unwrap()
            .into_iter()
            .map(|t| (t.team_id, t))
            .collect();

        for (i, row) in board.iter().enumerate() {
            assert_eq!(
                row.rank,
                i as i64 + 1,
                "{season}: board rank is its position"
            );
            let inputs = &schedule[&row.team_id];
            assert_eq!(
                inputs.adj_em, row.adj_efficiency_margin,
                "{season} {}",
                row.name
            );
            assert_eq!(inputs.adj_em_rank, Some(row.rank), "{season} {}", row.name);
        }

        // One query per team, so a sample: the top, the middle and the bottom.
        let sample = [0, board.len() / 2, board.len().saturating_sub(1)];
        for row in sample.iter().filter_map(|&i| board.get(i)) {
            let team = queries::get_team_by_id(&pool, row.team_id, season)
                .await
                .unwrap()
                .expect("team on the board");
            assert_eq!(team.adj_efficiency_margin, row.adj_efficiency_margin);
            assert_eq!(team.adj_efficiency_margin_rank, Some(row.rank));
            assert_eq!(team.adj_offense, row.adj_offense);
            assert_eq!(team.adj_defense, row.adj_defense);
            assert_eq!(team.sos, row.sos);
        }
    }
}

/// The Predict page lists the board, so a team that has not played has to be
/// predictable against one that has. The model cannot serve that matchup.
#[tokio::test]
#[ignore]
async fn a_matchup_with_an_unplayed_team_gets_the_preseason_projection() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let season: i32 = sqlx::query_scalar("SELECT max(season) FROM games")
        .fetch_one(&pool)
        .await
        .unwrap();
    let inputs = queries::get_team_rating_inputs(&pool, season)
        .await
        .unwrap();
    let rated = |played: bool| {
        inputs
            .iter()
            .find(|t| t.has_played == played && t.adj_em.is_some())
    };
    let (Some(played), Some(unplayed)) = (rated(true), rated(false)) else {
        return; // Past the opening weeks everyone has played.
    };
    let regime = projection::preseason_only_regime(
        &pool,
        season,
        played.team_id,
        unplayed.team_id,
        Venue::Neutral,
    )
    .await;
    assert!(
        matches!(regime, PreseasonOnlyRegime::Serve(_)),
        "{regime:?}"
    );
}
