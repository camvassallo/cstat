//! Invariant: every `player_departures` row actually removes its player.
//!
//! The curated capture (issue #215) is hand-entered from news reports, and its
//! join to a roster player is a fuzzy `(normalized name, resolved team)` match.
//! That combination has one nasty failure mode: a typo in the name or team
//! string produces a row that *looks* correct in the table and in the JSON
//! capture while doing absolutely nothing — the player stays on the projected
//! roster and the team stays over-projected, which is the exact bug the table
//! was built to fix.
//!
//! Two things are asserted per capture row:
//!
//!   1. It resolves — the player shows up as a `LeftProgram` departure on some
//!      team. (An unmatched row is the silent no-op above.)
//!   2. It is complete — the same player appears in nobody's `returning` and
//!      nobody's `arrivals`. The arrivals half matters for a player who
//!      committed in the portal *before* signing professionally: he must not be
//!      carried onto the destination roster he never joined.
//!
//! Gated `#[ignore]` — needs a local DB with rosters + the capture loaded
//! (`cstat-ingest departures`) and the ONNX model dir present. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test curated_departures -- --ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use cstat_core::inference::Predictor;
use cstat_core::roster_features::{QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use cstat_core::roster_projection::{
    DepartureReason, compose_all_projections, fetch_draft_entrants, fetch_player_departures,
    normalize_player_name,
};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// Base season carrying the capture. 2026 is the first year with curated rows
/// (Mario Saint-Supery → Valencia).
const BASE_SEASON: i32 = 2026;

#[tokio::test]
#[ignore = "needs local DB with player_departures loaded + MODEL_DIR"]
async fn curated_departures_remove_their_player() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../training/models"));
    let predictor = Predictor::load(&model_dir).expect("load models");

    let captured = fetch_player_departures(&pool, BASE_SEASON).await.unwrap();
    if captured.is_empty() {
        eprintln!("no player_departures rows for {BASE_SEASON}; nothing to check");
        return;
    }
    let entrants = fetch_draft_entrants(&pool, BASE_SEASON).await.unwrap();

    // `false` = don't retro-exclude redshirt recruits; irrelevant to this gate.
    let projections =
        compose_all_projections(&pool, BASE_SEASON, &entrants, &captured, &predictor, false)
            .await
            .unwrap();

    // Which capture rows resolved, counted per normalized name. Counting rather
    // than set-membership because cstat genuinely carries same-name players in
    // one season (issue #138 — two Jake Davises in 2026): two capture rows
    // sharing a name must consume two resolved departures, or a set would call
    // the unresolved one matched and hide exactly the bug this guards.
    let mut resolved: HashMap<String, Vec<Uuid>> = HashMap::new();
    for dep in projections.iter().flat_map(|p| p.departures.iter()) {
        if let DepartureReason::LeftProgram {
            player_id, name, ..
        } = dep
        {
            resolved
                .entry(normalize_player_name(name))
                .or_default()
                .push(*player_id);
        }
    }

    // A capture row that didn't resolve is only benign in one specific case:
    // the name IS a real base-season player who sits below the projection's
    // roster gate. Sub-gate players never enter `compose_all_projections`, so
    // there was nothing to remove. Every other miss is a mistake — an unknown
    // name is a typo, and a known-and-qualified name that still didn't resolve
    // means the team string is wrong. Keying the benign test on the name alone
    // would be backwards: a misspelling matches nothing and would be excused as
    // "sub-gate", which is exactly the bug this guard exists to catch.
    // `bool_or` + GROUP BY rather than a bare LEFT JOIN: `player_season_stats`
    // is unique on `(player_id, team_id, season)`, so a player with rows on two
    // teams in one season would otherwise fan the join out.
    let roster: Vec<(String, bool)> = sqlx::query_as::<_, (String, bool)>(
        r#"
        SELECT p.name,
               COALESCE(bool_or(pss.games_played >= $2 AND pss.minutes_per_game >= $3), false)
        FROM players p
        LEFT JOIN player_season_stats pss
               ON pss.player_id = p.id AND pss.season = p.season
        WHERE p.season = $1
        GROUP BY p.id, p.name
        "#,
    )
    .bind(BASE_SEASON)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(&pool)
    .await
    .unwrap();
    let known: HashSet<String> = roster
        .iter()
        .map(|(n, _)| normalize_player_name(n))
        .collect();
    let qualified: HashSet<String> = roster
        .iter()
        .filter(|(_, q)| *q)
        .map(|(n, _)| normalize_player_name(n))
        .collect();

    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for d in &captured {
        let key = normalize_player_name(&d.name);

        // 1. Resolved to a LeftProgram departure somewhere?
        let Some(pid) = resolved.get_mut(&key).and_then(|pids| pids.pop()) else {
            if !known.contains(&key) {
                violations.push(format!(
                    "{} ({}) is in player_departures but no {BASE_SEASON} player has that \
                     name — the row is a silent no-op; check the spelling",
                    d.name, d.current_team,
                ));
            } else if qualified.contains(&key) {
                violations.push(format!(
                    "{} ({}) is in player_departures and is a qualified {BASE_SEASON} player, \
                     but the row resolved to nobody — check the team string",
                    d.name, d.current_team,
                ));
            }
            // Known but sub-gate: correctly a no-op, nothing to assert.
            continue;
        };
        checked += 1;

        // 2. Gone from every returning core and every arrivals list.
        if let Some(p) = projections
            .iter()
            .find(|p| p.returning.iter().any(|r| r.player_id == pid))
        {
            violations.push(format!(
                "{} is a curated departure but still returning on team {}",
                d.name, p.team_id,
            ));
        }
        if let Some(p) = projections
            .iter()
            .find(|p| p.arrivals.iter().any(|a| a.player_id == pid))
        {
            violations.push(format!(
                "{} is a curated departure but still an arrival on team {} — \
                 a portal commit who then signed pro must not land at the destination",
                d.name, p.team_id,
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "{} curated-departure violation(s) for base {BASE_SEASON}:\n  {}",
        violations.len(),
        violations.join("\n  "),
    );
    eprintln!(
        "{checked} of {} curated departure(s) for {BASE_SEASON} resolved and removed \
         ({} sub-gate / unknown, correctly no-ops)",
        captured.len(),
        captured.len() - checked,
    );
}
