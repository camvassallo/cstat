//! R4 invariant guard (see `docs/intraseason_data_safety_plan.md` §R4).
//!
//! These four PBP / lineups-object source tables stay EXCLUDED from every
//! `sync_to_prod.sh` push:
//!
//!   play_by_play, lineup_stints, natstat_lineups, natstat_lineup_games
//!
//! **The assertion is unchanged. The reason for it inverted at tipoff (#249),
//! and the old reason is now actively misleading**, so it is worth being precise
//! about which claim this test is defending.
//!
//! *What it used to protect.* `compute_pbp_lineups` (`compute.rs`, the
//! `if games.is_empty() && covered_pairs.is_empty()` early-return) no-ops on a
//! prod holding zero PBP. Keeping the sources local-only guaranteed that
//! emptiness, which made the laptop the SOLE prod writer of
//! `lineup_aggregates` / `player_on_off` — and made
//! `--tables lineup_aggregates,player_on_off` the documented in-season push.
//!
//! *Why that premise expired.* Prod now ingests its own PBP (the nightly's
//! `playbyplay` / `lineups` steps). From the first game of a season the
//! early-return no longer fires, and prod rebuilds both rollups every night.
//! The laptop is no longer the sole writer; it is the WRONG writer, and pushing
//! those two rollups is now the collision this test used to prevent. That
//! guidance moved out of `CLAUDE.md`; the ownership table is
//! `docs/tipoff_self_sufficiency_plan.md` §3.
//!
//! *What the exclusion protects now.* Two things, both still load-bearing:
//!
//! 1. **Scope coherence.** Prod's PBP is exactly what prod itself ingested —
//!    the current season. So `compute_pbp_lineups` rewrites exactly the current
//!    season's rollups, and the laptop keeps ownership of the historical ones.
//!    Ship historical PBP up and prod starts rebuilding historical rollups too,
//!    putting both sides on the same rows: the collision, one season-range over.
//! 2. **Volume.** A lived-through season of PBP is ~1 GB against a 10 GB prod
//!    volume, before `lineup_stints` (#252). The exclusion is what keeps the
//!    laptop's whole PBP history out of that budget.
//!
//! So: do not drop any of the four from EXCLUDED, and do not re-add
//! `lineup_aggregates` / `player_on_off` to a `--tables` push for a season prod
//! is ingesting. Nothing else tests this coupling; it is a coupling, not a
//! guarantee.
//!
//! This is a pure static check: it reads the shell script and needs no DB, so it
//! runs in plain `cargo test` and trips CI the moment the invariant breaks
//! (unlike the DB-gated `#[ignore]` invariant tests in `swapped_games.rs`).

use std::path::PathBuf;

/// The PBP / lineups-object source tables that stay local-only. Their absence
/// from prod is what confines `compute_pbp_lineups` to the season prod ingested
/// itself, and what keeps the laptop's PBP history out of prod's disk budget.
/// Removing any from EXCLUDED breaks R4.
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
         These PBP / lineups-object source tables MUST stay local-only. Prod \
         ingests its own PBP for the current season and rebuilds \
         lineup_aggregates / player_on_off from it every night; shipping the \
         laptop's history up widens that rebuild over the historical seasons the \
         laptop owns, putting two writers on the same rows — and costs ~1 GB per \
         season against a 10 GB volume. See docs/intraseason_data_safety_plan.md \
         §R4, docs/tipoff_self_sufficiency_plan.md §3, and the early-return in \
         compute.rs::compute_pbp_lineups.\n\
         Current EXCLUDED = {excluded:?}"
    );
}
