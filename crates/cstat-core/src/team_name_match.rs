//! Cross-source team-name matching for 247 / Torvik / NatStat alignment.
//!
//! The 247 portal feed and NBA Draft sources use short names ("Kansas",
//! "UConn", "NC State"); cstat carries the Torvik short_name AND the full
//! NatStat name on `teams`. This module resolves a 247-style short name
//! to a cstat team by scoring exact short_name hits first, then alias
//! table hits, then bare prefix matches against the full name.
//!
//! Originally lived in both `cstat-api/src/routes/transfers.rs` and
//! `cstat-ingest/src/ingest/transfers.rs` as a doc-cross-referenced
//! duplicate pair. Promoted here once a third consumer
//! (`roster_projection`) needed the same matching — see the
//! "Helpers are duplicated ... promote to a shared module if a third
//! consumer appears" note on the original definitions.

/// 247 short name → cstat team-name prefix that should appear at the
/// start of `teams.name`. Listed only for cases the bare prefix branch
/// can't catch (acronyms like "UConn" don't prefix "Connecticut Huskies"),
/// or to nudge ambiguous prefix matches toward the canonical school
/// (bare "Miami" should resolve to Miami (Fla.), not Miami (Ohio)).
/// Add entries here as we spot misses.
pub const TEAM_ALIASES: &[(&str, &str)] = &[
    ("uconn", "connecticut"),
    ("ole miss", "mississippi"),
    ("usc", "southern california"),
    ("nc state", "north carolina state"),
    // Bare "Miami" prefix-matches both Florida and Ohio — anchor it to FL.
    ("miami", "miami (fla.)"),
    ("miami (fl)", "miami (fla.)"),
    ("miami (oh)", "miami (ohio)"),
    // 247 recruits use bare "Kansas City" / "Pennsylvania" where cstat
    // carries the full NatStat names. Exact-match (not prefix) to avoid
    // sweeping in Penn State or other related programs.
    ("kansas city", "missouri-kansas city kangaroos"),
    ("pennsylvania", "penn quakers"),
    // barttorvik coachdict spellings that differ from cstat's hyphenated full
    // names (the truncation/hyphen drops the bare-prefix branch). "Texas A&M
    // Corpus Chris" is coachdict's truncation of Corpus Christi — two team-name
    // variants exist across seasons (hyphenated "…-Corpus Christi Islanders"
    // early, bare "Texas A&M Corpus Christi" recently), so both targets are
    // listed under the one key; the scorer checks every alias entry.
    ("texas a&m corpus chris", "texas a&m-corpus christi"),
    ("texas a&m corpus chris", "texas a&m corpus christi"),
    ("ut martin", "tennessee-martin"),
    ("arkansas little rock", "arkansas-little rock"),
    // Renamed schools. barttorvik retro-applies a program's *current* name to
    // its historical seasons while NatStat keeps the contemporaneous one, so
    // the two only disagree where a school renamed. Houston Baptist became
    // Houston Christian in 2022; Torvik calls the 2015-2021 rows "Houston
    // Christian" too, which stranded that roster every season (issue #243).
    ("houston christian", "houston baptist"),
];

/// Score how well a cstat team matches a 247 short name. Lower is better;
/// `None` means no match. Tries the Torvik-style `short_name` first
/// (which usually matches 247 directly, e.g. "Kansas" == "Kansas") and
/// falls back to the full NatStat name with alias/prefix logic for
/// legacy edge cases.
pub fn team_match_score(db_short: Option<&str>, db_full: &str, short: &str) -> Option<u32> {
    let short_lc = short.to_lowercase();
    // 0 = exact short_name match. The common case now that teams.short_name
    // is populated with Torvik names — "Kansas", "UConn", "Duke" all
    // resolve here.
    if let Some(s) = db_short
        && s.to_lowercase() == short_lc
    {
        return Some(0);
    }
    let db_lc = db_full.to_lowercase();
    if db_lc == short_lc {
        return Some(0);
    }
    // 1 = alias hit against the full name. Kept for 247-side aliases
    // that don't equal the short_name (e.g. "miami" → "Miami FL";
    // "ole miss" → "Mississippi"; ambiguous bare names like "Miami").
    for (k, v) in TEAM_ALIASES {
        if short_lc == *k && (db_lc == *v || db_lc.starts_with(&format!("{v} "))) {
            return Some(1);
        }
    }
    // 2 = bare prefix match against the full name. Catches the case
    // where short_name is missing — falls back to old behavior.
    if db_lc.starts_with(&format!("{short_lc} ")) {
        return Some(2);
    }
    None
}

