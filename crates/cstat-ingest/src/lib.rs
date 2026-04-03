pub mod cache;
pub mod client;
pub mod ingest;
pub mod rate_limiter;

pub use client::NatStatClient;

use serde_json::Value;

/// Extract the data payload from a NatStat API response.
///
/// NatStat v4 puts results under endpoint-specific keys (e.g., "teamcodes", "games", "players")
/// rather than a generic "results" key. This finds the first non-metadata key.
pub fn extract_results(page: &Value) -> Vec<&Value> {
    const META_KEYS: &[&str] = &["meta", "user", "query", "success", "error", "warnings"];
    if let Some(obj) = page.as_object() {
        for (key, value) in obj {
            if META_KEYS.contains(&key.as_str()) {
                continue;
            }
            return match value {
                Value::Array(arr) => arr.iter().collect(),
                Value::Object(inner) => inner.values().collect(),
                _ => vec![],
            };
        }
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_results_from_teamcodes_object() {
        let response = json!({
            "meta": {"results-total": 2},
            "success": "1",
            "user": {"ratelimit": 500},
            "query": {},
            "teamcodes": {
                "team_224": {"code": "KU", "name": "Kansas Jayhawks"},
                "team_236": {"code": "DUKE", "name": "Duke Blue Devils"}
            }
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_extract_results_from_games_object() {
        let response = json!({
            "meta": {},
            "success": "1",
            "user": {},
            "query": {},
            "games": {
                "game_123": {"id": "123", "gameday": "2026-03-15"},
                "game_456": {"id": "456", "gameday": "2026-03-16"}
            }
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_extract_results_from_array() {
        let response = json!({
            "meta": {},
            "success": "1",
            "data": [{"id": 1}, {"id": 2}, {"id": 3}]
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_extract_results_skips_metadata() {
        let response = json!({
            "meta": {"page": 1},
            "success": "1",
            "user": {"account": "test"},
            "query": {"endpoint": "teams"},
            "error": "",
            "warnings": ""
        });
        // All keys are metadata — no data payload
        let results = extract_results(&response);
        assert!(results.is_empty());
    }

    #[test]
    fn test_extract_results_empty_response() {
        let response = json!(null);
        let results = extract_results(&response);
        assert!(results.is_empty());
    }
}
