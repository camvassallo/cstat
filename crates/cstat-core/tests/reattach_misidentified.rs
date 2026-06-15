//! Invariant for `compute::reattach_misidentified_players` — the reconciliation
//! that moves box rows NatStat stamped with the WRONG same-name player's natstat
//! id onto the real human (issue #138 — two "Jake Davis" in 2026, one at Illinois
//! and one at Cal Poly, where Cal Poly Davis's box lines arrived stamped with
//! Illinois Davis's id and produced a spurious 2-GP season row on his progression
//! page).
//!
//! Asserts the post-compute invariant: no `player_game_stats` row remains
//! reattachable — i.e. there is no row sitting on a team that is NOT its owner's
//! reconciled majority team while a single DISTINCT same-name player genuinely
//! rosters to that team (and could take the row without tripping the
//! `(player_id, game_id)` unique index). Genuine mid-season transfers are exempt
//! by construction: a transferring human keeps one natstat_id, so their
//! foreign-team rows have no same-name sibling on that team.
//!
//! Gated `#[ignore]` — needs a local DB with box scores ingested + `compute` run
//! (so the reattach step has executed) for some season.
//!   DATABASE_URL=... cargo test -p cstat-core --test reattach_misidentified -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB with box scores + compute run for some season"]
async fn no_misidentified_box_rows_remain() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // Every row the reattach step would act on: on a foreign team, with exactly
    // one same-name sibling rostered there, and no clashing row for that game.
    let leftover: i64 = sqlx::query(
        r#"
        WITH sibling AS (
            SELECT pgs.id AS pgs_id, pgs.game_id,
                   (array_agg(b.id))[1] AS correct_player_id
            FROM player_game_stats pgs
            JOIN players a ON a.id = pgs.player_id
            JOIN players b ON b.name = a.name
                          AND b.season = pgs.season
                          AND b.id <> a.id
                          AND b.team_id = pgs.team_id
            WHERE a.team_id IS NOT NULL
              AND pgs.team_id IS DISTINCT FROM a.team_id
            GROUP BY pgs.id, pgs.game_id
            HAVING COUNT(DISTINCT b.id) = 1
        )
        SELECT count(*)
        FROM sibling s
        WHERE NOT EXISTS (
            SELECT 1 FROM player_game_stats x
            WHERE x.player_id = s.correct_player_id AND x.game_id = s.game_id
        )
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get::<i64, _>(0);

    assert_eq!(
        leftover, 0,
        "{leftover} box rows are still filed under the wrong same-name player \
         (reattach_misidentified_players did not run, or ran before this data landed)"
    );
}
