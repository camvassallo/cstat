//! Reconciliation invariants for the `player_on_off` derivation (PBP item "A").
//!
//! On/off is `team-total − on` over ALL reconstructed stints (any size, NOT the
//! 5-man-clamped lineup set the top-lineup aggregates use), with each player's
//! per-game ON capped at his box minutes, restricted to games he appeared in.
//! That gives a handful of invariants that must hold for every row, independent
//! of the (replay-approximate) lineup quality and the per-player scaling:
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

    // (3) Every row has a real on-court sample on BOTH ends, so both rates have
    // a positive denominator. Gated on possessions, NOT minutes — clock-parse
    // gaps can zero `on_minutes` for a player who genuinely played (see the
    // methodology's clock-vintage note), so minutes is not a valid existence
    // signal; possessions is. The derivation's `on_posf >= 100 AND on_posa >= 100`
    // minimum-ON-sample gate keeps only real rotation players (on/off is noise
    // below ~100 on-court possessions), so this `<= 0` check has wide margin.
    let no_on: i64 = sqlx::query(
        "SELECT count(*) FROM player_on_off \
         WHERE on_possessions_for <= 0 OR on_possessions_against <= 0 OR games <= 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        no_on, 0,
        "{no_on} rows with a non-positive on-court possession side / games (shouldn't exist)"
    );

    // (4) Every row is credited to the player's OWN team. The replay/onfloor
    // resolution can leak a player's UUID into another team's lineup arrays
    // (same-name collision); the derivation drops those by keying on the
    // box-score-authoritative `players.team_id`, so no row may sit on a foreign
    // team — and that keeps (season, player_id) unique (migration 035's index).
    let wrong_team: i64 = sqlx::query(
        "SELECT count(*) FROM player_on_off oo \
         JOIN players p ON p.id = oo.player_id AND p.season = oo.season \
         WHERE p.team_id IS DISTINCT FROM oo.team_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        wrong_team, 0,
        "{wrong_team} rows credited to a team other than the player's canonical team"
    );

    eprintln!("player_on_off: {total} rows, all invariants hold");
}
