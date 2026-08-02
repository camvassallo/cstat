//! Presentation names for players (issue #243 follow-up).
//!
//! `players.name` is NatStat's legal name and is the join key five resolvers
//! match on (transfers, recruits, draft, awards, and the Torvik matcher). It
//! stays put. `players.display_name` carries the name to *show*, and the API
//! serves `COALESCE(display_name, name)` so the wire format is unchanged.
//!
//! # Why the rule is narrow
//!
//! Torvik carries the name people actually use, so "just use Torvik's" looks
//! obvious. The data says otherwise. Across the 1,043 player-seasons where the
//! two sources disagree substantively, 247Sports — a third, independent feed —
//! sides with NatStat **44 to 20**. Torvik is frequently the source with the
//! typo:
//!
//! | Torvik | NatStat | 247 agrees with |
//! | --- | --- | --- |
//! | Jeff**e**ry Solarin | Jeffrey Solarin | NatStat |
//! | Ezra Aus**u**r | Ezra Ausar | NatStat |
//! | Mart**e**z Robinson | Martaz Robinson | NatStat |
//! | Javonte Johnson | Javon**té** Johnson | NatStat |
//!
//! Adopting Torvik wholesale would trade one class of wrong names for another.
//! So only two things are allowed to set a display name:
//!
//! 1. [`suffix_restoration`] — NatStat drops generational suffixes ("Jaren
//!    Jackson" for Jaren Jackson Jr., "Marvin Bagley" for Marvin Bagley III);
//!    Torvik keeps them. Safe *by construction*: the rule fires only when the
//!    two names are identical after the suffix is stripped, so it can never
//!    introduce a spelling the sources didn't already agree on. ~2,000 players.
//! 2. [`overrides`] — a hand-curated table for the marquee cases where the
//!    legal name simply isn't the known one. Small, and each entry carries the
//!    evidence that justified it.
//!
//! Everything else is left alone: `display_name` stays NULL and `name` shows.

use serde::Deserialize;
use std::sync::OnceLock;

/// One curated display name, keyed by the cross-season Torvik player id.
///
/// `torvik_pid` rather than name or `natstat_id`: names collide (two different
/// Jonathan Davises, one of whom is Johnny), and `natstat_id` is re-minted when
/// a player transfers, so it would need one entry per stop.
#[derive(Debug, Clone, Deserialize)]
pub struct DisplayNameOverride {
    pub torvik_pid: i32,
    pub display_name: String,
    /// Why this entry exists. Required by convention — an override without a
    /// stated reason is indistinguishable from a typo six months later.
    #[allow(dead_code)]
    #[serde(default)]
    pub note: String,
}

const OVERRIDES_JSON: &str = include_str!("../../../data/player_display_names.json");

/// The curated overrides, parsed once.
pub fn overrides() -> &'static [DisplayNameOverride] {
    static PARSED: OnceLock<Vec<DisplayNameOverride>> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(OVERRIDES_JSON)
            .expect("data/player_display_names.json must be valid JSON")
    })
}

/// Generational suffixes NatStat drops and Torvik keeps.
const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "v"];

/// Split a name into `(base, suffix)`, where `suffix` is the trailing
/// generational token if there is one.
///
/// Only a *trailing* token counts. NatStat sometimes mangles the order
/// ("Dachon Jr Burke"); that leaves the suffix mid-string, the base won't match
/// Torvik's, and the row is skipped rather than guessed at.
fn split_suffix(name: &str) -> (&str, Option<&str>) {
    let trimmed = name.trim_end();
    let Some((head, last)) = trimmed.rsplit_once(char::is_whitespace) else {
        return (trimmed, None);
    };
    let bare = last.trim_end_matches('.').to_ascii_lowercase();
    if SUFFIXES.contains(&bare.as_str()) {
        (head.trim_end(), Some(last))
    } else {
        (trimmed, None)
    }
}

