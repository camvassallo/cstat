use std::collections::HashMap;
use std::sync::OnceLock;

const TEAM_SHORT_NAMES_JSON: &str = include_str!("../../../../data/team_short_names.json");

static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Torvik-style short name for a NatStat team code (e.g. `DUKE` -> `Duke`),
/// or `None` if the team isn't in the bundled mapping.
pub fn short_name(natstat_id: &str) -> Option<&'static str> {
    let map = MAP.get_or_init(|| {
        serde_json::from_str(TEAM_SHORT_NAMES_JSON)
            .expect("data/team_short_names.json must be valid JSON")
    });
    map.get(natstat_id).map(|s| s.as_str())
}
