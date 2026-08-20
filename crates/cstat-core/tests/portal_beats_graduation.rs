//! Invariant: an observed portal move outranks the inferred senior graduation.
//!
//! The departure classifier has one inferred channel — `class_year == 'Sr'` ⇒
//! `GraduatedSenior` — and three observed ones (curated `player_departures`,
//! the 247 portal feed, the NBA early-entrant list). Because the inference is
//! only ever a guess about eligibility, it must lose to any feed that actually
//! reports where the player went.
//!
//! Ordering it the other way was harmless while "senior" reliably meant "out of
//! eligibility": a senior in the portal was a rare grad transfer. The NCAA's
//! age-based **5-in-5** model (adopted 2026-06-23, effective season 2027) ends
//! that — 53 of the 56 players who entered the 2026 portal after June 1 and
//! resolve to a cstat player were `Sr`-labelled in 2026 (issue #220). Under the
//! old ordering every one of them renders as "Sr graduation" on his former
//! team, with no destination chip and no link, while simultaneously showing up
//! in another team's `arrivals`.
//!
//! The roster math is identical either way — the player departs, and
//! `departures_cam_v3_sum` is not one of the 27 roster-impact features — so
//! what this guards is the label and the destination link.
//!
//! `LeftProgram` legitimately outranks a portal row and is accepted here: a
//! player who committed in the portal and *then* signed professionally is
//! correctly labelled by where he actually went (see `curated_departures.rs`).
//!
//! Gated `#[ignore]` — needs a local DB with rosters + the portal class loaded
//! (`cstat-ingest transfers --year 2026`) and the ONNX model dir present. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test portal_beats_graduation -- --ignored --nocapture

use std::collections::HashSet;
use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_projection::{
    DepartureReason, compose_all_projections, fetch_draft_entrants, fetch_player_departures,
};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// Base season whose portal class feeds the projection. `transfers.year` is
/// keyed to the same number (`roster_projection` reads `WHERE year = $1`).
const BASE_SEASON: i32 = 2026;

#[tokio::test]
#[ignore = "needs local DB with transfers loaded + MODEL_DIR"]
async fn portal_departure_outranks_senior_graduation() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../training/models"));
    let predictor = Predictor::load(&model_dir).expect("load models");

    // Every player the portal feed says moved out of a program this cycle.
    // `status <> 'Withdrawn'` mirrors the projection's own transfer query
    // (`roster_projection.rs`): a withdrawal entered the portal and pulled
    // back out, so he is still on his source roster and *should* fall through
    // to the senior branch. Without this filter the gate flags him as a
    // mislabel — 2026 has 25 such rows.
    let outbound: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT cstat_player_id FROM transfers \
         WHERE year = $1 AND cstat_player_id IS NOT NULL AND status <> 'Withdrawn'",
    )
    .bind(BASE_SEASON)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    assert!(
        !outbound.is_empty(),
        "no resolved transfers for {BASE_SEASON} — run `cstat-ingest transfers --year {BASE_SEASON}` first"
    );

    let entrants = fetch_draft_entrants(&pool, BASE_SEASON).await.unwrap();
    let captured = fetch_player_departures(&pool, BASE_SEASON).await.unwrap();
    // `false` = don't retro-exclude redshirt recruits; irrelevant to this gate.
    let projections =
        compose_all_projections(&pool, BASE_SEASON, &entrants, &captured, &predictor, false)
            .await
            .unwrap();

    let mut mislabelled: Vec<(String, String)> = Vec::new();
    let mut checked = 0usize;
    for p in &projections {
        for dep in &p.departures {
            if !outbound.contains(&dep.player_id()) {
                continue;
            }
            checked += 1;
            match dep {
                // The two acceptable labels: the portal row itself, or a
                // curated exit that supersedes it.
                DepartureReason::Transferred { .. } | DepartureReason::LeftProgram { .. } => {}
                DepartureReason::GraduatedSenior { name, .. } => {
                    mislabelled.push((name.clone(), p.team_name.clone()));
                }
                // A firm draft departure is also an observation, and it is
                // checked after the senior branch by design — a player in both
                // the portal and the draft list is a real ambiguity, not this
                // bug. Not asserted either way.
                DepartureReason::DraftGone { .. } => {}
            }
        }
    }

    assert!(
        checked > 0,
        "no resolved portal player appeared in any team's departures — \
         the join is broken, not the ordering"
    );
    assert!(
        mislabelled.is_empty(),
        "{} portal departure(s) labelled `GraduatedSenior` instead of `Transferred` \
         (senior check is running ahead of the portal check): {:?}",
        mislabelled.len(),
        mislabelled
    );
    eprintln!(
        "checked {checked} portal departures across {} teams",
        projections.len()
    );
}