/// Comparison key for a name's base: lowercase, letters and digits only.
/// Drops the punctuation the two feeds disagree about (`D.J.` vs `DJ`,
/// `O'Neale` vs `ONeale`) so only a real spelling difference blocks a match.
fn base_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The display name to use when Torvik supplies a generational suffix that
/// NatStat dropped, or `None` when the rule doesn't apply.
///
/// Fires only when every one of these holds:
/// - the two names differ at all;
/// - their bases are identical once the suffix is stripped — this is the
///   safety property, and it is why the rule cannot import a Torvik typo;
/// - Torvik has a trailing suffix and NatStat does not.
///
/// The reverse case (NatStat has the suffix, Torvik dropped it) returns `None`:
/// `name` is already the fuller form, so there is nothing to improve.
pub fn suffix_restoration(cstat_name: &str, torvik_name: &str) -> Option<String> {
    let (c_base, c_suffix) = split_suffix(cstat_name);
    let (t_base, t_suffix) = split_suffix(torvik_name);

    if t_suffix.is_none() || c_suffix.is_some() {
        return None;
    }
    if base_key(c_base) != base_key(t_base) {
        return None;
    }
    if cstat_name.eq_ignore_ascii_case(torvik_name) {
        return None;
    }

    // Keep NatStat's spelling of the base and borrow only the suffix. The
    // bases are equal modulo punctuation, and NatStat is the side 247 backs on
    // punctuation-level differences ("Javonté" over "Javonte").
    Some(format!("{} {}", c_base.trim(), t_suffix?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_a_dropped_suffix() {
        assert_eq!(
            suffix_restoration("Jaren Jackson", "Jaren Jackson Jr."),
            Some("Jaren Jackson Jr.".to_string())
        );
        assert_eq!(
            suffix_restoration("Marvin Bagley", "Marvin Bagley III"),
            Some("Marvin Bagley III".to_string())
        );
        assert_eq!(
            suffix_restoration("Wade Taylor", "Wade Taylor IV"),
            Some("Wade Taylor IV".to_string())
        );
    }

    #[test]
    fn keeps_natstat_spelling_of_the_base() {
        // Bases match modulo punctuation/diacritics; we take NatStat's, which
        // is the side the third source backs on that class of difference.
        assert_eq!(
            suffix_restoration("Javonté Johnson", "Javonte Johnson Jr."),
            None,
            "different base letters must not be bridged"
        );
        assert_eq!(
            suffix_restoration("D.J. Wagner", "DJ Wagner Jr."),
            Some("D.J. Wagner Jr.".to_string())
        );
    }

    #[test]
    fn refuses_when_the_base_names_differ() {
        // The whole safety property: a Torvik typo cannot ride in on a suffix.
        assert_eq!(suffix_restoration("Ezra Ausar", "Ezra Ausur Jr."), None);
        assert_eq!(suffix_restoration("Obadiah Toppin", "Obi Toppin Jr."), None);
    }

    #[test]
    fn refuses_when_natstat_already_has_a_suffix() {
        assert_eq!(
            suffix_restoration("Ace Baldwin Jr.", "Ace Baldwin Jr."),
            None
        );
        // NatStat fuller than Torvik — nothing to add.
        assert_eq!(suffix_restoration("Ace Baldwin Jr.", "Ace Baldwin"), None);
    }

    #[test]
    fn refuses_a_mid_string_suffix() {
        // NatStat's mangled ordering leaves the suffix off the end, so the
        // bases don't match and the row is skipped rather than guessed at.
        assert_eq!(
            suffix_restoration("Dachon Jr Burke", "Dachon Burke Jr."),
            None
        );
    }

    #[test]
    fn refuses_identical_names() {
        assert_eq!(suffix_restoration("Cooper Flagg", "Cooper Flagg"), None);
        assert_eq!(suffix_restoration("Cooper Flagg", "cooper flagg"), None);
    }

    #[test]
    fn a_lone_suffix_token_is_not_a_name() {
        assert_eq!(suffix_restoration("Jr", "Jr"), None);
    }

    #[test]
    fn overrides_parse_and_are_well_formed() {
        let all = overrides();
        assert!(!all.is_empty(), "seed overrides should be present");
        for o in all {
            assert!(!o.display_name.trim().is_empty(), "pid {}", o.torvik_pid);
            assert!(
                !o.note.trim().is_empty(),
                "pid {} needs a note explaining why it is overridden",
                o.torvik_pid
            );
        }
        let mut pids: Vec<i32> = all.iter().map(|o| o.torvik_pid).collect();
        pids.sort_unstable();
        let before = pids.len();
        pids.dedup();
        assert_eq!(before, pids.len(), "duplicate torvik_pid in overrides");
    }
}
