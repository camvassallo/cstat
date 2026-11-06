//! Betting lines from ESPN's game summary into `espn_game_lines` (#448): the
//! record panel's primary line, with `game_lines` as its fallback and check.
//!
//! The game summary lists a book's spread, moneyline and total, with prices,
//! before and after tipoff, which the scoreboard does not. A line read after
//! the game is the closing line: the summary does not change it at the final.
//! [`crate::broadcasts`] has already matched each game to its event, so this
//! is one call per game with no name matching.
//!
//! The summary's `favoriteAtOpen` and opening `pointSpread` are not read: on
//! at least two 2026-11-02 games they name the wrong side, against the opening
//! moneyline beside them and six other book feeds.
//!
//! Best-effort: a failure leaves the panel on the `game_lines` feed.

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
    #[serde(rename = "overUnder")]
    over_under: Option<f64>,
    #[serde(rename = "overOdds")]
    over_odds: Option<f64>,
    #[serde(rename = "underOdds")]
    under_odds: Option<f64>,
    provider: Option<Provider>,
    #[serde(rename = "homeTeamOdds")]
    home: Option<TeamOdds>,
    #[serde(rename = "awayTeamOdds")]
    away: Option<TeamOdds>,
}

#[derive(Deserialize)]
struct Provider {
    name: Option<String>,
}

#[derive(Deserialize)]
struct TeamOdds {
    #[serde(default)]
    favorite: bool,
    #[serde(rename = "moneyLine")]
    money_line: Option<f64>,
    #[serde(rename = "spreadOdds")]
    spread_odds: Option<f64>,
}

/// One game's line, from `games`' home side.
#[derive(Debug, Default, PartialEq)]
struct Line {
    home_spread: Option<f64>,
    home_spread_odds: Option<i32>,
    away_spread_odds: Option<i32>,
    home_moneyline: Option<i32>,
    away_moneyline: Option<i32>,
    total: Option<f64>,
    over_odds: Option<i32>,
    under_odds: Option<i32>,
    provider: Option<String>,
}

/// American odds, which are never inside (-100, 100).
fn american(odds: Option<f64>) -> Option<i32> {
    odds.filter(|o| o.is_finite() && o.abs() >= 100.0)
        .map(|o| o.round() as i32)
}

/// A summary's line in the game's frame. `None` when it lists no market. The
/// sign of the source's `spread` is not used: the favourite flag says whose it
/// is.
fn parse_line(summary: &Summary, source_home_is_away: bool) -> Option<Line> {
    let pick = summary.pickcenter.first()?;
    let (home, away) = (pick.home.as_ref(), pick.away.as_ref());
    let source_spread = pick
        .spread
        .zip(home)
        .map(|(s, h)| if h.favorite { -s.abs() } else { s.abs() });
    let price =
        |side: Option<&TeamOdds>, read: fn(&TeamOdds) -> Option<f64>| american(side.and_then(read));
    let (home_ml, away_ml) = (price(home, |o| o.money_line), price(away, |o| o.money_line));
    let (home_so, away_so) = (
        price(home, |o| o.spread_odds),
        price(away, |o| o.spread_odds),
    );
    let total = pick.over_under.filter(|t| (80.0..=260.0).contains(t));
    if source_spread.is_none() && total.is_none() && (home_ml.is_none() || away_ml.is_none()) {
        return None;
    }
    let flip = source_home_is_away;
    Some(Line {
        home_spread: source_spread.map(|s| if flip { -s } else { s }),
        home_spread_odds: if flip { away_so } else { home_so },
        away_spread_odds: if flip { home_so } else { away_so },
        home_moneyline: if flip { away_ml } else { home_ml },
        away_moneyline: if flip { home_ml } else { away_ml },
        total,
        over_odds: american(pick.over_odds),
        under_odds: american(pick.under_odds),
        provider: pick.provider.as_ref().and_then(|p| p.name.clone()),
    })
}

