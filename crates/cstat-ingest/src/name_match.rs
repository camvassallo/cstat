//! Shared pieces of the name linkers that bind a third-party player row
//! (a Torvik season row, a 247 portal entry) to a cstat `players` row once
//! exact-name matching has run out.
//!
//! The pairing rule lives here so the linkers cannot drift apart: a fix to how
//! ambiguity is refused reaches every caller at once.

use cstat_core::roster_projection::normalize_player_name;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Pair source rows to candidates that share a grouping key, accepting a group
/// only when it holds exactly one row AND exactly one candidate and `guard`
/// agrees with that pairing. Returns `(row index, candidate index)` pairs,
/// sorted by row index.
///
/// Requiring uniqueness on the source side as well as the candidate side is
/// what makes the pass order-independent: with at most one claimant and at
/// most one candidate per key, no two rows can ever contend for the same
/// player, so the result doesn't depend on which row is visited first.
///
/// Callers pass only rows still unmatched and candidates still unclaimed, and
/// apply the claims themselves.
pub fn pair_unambiguous<K: Eq + Hash>(
    rows: &[K],
    candidates: &[K],
    guard: impl Fn(usize, usize) -> bool,
) -> Vec<(usize, usize)> {
    let mut groups: HashMap<&K, Vec<usize>> = HashMap::new();
    for (i, k) in rows.iter().enumerate() {
        groups.entry(k).or_default().push(i);
    }
    let mut pool: HashMap<&K, Vec<usize>> = HashMap::new();
    for (i, k) in candidates.iter().enumerate() {
        pool.entry(k).or_default().push(i);
    }
    let mut out: Vec<(usize, usize)> = groups
        .into_iter()
        .filter_map(
            |(k, group)| match (group.as_slice(), pool.get(k)?.as_slice()) {
                ([row], [cand]) if guard(*row, *cand) => Some((*row, *cand)),
                _ => None,
            },
        )
        .collect();
    out.sort_unstable();
    out
}

/// Pair two box scores' players for one game side: exact normalized name
/// first, then a last name unique on both sides among those left. Returns
/// `(ours, theirs)` index pairs, sorted. An ambiguous name pairs nothing.
///
/// Shared by `source-parity` (comparing the two sources' box lines) and
/// `map-espn-ids` (reading a player's ESPN id off the paired CBBD line), so
/// the id mapping pairs exactly the rows the parity report vouched for.
pub fn pair_box_names<A: AsRef<str>, B: AsRef<str>>(
    ours: &[A],
    theirs: &[B],
) -> Vec<(usize, usize)> {
    let norm_o: Vec<String> = ours
        .iter()
        .map(|n| normalize_player_name(n.as_ref()))
        .collect();
    let norm_t: Vec<String> = theirs
        .iter()
        .map(|n| normalize_player_name(n.as_ref()))
        .collect();
    let mut pairs = pair_unambiguous(&norm_o, &norm_t, |_, _| true);

    let taken_o: HashSet<usize> = pairs.iter().map(|(o, _)| *o).collect();
    let taken_t: HashSet<usize> = pairs.iter().map(|(_, t)| *t).collect();
    let left_o: Vec<usize> = (0..ours.len()).filter(|i| !taken_o.contains(i)).collect();
    let left_t: Vec<usize> = (0..theirs.len()).filter(|i| !taken_t.contains(i)).collect();
    let last = |s: &String| s.split_whitespace().last().unwrap_or("").to_string();
    let last_o: Vec<String> = left_o.iter().map(|&i| last(&norm_o[i])).collect();
    let last_t: Vec<String> = left_t.iter().map(|&i| last(&norm_t[i])).collect();
    pairs.extend(
        pair_unambiguous(&last_o, &last_t, |a, _| !last_o[a].is_empty())
            .into_iter()
            .map(|(a, b)| (left_o[a], left_t[b])),
    );
    pairs.sort_unstable();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_names_pair_by_name_then_unique_last_name() {
        // NatStat drops the suffix ("Abdi Bashir"); CBBD keeps it. Two Smiths
        // on one side make "smith" ambiguous, so neither pairs by last name.
        let ours = ["Abdi Bashir", "DK Thorn", "Al Smith", "Bo Smith"];
        let theirs = [
            "Abdi Bashir Jr.",
            "Dekedran Thorn",
            "Alan Smith",
            "Bob Smith",
        ];
        assert_eq!(pair_box_names(&ours, &theirs), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn box_names_refuse_a_duplicated_full_name() {
        // Two identical names on one side: neither pairs, by name or last name.
        let ours = ["Jake Davis", "Jake Davis"];
        let theirs = ["Jake Davis"];
        assert!(pair_box_names(&ours, &theirs).is_empty());
    }

    #[test]
    fn pairs_only_groups_unique_on_both_sides() {
        let rows = ["a", "b", "b", "c"];
        let cands = ["a", "b", "c", "c"];
        // a: 1 row / 1 cand → paired. b: 2 rows → refused. c: 2 cands → refused.
        assert_eq!(pair_unambiguous(&rows, &cands, |_, _| true), vec![(0, 0)]);
    }

    #[test]
    fn guard_can_veto_a_unique_pair() {
        let rows = ["a"];
        let cands = ["a"];
        assert!(pair_unambiguous(&rows, &cands, |_, _| false).is_empty());
    }
}
