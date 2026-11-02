//! A cheap guard against invented numbers: every number in a draft must
//! appear somewhere in the facts it was written from.
//!
//! It is a flag, not a filter. A miss is shown next to the draft in Slack so
//! the proofreader checks that one figure; the draft is never dropped, because
//! a legitimate rewording ("a 31-point night" from `points: 31`) passes and a
//! harmless one ("two straight") has no digits to check.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)?").expect("valid regex"));

/// Small integers read as words more often than as stats ("3 drafts", "1st",
/// "No. 1" when rank 1 is also in the facts). Not worth a flag.
const ALWAYS_OK: [&str; 4] = ["0", "1", "2", "3"];

/// The numbers in `text` that do not appear in `facts`.
pub fn unverified_numbers(text: &str, facts: &Value) -> Vec<String> {
    let known = known_numbers(facts);
    let mut seen = HashSet::new();
    NUMBER
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .filter(|n| !ALWAYS_OK.contains(&n.as_str()) && !known.contains(n))
        .filter(|n| seen.insert(n.clone()))
        .collect()
}

/// Every spelling of every number in the facts a draft could fairly use: the
/// value as stored, rounded to one and zero decimals, and a 0-1 probability as
/// a percentage.
fn known_numbers(facts: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    walk(facts, &mut out);
    out
}

fn walk(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::Number(n) => {
            if let Some(x) = n.as_f64() {
                add_spellings(x, out);
            }
        }
        Value::String(s) => {
            for m in NUMBER.find_iter(s) {
                out.insert(m.as_str().to_string());
                if let Ok(x) = m.as_str().parse::<f64>() {
                    add_spellings(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
        Value::Object(o) => o.values().for_each(|x| walk(x, out)),
        _ => {}
    }
}

fn add_spellings(x: f64, out: &mut HashSet<String>) {
    let x = x.abs();
    out.insert(format!("{x:.0}"));
    out.insert(format!("{x:.1}"));
    out.insert(trim(x));
    if x > 0.0 && x < 1.0 {
        out.insert(format!("{:.0}", x * 100.0));
        out.insert(format!("{:.1}", x * 100.0));
    }
}

/// The shortest plain spelling: 31.0 → "31", 7.25 → "7.25".
fn trim(x: f64) -> String {
    let s = format!("{x}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_from_the_facts_pass() {
        let facts = json!({
            "points": 31, "campom": 22.43, "home_win_probability": 0.579,
            "record": "12-3", "tip": "7:30 PM ET"
        });
        let text = "31 points, a 22.4 CAM, and we gave them a 58% chance. Now 12-3. Tip 7:30.";
        assert!(unverified_numbers(text, &facts).is_empty());
    }

    #[test]
    fn invented_numbers_are_flagged_once() {
        let facts = json!({"points": 31});
        let text = "31 points on 64% shooting, 64% from three";
        assert_eq!(unverified_numbers(text, &facts), vec!["64".to_string()]);
    }

    #[test]
    fn signs_do_not_matter() {
        let facts = json!({"predicted_margin": -7.5});
        assert!(unverified_numbers("favored by 7.5", &facts).is_empty());
    }

    #[test]
    fn small_word_numbers_are_ignored() {
        assert!(unverified_numbers("3 takes, 1 game", &json!({})).is_empty());
    }
}
