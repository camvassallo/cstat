//! Invariants for the two curated captures added for the 2027 preseason board:
//! season-long absences (issue #217) and eligibility returns that name a
//! `destination`.
//!
//! Absences. Every `player_absences` row must:
//!   1. resolve — show up as an `OutForSeason` entry on the team it names
//!      (an unmatched row is a silent no-op: the injured player keeps scoring);
//!   2. be complete — the player is in that team's scored roster under
//!      NEITHER scenario. Absences are firm, so the ceiling must not carry him
//!      either, and an absent recruit must not survive in `recruits`.
//!
//! Destinations. Every `player_returns` row with a `destination` must leave the
//! player off his base team's scored roster and put him on the destination's
//! (as an arrival when `granted`, in its uncertain bucket when `contested`) —
//! unless 247 recorded a different committed destination, which wins.
//!
//! Gated `#[ignore]` — needs a local DB with rosters and both captures loaded
//! (`cstat-ingest returns`, `cstat-ingest absences`) and the ONNX model dir.
//! Run:
//!   DATABASE_URL=... MODEL_DIR=$PWD/training/models \
//!     cargo test -p cstat-core --test curated_absences -- --ignored --nocapture

use std::collections::HashMap;
use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_projection::TeamRow;
use cstat_core::roster_projection::{
    DepartureReason, DraftScenario, ProjectedRoster, ReturnStatus, compose_all_projections,
    fetch_draft_entrants, fetch_player_absences, fetch_player_departures, fetch_player_returns,
    normalize_player_name, resolve_team_id,
};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const BASE_SEASON: i32 = 2026;

async fn setup() -> (sqlx::PgPool, Vec<ProjectedRoster>, Vec<TeamRow>) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../training/models"));
    let predictor = Predictor::load(&model_dir).expect("load models");
    let entrants = fetch_draft_entrants(&pool, BASE_SEASON).await.unwrap();
    let departures = fetch_player_departures(&pool, BASE_SEASON).await.unwrap();
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
    let teams = cstat_core::roster_projection::fetch_team_rows(&pool, BASE_SEASON)
        .await
        .unwrap();
    (pool, projections, teams)
}

/// Player ids on a team's scored roster under either scenario.
fn scored_ids(p: &ProjectedRoster) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = p
        .for_scenario(DraftScenario::Ceiling)
        .iter()
        .map(|r| r.player_id)
        .collect();
    ids.extend(
        p.for_scenario(DraftScenario::Floor)
            .iter()
            .map(|r| r.player_id),
    );
    ids
}

#[tokio::test]
#[ignore = "needs local DB with player_absences loaded + MODEL_DIR"]
async fn absences_remove_their_player_from_the_scored_roster() {
    let (pool, projections, teams) = setup().await;
    let absences = fetch_player_absences(&pool, BASE_SEASON).await.unwrap();
    if absences.is_empty() {
        eprintln!("no player_absences rows for {BASE_SEASON}; nothing to check");
        return;
    }
    let by_team: HashMap<Uuid, &ProjectedRoster> =
        projections.iter().map(|p| (p.team_id, p)).collect();

    let mut failures: Vec<String> = Vec::new();
    for a in &absences {
        let Some(team_id) = resolve_team_id(&teams, &a.current_team) else {
            failures.push(format!(
                "{} — team {:?} does not resolve",
                a.name, a.current_team
            ));
            continue;
        };
        let Some(p) = by_team.get(&team_id) else {
            failures.push(format!("{} — {} has no projection", a.name, a.current_team));
            continue;
        };
        let key = normalize_player_name(&a.name);
        let hit = p.departures.iter().find_map(|d| match d {
            DepartureReason::OutForSeason {
                player_id, name, ..
            } if normalize_player_name(name) == key => Some(*player_id),
            _ => None,
        });
        let Some(pid) = hit else {
            failures.push(format!(
                "{} ({}) — not listed out for the season: the row matched nobody",
                a.name, a.current_team
            ));
            continue;
        };
        if scored_ids(p).contains(&pid) {
            failures.push(format!(
                "{} ({}) — still on the scored roster",
                a.name, a.current_team
            ));
        }
        if p.recruits
            .iter()
            .any(|(_, m)| normalize_player_name(&m.name) == key)
        {
            failures.push(format!(
                "{} ({}) — still listed as a recruit",
                a.name, a.current_team
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "absence rows misapplied:\n  {}",
        failures.join("\n  ")
    );
}

#[tokio::test]
#[ignore = "needs local DB with player_returns loaded + MODEL_DIR"]
async fn returns_with_a_destination_move_the_player() {
    let (pool, projections, teams) = setup().await;
    let returns = fetch_player_returns(&pool, BASE_SEASON).await.unwrap();
    let movers: Vec<_> = returns
        .iter()
        .filter(|r| {
            r.destination
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty())
        })
        .collect();
    if movers.is_empty() {
        eprintln!("no player_returns rows with a destination for {BASE_SEASON}");
        return;
    }
    let by_team: HashMap<Uuid, &ProjectedRoster> =
        projections.iter().map(|p| (p.team_id, p)).collect();

    let mut failures: Vec<String> = Vec::new();
    for r in movers {
        let dest = r.destination.as_deref().unwrap();
        let (Some(base_id), Some(dest_id)) = (
            resolve_team_id(&teams, &r.current_team),
            resolve_team_id(&teams, dest),
        ) else {
            failures.push(format!("{} — a team string does not resolve", r.name));
            continue;
        };
        let key = normalize_player_name(&r.name);
        let Some(base) = by_team.get(&base_id) else {
            failures.push(format!("{} — {} has no projection", r.name, r.current_team));
            continue;
        };
        // He must leave the base team as a portal-style move...
        let moved = base.departures.iter().find_map(|d| match d {
            DepartureReason::Transferred {
                player_id,
                name,
                destination_team_id,
                ..
            } if normalize_player_name(name) == key => Some((*player_id, *destination_team_id)),
            _ => None,
        });
        let Some((pid, landed)) = moved else {
            failures.push(format!(
                "{} — not a Transferred departure from {}",
                r.name, r.current_team
            ));
            continue;
        };
        // ...to the row's destination, unless 247 recorded another commit.
        if landed != Some(dest_id) {
            eprintln!(
                "{} — 247 commit overrides the curated destination {dest:?}",
                r.name
            );
            continue;
        }
        let Some(d) = by_team.get(&dest_id) else {
            failures.push(format!("{} — destination {dest} has no projection", r.name));
            continue;
        };
        let placed = match r.parsed_status() {
            ReturnStatus::Granted => d.arrivals.iter().any(|a| a.player_id == pid),
            ReturnStatus::Contested => d.uncertain.iter().any(|(row, _)| row.player_id == pid),
        };
        if !placed {
            failures.push(format!("{} — not placed at {dest}", r.name));
        }
        if scored_ids(base).contains(&pid) {
            failures.push(format!("{} — still scored at {}", r.name, r.current_team));
        }
    }
    assert!(
        failures.is_empty(),
        "destination rows misapplied:\n  {}",
        failures.join("\n  ")
    );
}
