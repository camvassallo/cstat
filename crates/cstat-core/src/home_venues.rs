//! Curated home venues — the teams whose real home court the upstream feed
//! flags as neutral.
//!
//! Companion to the neutral-site derivation in
//! [`crate::compute::compute_derived_game_fields`], which promotes a game to
//! neutral when it is played somewhere other than the host's own building.
//! That rule reads a team's homes off the last played season, counting only
//! venues where the source did *not* already say neutral — so a venue the
//! source is simply wrong about never enters the baseline, and the rule
//! confidently promotes games there. This is the override for that case.
//!
//! `include_str!`-compiled, like `data/conference_realignment.json` and
//! `data/team_short_names.json`, so editing it needs a **redeploy, not a
//! sync**.

use std::collections::HashMap;
use std::sync::OnceLock;

const HOME_VENUES_JSON: &str = include_str!("../../../data/home_venues.json");

static MAP: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

/// `(short_name, venue_code)` pairs that count as home, sorted for a stable
/// order so anything built from them (a SQL `VALUES` list, a test) is
/// deterministic.
pub fn pairs() -> Vec<(&'static str, &'static str)> {
    let map = MAP.get_or_init(|| {
        let parsed: HashMap<String, serde_json::Value> =
            serde_json::from_str(HOME_VENUES_JSON).expect("data/home_venues.json must be valid");
        parsed
            .into_iter()
            // `_comment` carries the file's own documentation and is an
            // array of strings like the entries, so it is skipped by name.
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| {
                let codes = v
                    .as_array()
                    .expect("each entry is an array of venue codes")
                    .iter()
                    .map(|c| {
                        c.as_str()
                            .expect("venue codes are strings, matching games.venue_code")
                            .to_string()
                    })
                    .collect();
                (k, codes)
            })
            .collect()
    });
    let mut out: Vec<(&'static str, &'static str)> = map
        .iter()
        .flat_map(|(team, codes)| codes.iter().map(move |c| (team.as_str(), c.as_str())))
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_parses_and_carries_the_known_override() {
        let p = pairs();
        assert!(
            p.contains(&("St. John's", "384")),
            "Madison Square Garden is St. John's home court and the feed says otherwise: {p:?}"
        );
        // The documentation block must never be read as a team.
        assert!(!p.iter().any(|(t, _)| t.starts_with('_')), "{p:?}");
    }
}
