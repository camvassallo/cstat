//! Curated list of games the feed publishes that should not be on the site.
//!
//! NatStat occasionally lists a game that is not part of a team's season, such
//! as Christian Brothers at Memphis on 2026-11-05. Nothing in the payload marks
//! one: its shape matches a real game against a non-D1 opponent, and those stay,
//! because they count toward the displayed W-L. Being missing from ESPN does not
//! separate them either, since real non-D1 games are missing there too. So the
//! list is by hand, keyed on the NatStat game id.
//!
//! `parse_game` in `cstat-ingest` refuses these ids, which covers every feed
//! path, and the schedule refresh deletes a row already stored.
//!
//! The file is `include_str!`-compiled, like `data/player_display_names.json`,
//! so **editing it needs a redeploy, not a data sync**.

use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

const EXCLUDED_GAMES_JSON: &str = include_str!("../../../data/excluded_games.json");

#[derive(Debug, Deserialize)]
struct ExcludedGames {
    games: Vec<ExcludedGame>,
}

/// `game` and `reason` are for the person reading the file. Only the id is used.
#[derive(Debug, Deserialize)]
struct ExcludedGame {
    natstat_id: String,
}

fn ids() -> &'static HashSet<String> {
    static IDS: OnceLock<HashSet<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        serde_json::from_str::<ExcludedGames>(EXCLUDED_GAMES_JSON)
            .expect("data/excluded_games.json is compiled in and must parse")
            .games
            .into_iter()
            .map(|g| g.natstat_id)
            .collect()
    })
}

/// Whether this NatStat game id is on the exclusion list.
pub fn is_excluded(natstat_id: &str) -> bool {
    ids().contains(natstat_id)
}

/// Every excluded NatStat game id, sorted.
pub fn excluded_game_ids() -> Vec<String> {
    let mut all: Vec<String> = ids().iter().cloned().collect();
    all.sort();
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_parses_and_matches_exact_ids() {
        assert!(is_excluded("1517094"));
        assert!(!is_excluded("151709"));
        assert!(excluded_game_ids().contains(&"1517094".to_string()));
    }
}
