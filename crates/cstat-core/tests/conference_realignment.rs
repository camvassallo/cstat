//! Invariants for the curated conference-realignment capture
//! (`data/conference_realignment.json`).
//!
//! The capture is hand-entered from news reports and joined to a team by
//! `teams.short_name`, which gives it the same failure mode as the curated
//! departures table: a typo produces an entry that reads perfectly in the JSON
//! and does absolutely nothing, leaving the team labelled with last season's
//! league on the one page whose whole job is to describe next season.
//!
//! The first three tests are pure and run in CI. The last one needs a local DB
//! and is `#[ignore]`-gated:
//!   DATABASE_URL=... cargo test -p cstat-core --test conference_realignment -- --ignored --nocapture

use std::collections::HashSet;

use cstat_core::compute::TORVIK_CONF_TO_CSTAT;
use cstat_core::realignment;

/// Base season the 2027 capture is a diff against.
const BASE_SEASON: i32 = 2026;
const TARGET_SEASON: i32 = 2027;

#[test]
fn every_destination_is_a_conference_the_ingest_can_also_produce() {
    // A target conference the Torvik correction has no code for is a label that
    // exists only on the Future page: the moment the season is ingested the
    // team's conference would flip to something else (or to null). Tying the
    // curated destinations to `TORVIK_CONF_TO_CSTAT` keeps the pre-season view
    // and the played-season view speaking one vocabulary — which is exactly how
    // "UAC" would otherwise have been missed.
    let known: HashSet<&str> = TORVIK_CONF_TO_CSTAT.iter().map(|(_, c)| *c).collect();
    for (season, r) in realignment::all() {
        for m in &r.moves {
            assert!(
                known.contains(m.to.as_str()),
                "{season}: {} moves to unknown conference code {:?}",
                m.team,
                m.to,
            );
            assert!(
                known.contains(m.from.as_str()),
                "{season}: {} moves from unknown conference code {:?}",
                m.team,
                m.from,
            );
        }
    }
}

#[test]
fn no_team_is_captured_twice() {
    for (season, r) in realignment::all() {
        let mut seen = HashSet::new();
        for team in r
            .moves
            .iter()
            .map(|m| &m.team)
            .chain(r.left_division_i.iter().map(|d| &d.team))
        {
            assert!(
                seen.insert(team.clone()),
                "{season}: {team} appears more than once — the two entries \
                 would resolve by file order, which is not a decision anyone made",
            );
        }
    }
}

#[test]
fn a_move_actually_changes_something() {
    // from == to is either a typo or a no-op; both should be caught here rather
    // than shipping as a "moved" badge on a team that didn't move. (A league
    // *rebrand* does change the code — WAC -> UAC — so it passes this.)
    for (season, r) in realignment::all() {
        for m in &r.moves {
            assert_ne!(
                m.from, m.to,
                "{season}: {} 'moves' to its own conference",
                m.team
            );
        }
    }
}

#[tokio::test]
#[ignore = "needs a local DB with the base season ingested"]
async fn every_entry_matches_a_real_team_and_its_base_conference() {
    use sqlx::postgres::PgPoolOptions;

    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let base: std::collections::HashMap<String, Option<String>> =
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT short_name, conference FROM teams WHERE season = $1",
        )
        .bind(BASE_SEASON)
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        !base.is_empty(),
        "no season-{BASE_SEASON} teams to check against"
    );

    let r = realignment::for_season(TARGET_SEASON).expect("2027 capture");
    let mut problems: Vec<String> = Vec::new();
    for (team, from) in r
        .moves
        .iter()
        .map(|m| (&m.team, &m.from))
        .chain(r.left_division_i.iter().map(|d| (&d.team, &d.from)))
    {
        match base.get(team) {
            // The entry names a team that doesn't exist in the base season —
            // usually a `short_name` spelling that differs from ours. It would
            // silently never fire.
            None => problems.push(format!(
                "{team}: no season-{BASE_SEASON} team with that short_name"
            )),
            Some(actual) if actual.as_deref() != Some(from.as_str()) => problems.push(format!(
                "{team}: capture says it leaves {from}, DB says {actual:?} — entry is stale and will no-op",
            )),
            Some(_) => {}
        }
    }
    assert!(
        problems.is_empty(),
        "curated realignment is out of sync:\n  {}",
        problems.join("\n  ")
    );
}
