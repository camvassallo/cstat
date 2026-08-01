//! Shared JSON value extraction helpers for NatStat ingestion.
//!
//! NatStat returns numeric fields as either JSON numbers or strings,
//! so we need flexible parsing that handles both.

use serde_json::Value;

/// Decode the HTML entities NatStat leaves in free-text fields (issue #243).
///
/// Some NatStat records arrive HTML-escaped rather than as plain text, so
/// `players.name` accumulated 177 rows storing the escape sequence verbatim —
/// `D&#039;Angelo Russell`, `Devonte&#039; Graham`, `Gregory &quot;GG&quot;
/// Jackson`. Those names are simply wrong in the database and render wrong on
/// the site. Decode at every write site so the raw form can never land again.
///
/// Single pass, like a browser: `&amp;#039;` decodes to the literal
/// `&#039;`, which is what double-escaped source text is *meant* to display.
pub fn decode_html_entities(s: &str) -> String {
    // Overwhelmingly the common case — skip the scan entirely.
    if !s.contains('&') {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // Entities are short; a `;` far away is punctuation, not a terminator.
        match after.find(';').filter(|end| *end <= 8) {
            Some(end) => match decode_entity(&after[..end]) {
                Some(c) => {
                    out.push(c);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode one entity body (the text between `&` and `;`), or `None` if it
/// isn't an entity we recognize — in which case the caller emits it verbatim.
fn decode_entity(body: &str) -> Option<char> {
    match body {
        "amp" => return Some('&'),
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        "nbsp" => return Some(' '),
        _ => {}
    }
    let digits = body.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

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

    // decode_html_entities (issue #243)

    #[test]
    fn decode_numeric_apostrophe_entities() {
        // The two forms actually present in `players.name`.
        assert_eq!(
            decode_html_entities("D&#039;Angelo Russell"),
            "D'Angelo Russell"
        );
        assert_eq!(
            decode_html_entities("Ja&#39;Vier Francis"),
            "Ja'Vier Francis"
        );
        assert_eq!(
            decode_html_entities("Devonte&#039; Graham"),
            "Devonte' Graham"
        );
    }

    #[test]
    fn decode_named_entities() {
        assert_eq!(
            decode_html_entities("Gregory &quot;GG&quot; Jackson"),
            "Gregory \"GG\" Jackson"
        );
        assert_eq!(decode_html_entities("Smith &amp; Jones"), "Smith & Jones");
    }

    #[test]
    fn decode_hex_entities() {
        assert_eq!(decode_html_entities("D&#x27;Angelo"), "D'Angelo");
    }

    #[test]
    fn decode_leaves_plain_names_untouched() {
        assert_eq!(decode_html_entities("Cooper Flagg"), "Cooper Flagg");
        assert_eq!(decode_html_entities("D'Angelo Russell"), "D'Angelo Russell");
    }

    #[test]
    fn decode_leaves_bare_ampersand_untouched() {
        // A `&` that isn't an entity must survive verbatim — team names like
        // "Texas A&M" flow through the same helper.
        assert_eq!(decode_html_entities("Texas A&M"), "Texas A&M");
        assert_eq!(decode_html_entities("A & B"), "A & B");
        assert_eq!(decode_html_entities("trailing &"), "trailing &");
    }

    #[test]
    fn decode_ignores_unknown_and_overlong_entities() {
        assert_eq!(decode_html_entities("&bogus;"), "&bogus;");
        // A distant `;` is punctuation, not an entity terminator.
        assert_eq!(
            decode_html_entities("Smith & Sons, Inc; Ltd"),
            "Smith & Sons, Inc; Ltd"
        );
    }

    #[test]
    fn decode_is_single_pass_like_a_browser() {
        // Double-escaped source is *meant* to render as the literal escape.
        assert_eq!(decode_html_entities("&amp;#039;"), "&#039;");
    }
}
