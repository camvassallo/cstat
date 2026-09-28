//! Every entry in `TEAM_ALIASES` must name exactly ONE program, in every
//! ingested season, against the real `teams` rows (#403).
//!
//! The Ole Miss bug was not a missing alias. `("ole miss", "mississippi")` was
//! present and correct-looking, but it was compared with the scorer's PREFIX
//! branch, so it matched Mississippi Rebels, Mississippi State Bulldogs and
//! Mississippi Valley State Delta Devils all at the same score. The caller takes
//! the best score and breaks ties arbitrarily; it took Valley State. That put
//! all eight of Ole Miss's 2026-27 transfer commitments in the SWAC, dropped Ole
//! Miss below the roster-thinness gate, and left an SEC program with no 2027
//! projection — with nothing anywhere reporting a problem, because a full roster
//! on the wrong team looks exactly like a full roster.
//!
//! `team_name_match::tests::no_alias_matches_two_programs` guards the same
//! invariant against a hand-written fixture and runs in CI. This one is the
//! exhaustive half: it sees every program cstat actually carries, including ones
//! added after the fixture was written, and it is the version to run when adding
//! an alias or ingesting a new season.
//!
//! Gated `#[ignore]` — needs a local DB. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test team_alias_ambiguity -- --ignored --nocapture

use cstat_core::team_name_match::{TEAM_ALIASES, team_match_score};
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "needs local DB"]
async fn no_alias_resolves_to_two_programs_in_any_season() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let rows: Vec<(i32, Option<String>, String)> =
        sqlx::query_as("SELECT season, short_name, name FROM teams ORDER BY season, name")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(!rows.is_empty(), "no teams — this check would be vacuous");

    let mut keys: Vec<&str> = TEAM_ALIASES.iter().map(|(k, _)| *k).collect();
    keys.sort_unstable();
    keys.dedup();

    let seasons: Vec<i32> = {
        let mut s: Vec<i32> = rows.iter().map(|(y, _, _)| *y).collect();
        s.dedup();
        s
    };
    println!(
        "checking {} alias keys against {} teams across {} seasons",
        keys.len(),
        rows.len(),
        seasons.len()
    );

    let mut violations = Vec::new();
    for season in &seasons {
        for key in &keys {
            // Ambiguity is a TIE at the best score, not merely more than one
            // match: callers take the minimum, so a worse-scoring also-ran is
            // fine and common (a bare-prefix hit losing to an exact one).
            let mut scored: Vec<(u32, &str)> = rows
                .iter()
                .filter(|(y, _, _)| y == season)
                .filter_map(|(_, short, full)| {
                    team_match_score(short.as_deref(), full, key).map(|s| (s, full.as_str()))
                })
                .collect();
            if scored.is_empty() {
                continue;
            }
            scored.sort_unstable();
            let best = scored[0].0;
            let tied: Vec<&str> = scored
                .iter()
                .filter(|(s, _)| *s == best)
                .map(|(_, f)| *f)
                .collect();
            if tied.len() > 1 {
                eprintln!("  {season}: {key:?} ties at score {best} across {tied:?}");
                violations.push((*season, *key, best, tied.len()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{} ambiguous alias resolutions — each one silently assigns players to \
         whichever program the tiebreak happens to pick. Target the exact full \
         name instead of a prefix (see the Ole Miss and Penn entries): {violations:?}",
        violations.len(),
    );
}
