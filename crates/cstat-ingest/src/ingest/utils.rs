//! Shared JSON value extraction helpers for NatStat ingestion.
//!
//! NatStat returns numeric fields as either JSON numbers or strings,
//! so we need flexible parsing that handles both.

use serde_json::Value;

/// Extract f64 from a JSON value that may be a number or string.
pub fn parse_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Extract i32 from a JSON value that may be a number or string.
pub fn parse_i32(v: &Value) -> Option<i32> {
    v.as_i64()
        .map(|i| i as i32)
        .or_else(|| v.as_f64().map(|f| f as i32))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Extract f64 from a JSON object, trying multiple field names in order.
pub fn get_f64(v: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(val) = v.get(*key)
            && let Some(f) = parse_f64(val)
        {
            return Some(f);
        }
    }
    None
}

/// Extract i32 from a JSON object, trying multiple field names in order.
pub fn get_i32(v: &Value, keys: &[&str]) -> Option<i32> {
    for key in keys {
        if let Some(val) = v.get(*key)
            && let Some(i) = parse_i32(val)
        {
            return Some(i);
        }
    }
    None
}

/// Extract f64 from a nested JSON path: `parent[key]`.
pub fn get_f64_from(parent: Option<&Value>, key: &str) -> Option<f64> {
    parent?.get(key).and_then(parse_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_f64_from_number() {
        assert_eq!(parse_f64(&json!(32.5)), Some(32.5));
    }

    #[test]
    fn parse_f64_from_string() {
        assert_eq!(parse_f64(&json!("32.5")), Some(32.5));
    }

    #[test]
    fn parse_f64_from_null() {
        assert_eq!(parse_f64(&json!(null)), None);
    }

    #[test]
    fn parse_i32_from_number() {
        assert_eq!(parse_i32(&json!(25)), Some(25));
    }

    #[test]
    fn parse_i32_from_string() {
        assert_eq!(parse_i32(&json!("25")), Some(25));
    }

    #[test]
    fn parse_i32_from_float() {
        assert_eq!(parse_i32(&json!(25.0)), Some(25));
    }

    #[test]
    fn get_f64_first_key() {
        let v = json!({"min": 32.5});
        assert_eq!(get_f64(&v, &["min"]), Some(32.5));
    }

    #[test]
    fn get_f64_fallback_key() {
        let v = json!({"mp": 28.0});
        assert_eq!(get_f64(&v, &["min", "minutes", "mp"]), Some(28.0));
    }

    #[test]
    fn get_f64_missing_key() {
        let v = json!({"other": 1.0});
        assert_eq!(get_f64(&v, &["min"]), None);
    }

    #[test]
    fn get_i32_from_object() {
        let v = json!({"pts": 25});
        assert_eq!(get_i32(&v, &["pts"]), Some(25));
    }

    #[test]
    fn get_i32_from_string_value() {
        let v = json!({"pts": "25"});
        assert_eq!(get_i32(&v, &["pts"]), Some(25));
    }

    #[test]
    fn get_f64_from_nested() {
        let v = json!({"stats": {"ppg": 18.5}});
        assert_eq!(get_f64_from(v.get("stats"), "ppg"), Some(18.5));
    }

    #[test]
    fn get_f64_from_none_parent() {
        assert_eq!(get_f64_from(None, "ppg"), None);
    }
}
