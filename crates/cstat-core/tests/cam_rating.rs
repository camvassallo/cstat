//! The CAM shown while a season is played is display only, and a finished
//! season is exactly the stat. The projection models train on
//! `cam_gbpm_v3_psos`, so a rating that reached a finished season would change
//! what a reader sees without changing what the models learned from.
//!
//! Gated `#[ignore]`: needs a local DB with `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test cam_rating -- --ignored --nocapture

use cstat_core::compute::{CAM_PRIOR_GAMES, FULL_SEASON_GAMES};
use cstat_core::queries::{self, OPENING_MEDIAN_GAMES, PlayerSortField};
use sqlx::postgres::PgPoolOptions;

/// The first two weeks of a season used to list nobody: a five-game floor no
/// team could clear yet. Skips quietly when no season is opening.
#[tokio::test]
#[ignore]
async fn an_opening_season_lists_players_on_the_rating() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let season: i32 = sqlx::query_scalar("SELECT MAX(season) FROM team_game_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    let median = queries::median_team_games(&pool, season).await.unwrap();
    if median.is_none_or(|m| m >= OPENING_MEDIAN_GAMES) {
        return;
    }

    let (rows, total) = queries::search_players(
        &pool,
        None,
        None,
        season,
        PlayerSortField::Campom,
        None,
        None,
        false,
        50,
        0,
    )
    .await
    .unwrap();
    assert!(total > 0, "{season}: an opening season listed nobody");
    assert_eq!(rows.len(), 50);

    let cams: Vec<f64> = rows.iter().filter_map(|r| r.campom).collect();
    assert!(
        cams.windows(2).all(|w| w[0] >= w[1]),
        "not sorted on the rating"
    );
    // The stat alone tops out near 5 after a game, so this is the rating.
    assert!(cams[0] > 10.0, "top of the board is {}", cams[0]);
    assert!(rows.iter().any(|r| r.campom_prior_weight.is_some()));
}

#[tokio::test]
#[ignore]
async fn a_finished_season_has_no_rating_and_a_rating_is_well_formed() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let finished_with_rating: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM torvik_player_stats t
          WHERE t.cam_rating IS NOT NULL
            AND NOT EXISTS (
                SELECT 1 FROM games g
                 WHERE g.season = t.season AND g.status IS DISTINCT FROM 'Final'
            )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        finished_with_rating, 0,
        "a finished season carries a rating"
    );

    let full_season_with_rating: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM torvik_player_stats
          WHERE cam_rating IS NOT NULL AND games_played >= $1",
    )
    .bind(FULL_SEASON_GAMES as i32)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        full_season_with_rating, 0,
        "a full season should read the stat"
    );

    // The same rule for teams: a finished season's blend is its raw rating.
    let finished_team_blended: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_season_stats t
          WHERE t.blended_adj_efficiency_margin IS NOT NULL
            AND (t.blended_adj_efficiency_margin IS DISTINCT FROM t.adj_efficiency_margin
                 OR t.preseason_weight IS NOT NULL)
            AND NOT EXISTS (
                SELECT 1 FROM games g
                 WHERE g.season = t.season AND g.status IS DISTINCT FROM 'Final'
            )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        finished_team_blended, 0,
        "a finished season carries a team prior"
    );

    let halves_off: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM torvik_player_stats
          WHERE cam_rating IS NOT NULL
            AND abs(cam_rating - cam_rating_o - cam_rating_d) > 1e-6",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(halves_off, 0, "CAMO + CAMD must equal CAM");

    let bad_weight: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM torvik_player_stats
          WHERE cam_rating IS NOT NULL
            AND NOT (cam_rating_prior_w > 0 AND cam_rating_prior_w < 1)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bad_weight, 0, "the prior weight left (0, 1)");

    // The weight is the documented curve, not just inside its bounds.
    let end = CAM_PRIOR_GAMES / (CAM_PRIOR_GAMES + FULL_SEASON_GAMES);
    let off_curve: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM torvik_player_stats
          WHERE cam_rating IS NOT NULL
            AND abs(cam_rating_prior_w
                    - (($1 / ($1 + games_played)) - $2) / (1.0 - $2)) > 1e-9",
    )
    .bind(CAM_PRIOR_GAMES)
    .bind(end)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(off_curve, 0, "the prior weight is off its curve");
}
