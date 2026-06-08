//! Reconciliation invariants for the `player_on_off` derivation (PBP item "A").
//!
//! On/off is `team-total − on` over the same validity-clamped game-lineups the
//! served aggregates read, restricted to games the player appeared in. That
//! gives a handful of invariants that must hold for every row, independent of
//! the (replay-approximate) lineup quality:
//!   * OFF can never be negative — a player's ON slice can't exceed his team's
//!     total, so `team − on >= 0` for points, possessions, and minutes.
//!   * `net_on_off == on_net_rtg − off_net_rtg` (when both sides have rates).
//!   * a row only exists for a player with a real on-court sample (possessions).
//!
//! A break in any of these means the team-total / unnest join drifted.
//!
//! Gated `#[ignore]` — needs a local DB with PBP loaded and `compute` run. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test player_on_off -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB with PBP loaded + compute run"]
async fn on_off_splits_reconcile() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let total: i64 = sqlx::query("SELECT count(*) FROM player_on_off")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert!(
        total > 0,
        "no player_on_off rows — run `compute` for a PBP season first"
    );

    // (1) No OFF component can be negative — team-total minus the player's ON
    // slice must stay non-negative for every accumulator.
    let neg_off: i64 = sqlx::query(
        "SELECT count(*) FROM player_on_off \
         WHERE off_points_for < 0 OR off_points_against < 0 \
            OR off_possessions_for < -1e-6 OR off_possessions_against < -1e-6 \
            OR off_minutes < -1e-6",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        neg_off, 0,
        "{neg_off} rows have a negative OFF component (team−on went negative)"
    );

    // (2) net_on_off is exactly the on/off net swing wherever both sides have a
    // rate (a 1e-6 tolerance for the float round-trip through Postgres).
    let bad_net: i64 = sqlx::query(
        "SELECT count(*) FROM player_on_off \
         WHERE on_net_rtg IS NOT NULL AND off_net_rtg IS NOT NULL \
           AND abs(net_on_off - (on_net_rtg - off_net_rtg)) > 1e-6",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        bad_net, 0,
        "{bad_net} rows where net_on_off != on_net_rtg - off_net_rtg"
    );

    // (3) Every row has a real on-court sample. Gated on possessions, NOT
    // minutes — clock-parse gaps can zero `on_minutes` for a player who
    // genuinely played (see the methodology's clock-vintage note), so minutes
    // is not a valid existence signal; possessions is.
    let no_on: i64 = sqlx::query(
        "SELECT count(*) FROM player_on_off \
         WHERE (on_possessions_for + on_possessions_against) <= 0 OR games <= 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        no_on, 0,
        "{no_on} rows with no on-court possessions / games (shouldn't exist)"
    );

    eprintln!("player_on_off: {total} rows, all invariants hold");
}