/// Boolean wrapper around `team_match_score`, kept for callers that don't
/// need the score (the player-disambiguation pass). Takes both the
/// Torvik short_name and the full NatStat name so alias entries that
/// target the full form (e.g. "nc state" → "north carolina state") still
/// fire.
pub fn team_matches(db_short: Option<&str>, db_full: Option<&str>, short_name: &str) -> bool {
    db_full
        .map(|full| team_match_score(db_short, full, short_name).is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_short_name_scores_best() {
        assert_eq!(
            team_match_score(Some("Kansas"), "Kansas Jayhawks", "Kansas"),
            Some(0),
        );
    }

    #[test]
    fn alias_resolves_when_short_name_misses() {
        assert_eq!(
            team_match_score(Some("Connecticut"), "Connecticut Huskies", "UConn"),
            Some(1),
        );
    }

    #[test]
    fn miami_alias_anchors_florida_over_ohio_prefix() {
        // Bare "Miami" would prefix-match both, but the FL alias scores
        // better (1) than the OH bare prefix (2), so min_by_key picks FL.
        let fl = team_match_score(None, "Miami (Fla.) Hurricanes", "Miami");
        let oh = team_match_score(None, "Miami (Ohio) RedHawks", "Miami");
        assert!(fl < oh, "FL ({fl:?}) should outscore OH ({oh:?})");
    }

    #[test]
    fn bare_prefix_fallback_when_no_alias() {
        assert_eq!(team_match_score(None, "Duke Blue Devils", "Duke"), Some(2),);
    }

    #[test]
    fn houston_rename_alias_beats_the_houston_bare_prefix() {
        // Torvik calls the pre-2022 Houston Baptist seasons "Houston
        // Christian" (issue #243). The rename alias must land on Baptist,
        // and the real Houston must still outscore Baptist's bare prefix so
        // a "Houston" row can't drift onto the wrong roster.
        assert_eq!(
            team_match_score(
                Some("Houston Baptist"),
                "Houston Baptist Huskies",
                "Houston Christian"
            ),
            Some(1),
        );
        let cougars = team_match_score(Some("Houston"), "Houston Cougars", "Houston");
        let huskies = team_match_score(
            Some("Houston Baptist"),
            "Houston Baptist Huskies",
            "Houston",
        );
        assert!(
            cougars < huskies,
            "Houston ({cougars:?}) should outscore Houston Baptist ({huskies:?})"
        );
    }

    #[test]
    fn returns_none_for_unrelated() {
        assert_eq!(
            team_match_score(Some("Kansas"), "Kansas Jayhawks", "Gonzaga"),
            None,
        );
    }

    #[test]
    fn umkc_alias_resolves_kansas_city() {
        // 247 sends bare "Kansas City"; cstat carries the full NatStat name
        // "Missouri-Kansas City Kangaroos" — short_name=UMKC doesn't match.
        assert_eq!(
            team_match_score(
                Some("UMKC"),
                "Missouri-Kansas City Kangaroos",
                "Kansas City",
            ),
            Some(1),
        );
    }

    #[test]
    fn penn_alias_resolves_pennsylvania_and_excludes_penn_state() {
        // 247 sends "Pennsylvania" for the Ivy League Quakers.
        assert_eq!(
            team_match_score(Some("Penn"), "Penn Quakers", "Pennsylvania"),
            Some(1),
        );
        // Exact-match alias must NOT also resolve "Pennsylvania" → Penn State,
        // which is a different program with a different short_name.
        assert_eq!(
            team_match_score(
                Some("Penn State"),
                "Penn State Nittany Lions",
                "Pennsylvania",
            ),
            None,
        );
    }
}
