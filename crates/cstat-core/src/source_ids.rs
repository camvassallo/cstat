//! The `natstat_id` of a row CollegeBasketballData created
//! (docs/natstat_migration_plan.md §4.5 step 5).
//!
//! The gap-fill creates `games` and `players` rows NatStat has never listed.
//! They carry their ESPN id in `espn_id` like every mapped row, and a
//! placeholder `natstat_id` of `espn:<ESPN id>` rather than NULL:
//! `players.natstat_id` is `NOT NULL` and `(natstat_id, season)`-unique, and
//! readers across the workspace decode `games.natstat_id` and
//! `players.natstat_id` as a plain string, so NULL would fail them at runtime
//! where a placeholder cannot. NatStat's ids are numeric, so the prefix never
//! collides with one.
//!
//! The placeholder is temporary by design. When NatStat later lists the same
//! game or player, its ingest **adopts** the CBBD-created row (writes its own id
//! over the placeholder) instead of inserting a second one, so the season has
//! one row per game and per person whichever source saw it first
//! (`cstat_ingest::ingest::games::adopt_cbbd_games` / `adopt_cbbd_player`).

/// The prefix of a placeholder `natstat_id` on a CBBD-created row.
pub const CBBD_CREATED_PREFIX: &str = "espn:";

/// The placeholder `natstat_id` for a row CBBD created from ESPN id `espn_id`.
pub fn cbbd_created_id(espn_id: &str) -> String {
    format!("{CBBD_CREATED_PREFIX}{espn_id}")
}

/// Whether `natstat_id` is a CBBD-created placeholder rather than NatStat's.
pub fn is_cbbd_created(natstat_id: &str) -> bool {
    natstat_id.starts_with(CBBD_CREATED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_recognised_and_natstat_ids_are_not() {
        assert_eq!(cbbd_created_id("401921689"), "espn:401921689");
        assert!(is_cbbd_created(&cbbd_created_id("5")));
        assert!(!is_cbbd_created("1516563"));
        assert!(!is_cbbd_created(""));
    }
}
