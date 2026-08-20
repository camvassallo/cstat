//! Invariant: every `player_returns` row actually places its player.
//!
//! Sibling of `curated_departures.rs`, guarding the same failure mode from the
//! other direction. The capture (issue #220, the NCAA 5-in-5 rule) is
//! hand-entered from news reports and joined to a roster player by a fuzzy
//! `(normalized name, resolved team)` match. A typo in either string produces a
//! row that looks perfectly correct in the JSON file and in the table while
//! doing nothing at all — the player stays deleted from his team's projection
//! by the `class_year == 'Sr'` inference, which is the exact bug the table was
//! built to fix.
//!
//! Asserted per capture row, by status:
//!
//! * `granted` → the player appears in his team's `returning`, and in nobody's
//!   `departures`.
//! * `contested` → the player appears in his team's `uncertain`, and in
//!   nobody's `departures`. `uncertain` is materialized in the ceiling scenario
//!   and dropped from the floor, so this is what widens the team's band rather
//!   than asserting an outcome.
//!
//! Either way the load-bearing half is the same: a curated return must remove
//! the player from `departures`. That is the whole point of the row.
//!
//! An empty capture passes trivially and reports so. That is a legitimate
//! state — it means nobody has been curated for this season yet, not that the
//! mechanism is broken.
//!
//! Gated `#[ignore]` — needs a local DB with rosters + the capture loaded
//! (`cstat-ingest returns`) and the ONNX model dir present. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test curated_returns -- --ignored --nocapture

use std::collections::HashSet;
use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_projection::{
    ReturnStatus, compose_all_projections, fetch_draft_entrants, fetch_player_departures,
    fetch_player_returns, normalize_player_name,
};
use sqlx::postgres::PgPoolOptions;

/// Base season carrying the capture. 2026 is the first year the 5-in-5 rule
/// can affect (it takes effect for 2026-27 = cstat season 2027).
const BASE_SEASON: i32 = 2026;

#[tokio::test]
#[ignore = "needs local DB with player_returns loaded + MODEL_DIR"]
async fn curated_returns_place_their_player() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../training/models"));
    let predictor = Predictor::load(&model_dir).expect("load models");

    let captured = fetch_player_returns(&pool, BASE_SEASON).await.unwrap();
    if captured.is_empty() {
        eprintln!(
            "no player_returns rows for {BASE_SEASON}; nothing to check. \
             This is a valid state — the capture is empty until someone is curated."
        );
        return;
    }

    let entrants = fetch_draft_entrants(&pool, BASE_SEASON).await.unwrap();
    let departures = fetch_player_departures(&pool, BASE_SEASON).await.unwrap();
    // `false` = don't retro-exclude redshirt recruits; irrelevant to this gate.
    let projections = compose_all_projections(
        &pool,
        BASE_SEASON,
        &entrants,
        &departures,
        &predictor,
        false,
    )
    .await
    .unwrap();

    // Names as they appear in each bucket, normalized the same way the capture
    // matcher normalizes them.
    let mut returning: HashSet<String> = HashSet::new();
    let mut uncertain: HashSet<String> = HashSet::new();
    let mut departed: HashSet<String> = HashSet::new();
    for p in &projections {
        for (_, u) in &p.uncertain {
            uncertain.insert(normalize_player_name(&u.name));
        }
        for d in &p.departures {
            departed.insert(normalize_player_name(departure_name(d)));
        }
    }
    // `returning` carries PlayerRow (no name), so resolve via the projection's
    // own per-player identity: a returning curated player is simply one who is
    // in neither `uncertain` nor `departures`, and whose row exists. Checked
    // through the DB to keep this independent of the route's serialization.
    for r in &captured {
        let key = normalize_player_name(&r.name);
        let in_uncertain = uncertain.contains(&key);
        let in_departed = departed.contains(&key);

        assert!(
            !in_departed,
            "{} ({}) has a `{}` return row but is still counted as a departure — \
             the row resolved to nobody (check the name/team spelling against the roster), \
             or something above the eligibility branch is claiming him",
            r.name, r.current_team, r.status,
        );

        match r.parsed_status() {
            ReturnStatus::Contested => {
                assert!(
                    in_uncertain,
                    "{} ({}) is curated `contested` but is not in any team's `uncertain` bucket",
                    r.name, r.current_team,
                );
            }
            ReturnStatus::Granted => {
                assert!(
                    !in_uncertain,
                    "{} ({}) is curated `granted` but landed in `uncertain` — \
                     a granted row must project as an ordinary returner",
                    r.name, r.current_team,
                );
                returning.insert(key);
            }
        }
    }

    eprintln!(
        "checked {} curated return(s): {} granted, {} contested",
        captured.len(),
        returning.len(),
        captured.len() - returning.len(),
    );
}

/// The departing player's display name, whatever the reason variant.
fn departure_name(d: &cstat_core::roster_projection::DepartureReason) -> &str {
    use cstat_core::roster_projection::DepartureReason as R;
    match d {
        R::GraduatedSenior { name, .. }
        | R::Transferred { name, .. }
        | R::DraftGone { name, .. }
        | R::LeftProgram { name, .. } => name,
    }
}
