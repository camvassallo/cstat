//! Shared pieces of the name linkers that bind a third-party player row
//! (a Torvik season row, a 247 portal entry) to a cstat `players` row once
//! exact-name matching has run out.
//!
//! The pairing rule lives here so the linkers cannot drift apart: a fix to how
//! ambiguity is refused reaches every caller at once.

use std::collections::HashMap;
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

#[cfg(test)]
mod tests {
    use super::*;

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
