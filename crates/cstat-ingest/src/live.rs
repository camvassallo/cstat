//! Live scores for the Schedule tab: NatStat's `games` feed for one day, read
//! on demand and never written anywhere.
//!
//! The same feed the nightly ingests lists a game in progress with its running
//! score and a clock in `gamestatus` (`H2 12:28`, `Half`), so one fetch per day
//! (two pages on a full slate) is everything a scoreboard needs. Its game ids
//! are the `games.natstat_id` the Schedule tab's rows already carry, and its
//! home and visitor are ours, so there is no matching to do.
//!
//! Nothing here touches the database beyond the response cache. A running
//! score is display only: the stored result, and every season aggregate built
//! on it, waits for the nightly to see the game final
//! (`ingest::games::is_settled_status`).

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;

use crate::client::{NatStatClient, NatStatError};
use crate::extract_results;

/// How long one day's live listing is reused, in seconds. Two calls per day
/// per refresh on a full slate, so a minute costs at most ~120 calls an hour
/// out of the 2,500 budget while anyone has the page open.
pub const LIVE_TTL_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    /// Not started, or postponed / cancelled (then `detail` says which).
    Scheduled,
    Live,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveGame {
    #[serde(skip)]
    pub natstat_id: String,
    pub state: LiveState,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    /// What the scoreboard prints: `12:28 2nd`, `Halftime`, `Final/OT`,
    /// `Postponed`. `None` for a game that has not started.
    pub detail: Option<String>,
}

fn score(v: Option<&Value>) -> Option<i32> {
    let v = v?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .map(|n| n as i32)
}

/// `H2` → `2nd`, `OT` → `OT`, `OT2` → `2OT`.
fn period_label(p: &str) -> Option<String> {
    match p {
        "H1" => Some("1st".into()),
        "H2" => Some("2nd".into()),
        "OT" | "OT1" => Some("OT".into()),
        _ => p
            .strip_prefix("OT")
            .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
            .map(|n| format!("{n}OT")),
    }
}

/// `05:34` → `5:34`; `00:42` → `0:42`.
fn clock_label(c: &str) -> Option<String> {
    let (m, s) = c.split_once(':')?;
    let m: u32 = m.parse().ok()?;
    (s.len() == 2 && s.chars().all(|c| c.is_ascii_digit())).then(|| format!("{m}:{s}"))
}

/// Read a `gamestatus` (plus the overtime flag and whether a score is
/// present) into a state and the label to print.
fn read_status(
    status: Option<&str>,
    overtime: Option<&str>,
    has_score: bool,
) -> (LiveState, Option<String>) {
    let status = status.map(str::trim).filter(|s| !s.is_empty());
    let Some(s) = status else {
        return (LiveState::Scheduled, None);
    };
    if s.to_ascii_lowercase().starts_with("final") {
        let ot = overtime
            .map(str::trim)
            .filter(|o| !o.is_empty() && *o != "N" && !s.contains('/'));
        let detail = match ot {
            Some("Y") => format!("{s}/OT"),
            Some(o) => format!("{s}/{o}"),
            None => s.to_string(),
        };
        return (LiveState::Final, Some(detail));
    }
    if s.eq_ignore_ascii_case("half") || s.eq_ignore_ascii_case("halftime") {
        return (LiveState::Live, Some("Halftime".into()));
    }
    if let Some((p, c)) = s.split_once(' ')
        && let (Some(p), Some(c)) = (period_label(p), clock_label(c))
    {
        return (LiveState::Live, Some(format!("{c} {p}")));
    }
    // Anything else: a game with a score is under way in some form the feed
    // spells differently (shown as sent); one without is postponed, cancelled
    // or delayed.
    if has_score {
        (LiveState::Live, Some(s.to_string()))
    } else {
        (LiveState::Scheduled, Some(s.to_string()))
    }
}

/// One `games` record as a live line. `None` without an id.
pub fn parse_live(game: &Value) -> Option<LiveGame> {
    let natstat_id = game
        .get("id")
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))?
        .trim_matches('"')
        .to_string();
    let home_score = score(game.get("score-home"));
    let away_score = score(game.get("score-vis"));
    let has_score = home_score.is_some() && away_score.is_some();
    let (state, detail) = read_status(
        game.get("gamestatus").and_then(Value::as_str),
        game.get("overtime").and_then(Value::as_str),
        has_score,
    );
    // A game that has not started shows no score, whatever the feed sends
    // (some feeds park a 0-0 there).
    let (home_score, away_score) = if state == LiveState::Scheduled {
        (None, None)
    } else {
        (home_score, away_score)
    };
    Some(LiveGame {
        natstat_id,
        state,
        home_score,
        away_score,
        detail,
    })
}

/// Every game NatStat dates `date`, with its live state.
pub async fn fetch_day(
    client: &NatStatClient,
    date: NaiveDate,
) -> Result<Vec<LiveGame>, NatStatError> {
    let range = date.format("%Y-%m-%d").to_string();
    let pages = client
        .get_all_pages("games", Some(&range), Some(LIVE_TTL_SECS))
        .await?;
    Ok(pages
        .iter()
        .flat_map(extract_results)
        .filter_map(parse_live)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live(status: &str, home: &str, vis: &str) -> LiveGame {
        parse_live(&json!({
            "id": "1516751",
            "gamestatus": status,
            "overtime": "N",
            "score-home": home,
            "score-vis": vis,
        }))
        .unwrap()
    }

    #[test]
    fn a_running_clock_reads_as_live() {
        let g = live("H2 05:34", "61", "40");
        assert_eq!(g.state, LiveState::Live);
        assert_eq!(g.detail.as_deref(), Some("5:34 2nd"));
        assert_eq!((g.home_score, g.away_score), (Some(61), Some(40)));
        assert_eq!(
            live("H1 20:00", "0", "0").detail.as_deref(),
            Some("20:00 1st")
        );
        assert_eq!(
            live("OT2 01:02", "80", "80").detail.as_deref(),
            Some("1:02 2OT")
        );
    }

    #[test]
    fn halftime_and_final() {
        let h = live("Half", "43", "29");
        assert_eq!(
            (h.state, h.detail.as_deref()),
            (LiveState::Live, Some("Halftime"))
        );
        let f = live("Final", "84", "70");
        assert_eq!(
            (f.state, f.detail.as_deref()),
            (LiveState::Final, Some("Final"))
        );
        let ot = parse_live(&json!({
            "id": "1", "gamestatus": "Final", "overtime": "Y",
            "score-home": "90", "score-vis": "88",
        }))
        .unwrap();
        assert_eq!(ot.detail.as_deref(), Some("Final/OT"));
    }

    #[test]
    fn a_game_not_started_has_no_score() {
        let g = live("", "0", "0");
        assert_eq!(g.state, LiveState::Scheduled);
        assert_eq!((g.home_score, g.away_score, g.detail), (None, None, None));
        let p = parse_live(&json!({ "id": "2", "gamestatus": "Postponed" })).unwrap();
        assert_eq!(
            (p.state, p.detail.as_deref()),
            (LiveState::Scheduled, Some("Postponed"))
        );
    }
}
