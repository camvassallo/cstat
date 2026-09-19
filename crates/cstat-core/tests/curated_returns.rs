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
//! One carve-out, because the row's status follows the player through the
//! portal: a player who committed to ANOTHER D-I team is legitimately a
//! `Transferred` departure on the row's team, and the bucket assertion runs
//! against his destination instead — `contested` must be in its `uncertain`
//! (not its firm `arrivals`), `granted` in its `arrivals`. A `Transferred`
//! departure with no resolved destination is still a miss: nothing could have
//! placed him anywhere.
//!
//! An empty capture passes trivially and reports so. That is a legitimate
//! state — it means nobody has been curated for this season yet, not that the
//! mechanism is broken.
//!
//! Gated `#[ignore]` — needs a local DB with rosters + the capture loaded
//! (`cstat-ingest returns`) and the ONNX model dir present. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test curated_returns -- --ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_features::{QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use cstat_core::roster_projection::{
    DepartureReason, ReturnStatus, compose_all_projections, fetch_draft_entrants,
    fetch_player_departures, fetch_player_returns, normalize_player_name,
};
use cstat_core::team_name_match::team_match_score;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

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
    // matcher normalizes them — and keyed by TEAM, not by name alone. cstat
    // carries genuine same-name players across different teams in one season
    // (two Josh Reeds in 2026, Drexel and Penn St.), and with name-only sets
    // the sibling departing anywhere in D-I put the name in `departed` and
    // failed a perfectly good row for the other one. The row's team is
    // resolved with the same scorer the projection uses, so a team string the
    // projection accepts is never rejected here. `arrivals` carries PlayerRow
    // (no name), so it is keyed by player id instead.
    let mut returning: HashSet<String> = HashSet::new();
    let mut uncertain: HashMap<Uuid, HashSet<String>> = HashMap::new();
    let mut arrivals: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
    let mut departed: HashSet<(Uuid, String)> = HashSet::new();
    // (team, name) → the destination team of a `Transferred` departure with a
    // resolved D-I destination (the only kind of departure a curated row may
    // coexist with), and the departing player's id.
    let mut moved: HashMap<(Uuid, String), (Uuid, Uuid)> = HashMap::new();
    for p in &projections {
        for (_, u) in &p.uncertain {
            uncertain
                .entry(p.team_id)
                .or_default()
                .insert(normalize_player_name(&u.name));
        }
        for a in &p.arrivals {
            arrivals.entry(p.team_id).or_default().insert(a.player_id);
        }
        for d in &p.departures {
            let key = (p.team_id, normalize_player_name(departure_name(d)));
            if let DepartureReason::Transferred {
                player_id,
                destination_team_id: Some(dest),
                ..
            } = d
            {
                moved.insert(key.clone(), (*dest, *player_id));
            }
            departed.insert(key);
        }
    }
    let resolve_team = |want: &str| -> Option<Uuid> {
        projections
            .iter()
            .filter_map(|p| {
                team_match_score(Some(&p.team_name), &p.team_full_name, want)
                    .map(|score| (score, p.team_id))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, id)| id)
    };
    // `returning` carries PlayerRow (no name), so resolve via the projection's
    // own per-player identity: a returning curated player is simply one who is
    // in neither `uncertain` nor `departures`, and whose row exists. Checked
    // through the DB to keep this independent of the route's serialization.
    // Players below the projection's qualification gate were never on the
    // roster being projected, so a row naming one has nothing to place and
    // nothing to fail. Same tolerance `departures-audit` applies (it reports
    // them as benign); asserting on them here would reject a harmless row.
    let qualified: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT p.name FROM players p \
         JOIN player_season_stats pss ON pss.player_id = p.id AND pss.season = p.season \
         WHERE p.season = $1 AND pss.games_played >= $2 AND pss.minutes_per_game >= $3",
    )
    .bind(BASE_SEASON)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|n| normalize_player_name(&n))
    .collect();

    let mut below_gate = 0usize;
    for r in &captured {
        let key = normalize_player_name(&r.name);
        if !qualified.contains(&key) {
            below_gate += 1;
            continue;
        }
        let Some(team) = resolve_team(&r.current_team) else {
            panic!(
                "{} ({}): no projected team matches that team string — check it against \
                 teams.short_name",
                r.name, r.current_team
            );
        };
        let in_uncertain = uncertain.get(&team).is_some_and(|s| s.contains(&key));
        let in_departed = departed.contains(&(team, key.clone()));

        if in_departed {
            let Some((dest, pid)) = moved.get(&(team, key.clone())) else {
                panic!(
                    "{} ({}) has a `{}` return row but is still counted as a departure — \
                     the row resolved to nobody (check the name/team spelling against the roster), \
                     or something above the eligibility branch is claiming him",
                    r.name, r.current_team, r.status,
                );
            };
            let at_dest_uncertain = uncertain.get(dest).is_some_and(|s| s.contains(&key));
            let at_dest_arrival = arrivals.get(dest).is_some_and(|s| s.contains(pid));
            match r.parsed_status() {
                ReturnStatus::Contested => assert!(
                    at_dest_uncertain && !at_dest_arrival,
                    "{} ({}) is curated `contested` and moved in the portal, but is not in his \
                     destination's `uncertain` bucket (uncertain={at_dest_uncertain}, \
                     arrival={at_dest_arrival}) — a contested mover must be a `?` at his new \
                     school, not a firm arrival",
                    r.name,
                    r.current_team,
                ),
                ReturnStatus::Granted => {
                    assert!(
                        at_dest_arrival && !at_dest_uncertain,
                        "{} ({}) is curated `granted` and moved in the portal, but is not an \
                         ordinary arrival at his destination",
                        r.name,
                        r.current_team,
                    );
                    returning.insert(key);
                }
            }
            continue;
        }

        match r.parsed_status() {
            ReturnStatus::Contested => {
                assert!(
                    in_uncertain,
                    "{} ({}) is curated `contested` but is not in that team's `uncertain` bucket",
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
        "checked {} curated return(s): {} granted, {} contested, {} below the roster gate (skipped)",
        captured.len() - below_gate,
        returning.len(),
        captured.len() - below_gate - returning.len(),
        below_gate,
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
