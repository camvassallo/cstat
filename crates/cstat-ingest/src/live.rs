//! Live scores for the Schedule tab and the scorebug, from the same public
//! scoreboard the nightly's `broadcasts` step reads (`crate::broadcasts`).
//!
//! **Not NatStat.** NatStat's `games` feed carries a running score too, but on
//! the night it was tried (2026-11-02) it ran minutes behind and sat frozen for
//! four minutes straight while the scoreboard moved every game, and it did not
//! list a late Hawaii tip under that night at all. NatStat stays the source of
//! every stored result; this is display only, and never written anywhere but
//! the response cache.
//!
//! The scoreboard's games are joined to ours through `game_broadcasts`, which
//! the nightly fills: its event id, the Eastern day it is listed under, and
//! which way round the listing has the two teams (`source_home_is_away`), so a
//! neutral-site game listed the other way round still puts each score on the
//! right team.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;

use crate::broadcasts::{BroadcastClient, SourceStatus};
use crate::cache::ApiCache;

/// How long one day's scoreboard read is reused, in seconds. The source is
/// public and unmetered; this only keeps a busy page from fetching it per
/// request.
pub const LIVE_TTL_SECS: i64 = 20;

/// `api_cache` endpoint for a day's parsed live lines.
const CACHE_ENDPOINT: &str = "scoreboard/live";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    /// Not started, or postponed / cancelled (then `detail` says which).
    Scheduled,
    Live,
    Final,
}

/// One game's live line, home and away as the listing has them until
/// [`LiveGame::oriented`] puts them in ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveGame {
    pub state: LiveState,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    /// What the scoreboard prints: `2:19 2nd`, `Halftime`, `Final/OT`,
    /// `Postponed`. `None` for a game that has not started.
    pub detail: Option<String>,
}

impl LiveGame {
    /// The line in our home/away orientation, given the listing's.
    pub fn oriented(mut self, source_home_is_away: bool) -> Self {
        if source_home_is_away {
            std::mem::swap(&mut self.home_score, &mut self.away_score);
        }
        self
    }
}

/// A listing's status as a live line.
///
/// A game counts as in progress only once a score has been reported: a clock
/// at 0-0 can be the source flipping a game live at its listed start rather
/// than anything having happened, so until someone scores it still reads as
/// not started and shows its tip time.
pub fn read_status(s: &SourceStatus) -> LiveGame {
    let scored = matches!((s.home_score, s.away_score), (Some(h), Some(a)) if h + a > 0);
    let detail = s.detail.trim();
    let line = |state, detail: Option<String>| LiveGame {
        state,
        home_score: s.home_score.filter(|_| state != LiveState::Scheduled),
        away_score: s.away_score.filter(|_| state != LiveState::Scheduled),
        detail,
    };
    match s.state.as_str() {
        "post" if s.completed => line(
            LiveState::Final,
            Some(if detail.is_empty() { "Final" } else { detail }.to_string()),
        ),
        // Postponed, cancelled, suspended: over without a result.
        "post" => line(
            LiveState::Scheduled,
            (!detail.is_empty()).then(|| detail.to_string()),
        ),
        // `2:19 - 2nd` → `2:19 2nd`.
        "in" if scored => line(LiveState::Live, Some(detail.replace(" - ", " "))),
        _ => line(LiveState::Scheduled, None),
    }
}

/// Every listing on Eastern day `date`, keyed by the source's event id, in the
/// listing's own orientation. Read through the response cache, so a busy page
/// costs one fetch per [`LIVE_TTL_SECS`] across every API instance.
pub async fn fetch_day(
    pool: &PgPool,
    client: &BroadcastClient,
    date: NaiveDate,
) -> Result<HashMap<String, LiveGame>> {
    let cache = ApiCache::new(pool.clone());
    let key = date.format("%Y-%m-%d").to_string();
    if let Some(hit) = cache.get(CACHE_ENDPOINT, &key).await?
        && let Ok(lines) = serde_json::from_value(hit)
    {
        return Ok(lines);
    }
    let lines: HashMap<String, LiveGame> = client
        .fetch_day(date)
        .await?
        .into_iter()
        .map(|l| (l.event_id, read_status(&l.status)))
        .collect();
    let body = serde_json::to_value(&lines).context("serializing live lines")?;
    cache
        .set(CACHE_ENDPOINT, &key, &body, LIVE_TTL_SECS)
        .await?;
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: &str, completed: bool, detail: &str, h: i32, a: i32) -> LiveGame {
        read_status(&SourceStatus {
            state: state.into(),
            completed,
            detail: detail.into(),
            home_score: Some(h),
            away_score: Some(a),
        })
    }

    #[test]
    fn a_running_clock_reads_as_live() {
        let g = status("in", false, "2:19 - 2nd", 94, 52);
        assert_eq!(g.state, LiveState::Live);
        assert_eq!(g.detail.as_deref(), Some("2:19 2nd"));
        assert_eq!((g.home_score, g.away_score), (Some(94), Some(52)));
        assert_eq!(
            status("in", false, "3:31 - OT", 81, 79).detail.as_deref(),
            Some("3:31 OT")
        );
        assert_eq!(
            status("in", false, "Halftime", 43, 29).detail.as_deref(),
            Some("Halftime")
        );
    }

    #[test]
    fn a_clock_without_a_score_is_not_yet_live() {
        let g = status("in", false, "20:00 - 1st", 0, 0);
        assert_eq!(g.state, LiveState::Scheduled);
        assert_eq!((g.home_score, g.away_score, g.detail), (None, None, None));
    }

    #[test]
    fn final_and_postponed() {
        let f = status("post", true, "Final/OT", 90, 88);
        assert_eq!(
            (f.state, f.detail.as_deref()),
            (LiveState::Final, Some("Final/OT"))
        );
        let p = status("post", false, "Postponed", 0, 0);
        assert_eq!(
            (p.state, p.detail.as_deref(), p.home_score),
            (LiveState::Scheduled, Some("Postponed"), None)
        );
        let pre = status("pre", false, "11/3 - 7:00 PM EST", 0, 0);
        assert_eq!((pre.state, pre.detail), (LiveState::Scheduled, None));
    }

    #[test]
    fn a_reversed_listing_swaps_the_scores() {
        let g = status("in", false, "5:00 - 2nd", 60, 50).oriented(true);
        assert_eq!((g.home_score, g.away_score), (Some(50), Some(60)));
        let same = status("in", false, "5:00 - 2nd", 60, 50).oriented(false);
        assert_eq!((same.home_score, same.away_score), (Some(60), Some(50)));
    }
}
