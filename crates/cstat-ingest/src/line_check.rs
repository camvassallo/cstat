//! A second source's pregame spread into `game_line_checks` (#448).
//!
//! `game_lines` comes from one sportsbook feed, so its spread and moneyline can
//! be wrong together, and no check inside the feed can see that. ESPN's game
//! summary lists a book's line for every game, before and after tipoff, which
//! its scoreboard does not. [`crate::broadcasts`] has already matched each
//! game to its event, so this is one call per game with no name matching.
//!
//! Nothing here judges a line. The record panel compares the two
//! (`cstat_core::record::lines_disputed`), and it never reads our own
//! projection: filtering lines on that would flatter the record.
//!
//! Best-effort: a failure leaves the record as it was, on the feed's line.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

const SUMMARY_URL: &str =
    "https://site.api.espn.com/apis/site/v2/sports/basketball/mens-college-basketball/summary";

#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    pickcenter: Vec<Pick>,
}

#[derive(Deserialize)]
struct Pick {
    spread: Option<f64>,
    provider: Option<Provider>,
    #[serde(rename = "homeTeamOdds")]
    home: Option<TeamOdds>,
}

#[derive(Deserialize)]
struct Provider {
    name: Option<String>,
}

#[derive(Deserialize)]
struct TeamOdds {
    #[serde(default)]
    favorite: bool,
}

/// The spread from `games`' home side, and the book it came from. `None` with
/// no line listed. The sign of the source's `spread` is not used: it is the
/// favourite flag that says whose it is.
fn home_spread(summary: &Summary, source_home_is_away: bool) -> Option<(f64, Option<String>)> {
    let pick = summary.pickcenter.first()?;
    let magnitude = pick.spread?.abs();
    let source_home_favoured = pick.home.as_ref()?.favorite;
    let source_home = if source_home_favoured {
        -magnitude
    } else {
        magnitude
    };
    let spread = if source_home_is_away {
        -source_home
    } else {
        source_home
    };
    let book = pick.provider.as_ref().and_then(|p| p.name.clone());
    Some((spread, book))
}

/// What one run did.
#[derive(Debug, Default, Clone, Copy)]
pub struct LineCheckReport {
    /// Games with a line and a matched event.
    pub games: usize,
    pub written: usize,
    /// Games the source lists no line for.
    pub no_line: usize,
    pub failed: usize,
}

#[derive(sqlx::FromRow)]
struct Target {
    game_id: Uuid,
    season: i32,
    game_date: NaiveDate,
    home_team_id: Uuid,
    source_event_id: String,
    source_home_is_away: bool,
}

/// Check every game dated `from..=to` that has a spread and a matched event.
pub async fn run(pool: &PgPool, from: NaiveDate, to: NaiveDate) -> Result<LineCheckReport> {
    let http = reqwest::Client::builder()
        .user_agent("cstat/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the line-check HTTP client")?;
    let targets = sqlx::query_as::<_, Target>(
        r#"
        SELECT g.id AS game_id, g.season, g.game_date, g.home_team_id,
               b.source_event_id, b.source_home_is_away
        FROM games g
        JOIN game_lines l ON l.game_id = g.id
        JOIN game_broadcasts b ON b.game_id = g.id
        WHERE g.game_date BETWEEN $1 AND $2
          AND g.home_team_id IS NOT NULL
          AND l.home_spread IS NOT NULL
          AND b.source_event_id IS NOT NULL
          AND b.source_home_is_away IS NOT NULL
        ORDER BY g.game_date
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("fetching games to check")?;

    let mut report = LineCheckReport {
        games: targets.len(),
        ..Default::default()
    };
    for t in &targets {
        let fetched = async {
            http.get(SUMMARY_URL)
                .query(&[("event", t.source_event_id.as_str())])
                .send()
                .await?
                .error_for_status()?
                .json::<Summary>()
                .await
        }
        .await;
        let summary = match fetched {
            Ok(s) => s,
            Err(e) => {
                warn!(game_id = %t.game_id, error = %e, "line check fetch failed");
                report.failed += 1;
                continue;
            }
        };
        let Some((spread, book)) = home_spread(&summary, t.source_home_is_away) else {
            report.no_line += 1;
            continue;
        };
        sqlx::query(
            r#"
            INSERT INTO game_line_checks
                (game_id, season, game_date, home_team_id, home_spread, provider)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (game_id) DO UPDATE SET
                season = EXCLUDED.season,
                game_date = EXCLUDED.game_date,
                home_team_id = EXCLUDED.home_team_id,
                home_spread = EXCLUDED.home_spread,
                provider = EXCLUDED.provider,
                checked_at = now()
            "#,
        )
        .bind(t.game_id)
        .bind(t.season)
        .bind(t.game_date)
        .bind(t.home_team_id)
        .bind(spread)
        .bind(book)
        .execute(pool)
        .await
        .context("writing a line check")?;
        report.written += 1;
    }
    info!(?report, "line checks refreshed");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(json: &str) -> Summary {
        serde_json::from_str(json).expect("parses")
    }

    /// The real shape, trimmed: Florida A&M listed at home, Florida St. -28.5.
    const FAMU: &str = r#"{"pickcenter":[{"provider":{"name":"DraftKings"},"spread":28.5,
        "homeTeamOdds":{"favorite":false},"awayTeamOdds":{"favorite":true}}]}"#;

    #[test]
    fn the_favourite_flag_sets_the_sign() {
        let (spread, book) = home_spread(&summary(FAMU), false).expect("has a line");
        assert_eq!(spread, 28.5);
        assert_eq!(book.as_deref(), Some("DraftKings"));

        let home_favoured = r#"{"pickcenter":[{"spread":-6.5,"homeTeamOdds":{"favorite":true}}]}"#;
        assert_eq!(home_spread(&summary(home_favoured), false).unwrap().0, -6.5);
    }

    #[test]
    fn a_listing_with_the_sides_reversed_flips_into_the_games_frame() {
        assert_eq!(home_spread(&summary(FAMU), true).unwrap().0, -28.5);
    }

    #[test]
    fn no_line_is_nothing_to_write() {
        assert!(home_spread(&summary(r#"{"pickcenter":[]}"#), false).is_none());
        assert!(home_spread(&summary("{}"), false).is_none());
        let no_spread = r#"{"pickcenter":[{"homeTeamOdds":{"favorite":true}}]}"#;
        assert!(home_spread(&summary(no_spread), false).is_none());
    }
}
