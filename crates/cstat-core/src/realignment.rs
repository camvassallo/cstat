//! Curated conference realignment for a season that hasn't been ingested yet.
//!
//! `teams.conference` is season-scoped and realignment-accurate — but only for
//! seasons we already have, because it is written by the ingest and then
//! corrected from Torvik (see [`crate::compute`]'s `TORVIK_CONF_TO_CSTAT`). The
//! Future page projects a season *before* any of that exists: on 2026-08-22
//! there is no 2027 `teams` row for anybody, so the only conference on hand is
//! last season's, and last season's is wrong for the 30 programs that moved.
//! Showing Gonzaga in the West Coast Conference for 2026-27 is not a rounding
//! error; it is the single most-discussed fact about that team's season.
//!
//! So this module carries a hand-curated diff — the moves for one target
//! season, keyed by `teams.short_name` — that the projections route lays over
//! the base-season conference. It disappears on its own: once the target season
//! is ingested, its real `teams.conference` takes precedence and the curated
//! entry stops being consulted (see `fetch_conferences` in the projections
//! route). Nothing here feeds a model; it is display + search metadata.
//!
//! # Why each entry carries `from`
//!
//! The obvious shape for this file is `{team: new_conference}`. It is also the
//! shape that fails silently. Realignment entries outlive the season they were
//! written for, and the base season underneath them gets re-ingested and
//! re-corrected; an entry whose premise has quietly changed keeps overriding
//! anyway, and an override that is wrong looks exactly like an override that is
//! right. Recording the *departure* conference makes the premise checkable:
//! [`SeasonRealignment::target_conference`] applies a move only when the base
//! season still says what the file says it says, so a stale entry degrades to
//! the DB value instead of asserting a fiction.
//!
//! The file is `include_str!`-compiled, like `data/player_display_names.json`,
//! so **editing it needs a redeploy, not a data sync**.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const REALIGNMENT_JSON: &str = include_str!("../../../data/conference_realignment.json");

/// One program changing leagues, keyed by `teams.short_name`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConferenceMove {
    /// `teams.short_name` — unique within a season and the same key the Torvik
    /// conference correction joins on.
    pub team: String,
    /// The program's conference in the *base* (prior, played) season. Verified
    /// against the DB before the move is applied; see the module docs.
    pub from: String,
    /// The conference it plays in for the target season. A cstat conference
    /// code (`PAC-12`, `UAC`, …), not a display name.
    pub to: String,
    /// Why this entry exists, when the bare from → to doesn't tell the story
    /// (a league rebrand, say, which is not a move at all).
    #[serde(default)]
    pub note: String,
}

/// A program that stops playing Division I basketball in the target season.
///
/// Distinct from a move because there is no destination conference to show: the
/// team still has a base-season roster, so it still reaches the projection, but
/// labelling it with last year's league would put a program on the board that
/// isn't in the division any more.
#[derive(Debug, Clone, Deserialize)]
pub struct DepartedProgram {
    pub team: String,
    pub from: String,
    #[serde(default)]
    pub note: String,
}

/// A program arriving from outside Division I. **Informational only** — it has
/// no prior Division I season, so it has no roster to project and never reaches
/// the Future page. Recorded so the realignment record is complete rather than
/// looking like an oversight.
#[derive(Debug, Clone, Deserialize)]
pub struct ArrivingProgram {
    pub team: String,
    pub to: String,
    #[serde(default)]
    pub note: String,
}

/// The curated realignment for one target season.
#[derive(Debug, Clone, Deserialize)]
pub struct SeasonRealignment {
    /// Provenance for the capture — the sources the moves were read off.
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub moves: Vec<ConferenceMove>,
    #[serde(default)]
    pub left_division_i: Vec<DepartedProgram>,
    #[serde(default)]
    pub new_division_i: Vec<ArrivingProgram>,
}

/// What the curated file says a team plays in for the target season.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetConference<'a> {
    /// No curated entry, or one whose `from` no longer matches the base season
    /// (stale — see the module docs). Keep whatever the DB says.
    Unchanged,
    /// Moves to `to` from `from`.
    Moved { from: &'a str, to: &'a str },
    /// Leaves Division I; there is no destination league.
    LeftDivisionI { from: &'a str },
}

impl SeasonRealignment {
    /// The target-season conference for `short_name`, given the conference the
    /// base season records for it.
    ///
    /// A curated entry applies only when `base_conference` still equals its
    /// `from`. That guard is the whole reason `from` is recorded: it turns a
    /// stale entry into a no-op instead of a confident lie.
    pub fn target_conference(
        &self,
        short_name: &str,
        base_conference: Option<&str>,
    ) -> TargetConference<'_> {
        if let Some(d) = self.left_division_i.iter().find(|d| d.team == short_name)
            && base_conference == Some(d.from.as_str())
        {
            return TargetConference::LeftDivisionI { from: &d.from };
        }
        if let Some(m) = self.moves.iter().find(|m| m.team == short_name)
            && base_conference == Some(m.from.as_str())
        {
            return TargetConference::Moved {
                from: &m.from,
                to: &m.to,
            };
        }
        TargetConference::Unchanged
    }
}

/// Every curated season, parsed once. Keyed by target season.
pub fn all() -> &'static HashMap<i32, SeasonRealignment> {
    static PARSED: OnceLock<HashMap<i32, SeasonRealignment>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let raw: HashMap<String, SeasonRealignment> = serde_json::from_str(REALIGNMENT_JSON)
            .expect("data/conference_realignment.json must be valid JSON");
        raw.into_iter()
            .map(|(k, v)| {
                let season = k.parse::<i32>().unwrap_or_else(|_| {
                    panic!("data/conference_realignment.json: key {k:?} is not a season year")
                });
                (season, v)
            })
            .collect()
    })
}

/// The curated realignment for `season`, if one has been captured.
pub fn for_season(season: i32) -> Option<&'static SeasonRealignment> {
    all().get(&season)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_parses_and_has_the_2027_capture() {
        let r = for_season(2027).expect("2027 realignment captured");
        assert!(!r.moves.is_empty());
        assert!(!r.note.is_empty(), "capture must record its sources");
    }

    #[test]
    fn a_move_applies_only_when_its_premise_still_holds() {
        let r = for_season(2027).unwrap();
        assert_eq!(
            r.target_conference("Gonzaga", Some("WCC")),
            TargetConference::Moved {
                from: "WCC",
                to: "PAC-12"
            },
        );
        // Base season disagrees with the entry's `from` — the entry is stale,
        // so it must not fire. This is the guard that keeps a re-ingest from
        // being silently overridden by a year-old capture.
        assert_eq!(
            r.target_conference("Gonzaga", Some("PAC-12")),
            TargetConference::Unchanged,
        );
        assert_eq!(
            r.target_conference("Duke", Some("ACC")),
            TargetConference::Unchanged
        );
    }

    #[test]
    fn leaving_division_i_has_no_destination() {
        let r = for_season(2027).unwrap();
        assert_eq!(
            r.target_conference("Saint Francis", Some("NEC")),
            TargetConference::LeftDivisionI { from: "NEC" },
        );
    }
}
