//! Invariant: a player who withdrew from the transfer portal stays on their
//! source team's returning core.
//!
//! `transfers` keeps `Withdrawn` rows (see `ALLOWED_STATUS` in the ingest) —
//! they are players who entered the portal and then pulled out. Most of them
//! are back on the roster they started on. `compose_all_projections` used to
//! read `FROM transfers WHERE year = $1` with no status filter, so every one
//! of them was pushed into `outbound_by_team` and subtracted from the
//! returning core as though they had transferred away. In 2026 that silently
//! erased 25 players, including NC State's Paul McNeil (8.9 cam_v3).
//!
//! The handful who withdrew because they went pro are removed by the separate
//! `draft_entrants` path (`firm_draft_gone`). That path is only *reachable*
//! once the outbound branch stops short-circuiting them first — before the
//! fix, Allen Graves left Santa Clara labelled `transferred` with a null
//! destination instead of `draft_gone`. So the two halves of this invariant
//! are coupled: withdrawn-and-staying must return, withdrawn-to-the-NBA must
//! still depart, and for the right reason.
//!
//! Gated `#[ignore]` — needs a local DB with transfers + rosters ingested and
//! the ONNX model dir present. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test withdrawn_transfers_return -- --ignored --nocapture

use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_projection::{compose_all_projections, fetch_draft_entrants};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

/// Season whose portal cycle carries the withdrawn rows. 2026 is the first
/// year we refreshed live mid-cycle rather than bootstrapping from a
/// post-cycle snapshot, so it's the only season with a meaningful count.
const BASE_SEASON: i32 = 2026;

#[tokio::test]
#[ignore = "needs local DB with 2026 transfers ingested + MODEL_DIR"]
async fn withdrawn_players_stay_on_their_team() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../training/models"));
    let predictor = Predictor::load(&model_dir).expect("load models");

    // Withdrawn rows that resolve to a real player. These are the ones that
    // reach the outbound path at all — an unresolved `cstat_player_id` is
    // skipped upstream.
    let withdrawn = sqlx::query(
        r#"
        SELECT t.cstat_player_id AS player_id,
               t.full_name,
               t.source_institution,
               EXISTS (
                   SELECT 1 FROM draft_entrants d
                   WHERE d.year = t.year
                     AND d.player_name = t.full_name
                     AND d.status = 'gone'
               ) AS draft_gone
        FROM transfers t
        WHERE t.year = $1
          AND t.status = 'Withdrawn'
          AND t.cstat_player_id IS NOT NULL
        "#,
    )
    .bind(BASE_SEASON)
    .fetch_all(&pool)
    .await
    .unwrap();

    if withdrawn.is_empty() {
        // Nothing to assert — a season bootstrapped from a post-cycle
        // snapshot legitimately has no withdrawals.
        eprintln!("no resolved Withdrawn rows for {BASE_SEASON}; nothing to check");
        return;
    }

    let entrants = fetch_draft_entrants(&pool, BASE_SEASON).await.unwrap();
    let projections = compose_all_projections(&pool, BASE_SEASON, &entrants, &predictor)
        .await
        .unwrap();

    let mut violations: Vec<String> = Vec::new();
    let mut returned = 0usize;
    for row in &withdrawn {
        let pid: uuid::Uuid = row.get("player_id");
        let name: String = row.get("full_name");
        let source: Option<String> = row.get("source_institution");
        let draft_gone: bool = row.get("draft_gone");

        // Where did this player land across every team's projection?
        let departed_from = projections
            .iter()
            .find(|p| p.departures.iter().any(|d| d.player_id() == pid));
        let returned_to = projections
            .iter()
            .find(|p| p.returning.iter().any(|r| r.player_id == pid));

        let src = source.unwrap_or_else(|| "?".into());
        if draft_gone {
            // Went pro: must still be gone, and labelled as a draft
            // departure rather than a phantom transfer to nowhere.
            match departed_from {
                None => violations.push(format!(
                    "{name} ({src}) withdrew to the NBA but is not a departure anywhere"
                )),
                Some(p) => {
                    let reason = p
                        .departures
                        .iter()
                        .find(|d| d.player_id() == pid)
                        .map(|d| format!("{d:?}"))
                        .unwrap_or_default();
                    if !reason.contains("DraftGone") {
                        violations.push(format!(
                            "{name} ({src}) is an NBA departure but labelled {reason}"
                        ));
                    }
                }
            }
        } else {
            // Withdrew and stayed: must not be counted as a departure.
            //
            // Absent from BOTH lists is fine and not asserted — the roster
            // query gates on QUAL_MIN_GAMES_PLAYED / QUAL_MIN_MPG (5 GP,
            // 5 MPG), so a deep-bench player who withdrew is legitimately
            // outside the projection entirely (2026: Gai Chol, no season
            // row; Isaiah Denis, 9 GP at 3.1 MPG). Only the false-departure
            // direction is a bug.
            if departed_from.is_some() {
                violations.push(format!(
                    "{name} ({src}) withdrew from the portal but is counted as a departure"
                ));
            } else if returned_to.is_some() {
                returned += 1;
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{} withdrawn-player violation(s):\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
    eprintln!(
        "{} withdrawn row(s) checked; {returned} back in a returning core",
        withdrawn.len()
    );
}