/// What one run did.
#[derive(Debug, Default, Clone, Copy)]
pub struct EspnLinesReport {
    /// Games with a matched event.
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

/// Fetch every game dated `from..=to` that has a matched event.
pub async fn run(pool: &PgPool, from: NaiveDate, to: NaiveDate) -> Result<EspnLinesReport> {
    let http = reqwest::Client::builder()
        .user_agent("cstat/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the lines HTTP client")?;
    let targets = sqlx::query_as::<_, Target>(
        r#"
        SELECT g.id AS game_id, g.season, g.game_date, g.home_team_id,
               b.source_event_id, b.source_home_is_away
        FROM games g
        JOIN game_broadcasts b ON b.game_id = g.id
        WHERE g.game_date BETWEEN $1 AND $2
          AND g.home_team_id IS NOT NULL
          AND b.source_event_id IS NOT NULL
          AND b.source_home_is_away IS NOT NULL
        ORDER BY g.game_date
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("fetching games to read lines for")?;

    let mut report = EspnLinesReport {
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
                warn!(game_id = %t.game_id, error = %e, "lines fetch failed");
                report.failed += 1;
                continue;
            }
        };
        let Some(line) = parse_line(&summary, t.source_home_is_away) else {
            report.no_line += 1;
            continue;
        };
        sqlx::query(
            r#"
            INSERT INTO espn_game_lines
                (game_id, season, game_date, home_team_id, home_spread,
                 home_spread_odds, away_spread_odds, home_moneyline, away_moneyline,
                 total, over_odds, under_odds, provider)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (game_id) DO UPDATE SET
                season = EXCLUDED.season,
                game_date = EXCLUDED.game_date,
                home_team_id = EXCLUDED.home_team_id,
                home_spread = EXCLUDED.home_spread,
                home_spread_odds = EXCLUDED.home_spread_odds,
                away_spread_odds = EXCLUDED.away_spread_odds,
                home_moneyline = EXCLUDED.home_moneyline,
                away_moneyline = EXCLUDED.away_moneyline,
                total = EXCLUDED.total,
                over_odds = EXCLUDED.over_odds,
                under_odds = EXCLUDED.under_odds,
                provider = EXCLUDED.provider,
                fetched_at = now()
            "#,
        )
        .bind(t.game_id)
        .bind(t.season)
        .bind(t.game_date)
        .bind(t.home_team_id)
        .bind(line.home_spread)
        .bind(line.home_spread_odds)
        .bind(line.away_spread_odds)
        .bind(line.home_moneyline)
        .bind(line.away_moneyline)
        .bind(line.total)
        .bind(line.over_odds)
        .bind(line.under_odds)
        .bind(line.provider)
        .execute(pool)
        .await
        .context("writing a line")?;
        report.written += 1;
    }
    info!(?report, "lines refreshed");
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
        "overUnder":158.5,"overOdds":-105.0,"underOdds":-115.0,
        "homeTeamOdds":{"favorite":false,"moneyLine":3500,"spreadOdds":-108.0},
        "awayTeamOdds":{"favorite":true,"moneyLine":-20000,"spreadOdds":-112.0}}]}"#;

    #[test]
    fn reads_every_market() {
        let line = parse_line(&summary(FAMU), false).expect("has a line");
        assert_eq!(
            line,
            Line {
                home_spread: Some(28.5),
                home_spread_odds: Some(-108),
                away_spread_odds: Some(-112),
                home_moneyline: Some(3500),
                away_moneyline: Some(-20000),
                total: Some(158.5),
                over_odds: Some(-105),
                under_odds: Some(-115),
                provider: Some("DraftKings".to_string()),
            }
        );
    }

    #[test]
    fn the_favourite_flag_sets_the_spreads_sign() {
        let json = r#"{"pickcenter":[{"spread":6.5,"homeTeamOdds":{"favorite":true}}]}"#;
        let line = parse_line(&summary(json), false).expect("has a line");
        assert_eq!(line.home_spread, Some(-6.5));
    }

    #[test]
    fn a_listing_with_the_sides_reversed_flips_into_the_games_frame() {
        let line = parse_line(&summary(FAMU), true).expect("has a line");
        assert_eq!(line.home_spread, Some(-28.5));
        assert_eq!(line.home_moneyline, Some(-20000));
        assert_eq!(line.away_moneyline, Some(3500));
        assert_eq!(line.home_spread_odds, Some(-112));
        // A total has no side.
        assert_eq!(line.over_odds, Some(-105));
    }

    #[test]
    fn a_missing_market_stays_missing() {
        let json = r#"{"pickcenter":[{"spread":37.5,"homeTeamOdds":{"favorite":true},
            "awayTeamOdds":{}}]}"#;
        let line = parse_line(&summary(json), false).expect("has a line");
        assert_eq!(line.home_moneyline, None);
        assert_eq!(line.total, None);
    }

    #[test]
    fn an_impossible_price_is_dropped() {
        assert_eq!(american(Some(-108.0)), Some(-108));
        assert_eq!(american(Some(0.0)), None);
        assert_eq!(american(Some(50.0)), None);
        assert_eq!(american(None), None);
    }

    #[test]
    fn no_line_is_nothing_to_write() {
        assert!(parse_line(&summary(r#"{"pickcenter":[]}"#), false).is_none());
        assert!(parse_line(&summary("{}"), false).is_none());
        assert!(parse_line(&summary(r#"{"pickcenter":[{}]}"#), false).is_none());
    }
}
