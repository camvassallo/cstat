//! R4 invariant guard (see `docs/intraseason_data_safety_plan.md` §R4).
//!
//! The targeted `--tables lineup_aggregates,player_on_off` sync is the SOLE prod
//! writer of those two rollups **only because** `compute_pbp_lineups`
//! (`compute.rs`, the `if games.is_empty() && covered_pairs.is_empty()`
//! early-return) no-ops on a prod that holds zero PBP / lineups-object source
//! rows. That emptiness is guaranteed by these four source tables being EXCLUDED
//! from every `sync_to_prod.sh` push:
//!
//!   play_by_play, lineup_stints, natstat_lineups, natstat_lineup_games
//!
//! Ship any of them to prod (drop it from EXCLUDED, or a "let's serve stints"
//! change) and the nightly's `compute_pbp_lineups` would begin DELETE-ing and
//! rebuilding `lineup_aggregates` / `player_on_off` every night, silently
//! colliding with — and erasing — the operator's targeted sync. Nothing else
//! tests this coupling; it is a coupling, not a guarantee.
//!
//! This is a pure static check: it reads the shell script and needs no DB, so it
//! runs in plain `cargo test` and trips CI the moment the invariant breaks
//! (unlike the DB-gated `#[ignore]` invariant tests in `swapped_games.rs`).

use std::path::PathBuf;

/// The PBP / lineups-object source tables whose absence from prod is what makes
/// `compute_pbp_lineups` no-op there. Removing any from EXCLUDED breaks R4.
const REQUIRED_EXCLUDED: &[&str] = &[
    "play_by_play",
    "lineup_stints",
    "natstat_lineups",
    "natstat_lineup_games",
];

/// Parse the single-line bash array `EXCLUDED=("a" "b" ...)` out of
/// `scripts/sync_to_prod.sh` and return its double-quoted table names.
fn sync_script_excluded() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sync_to_prod.sh");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with("EXCLUDED=("))
        .unwrap_or_else(|| {
            panic!(
                "no `EXCLUDED=(` array found in {} — the sync script's exclusion \
                 list moved or was renamed; update this guard test to match",
                path.display()
            )
        });

    // Pull every double-quoted token out of the array literal. Splitting on `"`
    // yields alternating [outside, inside, outside, inside, …]; the inside
    // tokens are at odd indices.
    line.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn pbp_lineup_source_tables_stay_excluded_from_prod_sync() {
    let excluded = sync_script_excluded();
    let missing: Vec<&str> = REQUIRED_EXCLUDED
        .iter()
        .copied()
        .filter(|t| !excluded.iter().any(|e| e == t))
        .collect();

    assert!(
        missing.is_empty(),
        "R4 invariant broken: {missing:?} dropped from sync_to_prod.sh EXCLUDED.\n\
         These PBP / lineups-object source tables MUST stay local-only. Shipping \
         them to prod makes the nightly's compute_pbp_lineups rebuild \
         lineup_aggregates / player_on_off every night, colliding with the \
         operator's `--tables` push. See docs/intraseason_data_safety_plan.md §R4 \
         and the early-return in compute.rs::compute_pbp_lineups.\n\
         Current EXCLUDED = {excluded:?}"
    );
}
