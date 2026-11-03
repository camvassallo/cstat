//! Betting lines into `game_lines` (migration 063) from NatStat's dedicated
//! `/pointspread`, `/moneyline` and `/overunder` endpoints.
//!
//! # Why not `/forecasts`
//!
//! The lines used to ride on `/forecasts`, nested under each game's `forecast`
//! object, and [`super::elo::ingest_game_forecasts`] still reads them there.
//! From the 2026 season NatStat's forecast payload is flat (`predicted_spread`,
//! `predicted_score_home`, ...) and carries no lines at all, so that parser has
//! written NULLs since. The dedicated endpoints still serve them.
//!
//! # Three quirks this module is shaped around
//!
//! - **Pages are partial.** A page covers the next 100 *games* in the range
//!   and lists only the ones with a line, so a page can be short or empty with
//!   more to come. A season-wide range is newest-first, so in November its
//!   first page is March and empty. [`fetch_range`] pages by `results-total`
//!   rather than stopping at the first empty page, as
//!   [`crate::NatStatClient::get_all_pages`] does.
//! - **The favourite is a numeric team id**, NatStat's internal one from the
//!   `/teams` payload, not the team code every other field uses, and the
//!   spread is always the favourite's (negative). Without resolving that id
//!   there is no way to say which side a spread belongs to.
//! - **Stale quotes.** A game can arrive with an `as_of` a year before it is
//!   played (2026-11-04 Boise State, quoted 2025-11-03, with the moneyline
//!   favouring the NAIA visitor). A quote more than [`MAX_QUOTE_AGE_DAYS`]
//!   older than its game is dropped. On the 2026-27 opening schedule that is
//!   135 of ~520 quotes, some from 2019.
//! - **Moneylines with the sides swapped.** In 31 of 4,005 graded 2026 games
//!   the moneyline favoured the other team from the spread's favourite. The
//!   spread was the right one: its favourite won 58% of those games, and its
//!   error was smaller than across the agreeing games (6.8 points against
//!   9.0), where a sign error would have doubled it. A moneyline that
//!   contradicts the spread is dropped ([`moneyline_contradicts`]).

use super::utils::{parse_f64, parse_i32};
use crate::NatStatClient;
use crate::client::NatStatError;
use crate::extract_results;
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

/// A quote older than this, relative to its game, is a leftover rather than a
/// line for the game.
const MAX_QUOTE_AGE_DAYS: i64 = 14;

/// Safety cap on pages per endpoint per call. A full season is ~63.
const MAX_PAGES: u64 = 200;

/// What one run wrote and dropped.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinesReport {
    /// Games with at least one market written.
    pub games: usize,
    pub spreads: usize,
    pub moneylines: usize,
    pub totals: usize,
    /// Quotes for a game not in `games` (non-Division I opponents, mostly).
    pub unmatched: usize,
    /// Quotes dropped as stale, implausible or with no resolvable favourite.
    pub rejected: usize,
}

/// One quote as the feed states it, before it is matched to a game.
#[derive(Debug, Clone, PartialEq)]
struct Quote {
    game_code: String,
    game_day: NaiveDate,
    home_code: String,
    visitor_code: String,
    as_of: NaiveDateTime,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn parse_quote(v: &Value) -> Option<Quote> {
    let game_day = NaiveDate::parse_from_str(str_field(v, "gameday")?, "%Y-%m-%d").ok()?;
    let as_of = NaiveDateTime::parse_from_str(str_field(v, "as_of")?, "%Y-%m-%d %H:%M:%S").ok()?;
    if (game_day - as_of.date()).num_days() > MAX_QUOTE_AGE_DAYS {
        return None;
    }
    Some(Quote {
        game_code: str_field(v, "game-code")?.to_string(),
        game_day,
        home_code: str_field(v, "home-code").unwrap_or("").to_string(),
        visitor_code: str_field(v, "visitor-code").unwrap_or("").to_string(),
        as_of,
    })
}

/// A spread entry: the favourite's id and the (always non-positive) spread.
fn parse_spread(v: &Value) -> Option<(Quote, String, f64)> {
    let q = parse_quote(v)?;
    let spread = v.get("spread").and_then(parse_f64)?;
    if !spread.is_finite() || spread.abs() > 60.0 {
        return None;
    }
    let fav = str_field(v, "favorite_id")?.to_string();
    Some((q, fav, spread))
}

/// A moneyline entry: (home odds, visitor odds), American format.
fn parse_moneyline(v: &Value) -> Option<(Quote, i32, i32)> {
    let q = parse_quote(v)?;
    let home = v.get("home").and_then(parse_i32)?;
    let visitor = v.get("visitor").and_then(parse_i32)?;
    // American odds are never inside (-100, 100).
    if home.abs() < 100 || visitor.abs() < 100 {
        return None;
    }
    Some((q, home, visitor))
}

fn parse_total(v: &Value) -> Option<(Quote, f64)> {
    let q = parse_quote(v)?;
    let total = v.get("total").and_then(parse_f64)?;
    if !(80.0..=260.0).contains(&total) {
        return None;
    }
    Some((q, total))
}

/// The spread from the feed's home side, given which side is favoured.
/// `None` when neither the favourite id nor the moneyline settles it.
fn feed_home_spread(
    spread: f64,
    favorite_code: Option<&str>,
    q: &Quote,
    moneyline: Option<(i32, i32)>,
) -> Option<f64> {
    let magnitude = spread.abs();
    if magnitude == 0.0 {
        return Some(0.0);
    }
    let home_favoured = match favorite_code {
        Some(c) if !c.is_empty() && c == q.home_code => Some(true),
        Some(c) if !c.is_empty() && c == q.visitor_code => Some(false),
        _ => match moneyline {
            Some((h, v)) if h != v => Some(h < v),
            _ => None,
        },
    }?;
    Some(if home_favoured { -magnitude } else { magnitude })
}

/// Does a moneyline favour the other side from the spread? A pick'em on
/// either market contradicts nothing.
fn moneyline_contradicts(home_spread: f64, home_ml: i32, away_ml: i32) -> bool {
    home_spread != 0.0 && home_ml != away_ml && (home_spread < 0.0) != (home_ml < away_ml)
}

/// Fetch every page of `endpoint` over `range`, paging by `results-total`.
async fn fetch_range(
    client: &NatStatClient,
    endpoint: &str,
    range: &str,
    ttl: Option<i64>,
) -> Result<Vec<Value>, NatStatError> {
    let mut pages = Vec::new();
    let mut offset: u64 = 0;
    let mut total: Option<u64> = None;
    for _ in 0..MAX_PAGES {
        let off = (offset > 0).then_some(offset);
        let page = match client.get(endpoint, Some(range), off, ttl).await {
            Ok(p) => p,
            // NO_DATA is NatStat's "nothing here". On a later page of a range
            // whose total says more is coming, it is just a page of games
            // without lines.
            Err(NatStatError::ApiError { code, .. }) if code == "NO_DATA" => match total {
                Some(t) if offset + 100 < t => {
                    offset += 100;
                    continue;
                }
                _ => break,
            },
            Err(e) => return Err(e),
        };
        let meta = NatStatClient::parse_meta(&page);
        let step = meta.results_max.unwrap_or(100).max(1);
        total = total.or(meta.results_total);
        pages.push(page);
        offset += step;
        match total {
            Some(t) if offset < t => {}
            _ => break,
        }
    }
    Ok(pages)
}

/// The game a quote is for.
struct GameRef {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
}

struct Merged {
    /// The team the feed lists at home, the frame every value is stated in.
    home_team_id: Uuid,
    home_spread: Option<f64>,
    spread_as_of: Option<NaiveDateTime>,
    home_moneyline: Option<i32>,
    away_moneyline: Option<i32>,
    moneyline_as_of: Option<NaiveDateTime>,
    total: Option<f64>,
    total_as_of: Option<NaiveDateTime>,
}

/// Ingest lines for games of `season` played from `from` to `to` inclusive.
///
/// `ttl` is the response-cache lifetime: short for the nightly's forward window,
/// whose quotes move during the day, and the default for a historical backfill.
pub async fn ingest_lines(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    from: NaiveDate,
    to: NaiveDate,
    ttl: Option<i64>,
) -> Result<LinesReport, NatStatError> {
    let range = format!("{},{}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d"));
    let mut report = LinesReport::default();

    // NatStat's numeric team id -> team code. Stable across seasons (348 is
    // Auburn in both 2026 and 2027), but read for the season in hand so a
    // newly-added program resolves.
    let mut id_to_code: HashMap<String, String> = HashMap::new();
    for page in client
        .get_all_pages("teams", Some(&season.to_string()), None)
        .await?
    {
        for t in extract_results(&page) {
            if let (Some(id), Some(code)) = (str_field(t, "id"), str_field(t, "code")) {
                id_to_code.insert(id.to_string(), code.to_string());
            }
        }
    }

    let code_to_team: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT natstat_id, id FROM teams WHERE season = $1 AND natstat_id IS NOT NULL",
    )
    .bind(season)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let games: HashMap<String, GameRef> =
        sqlx::query_as::<_, (String, Uuid, NaiveDate, Uuid, Uuid)>(
            "SELECT natstat_id, id, game_date, home_team_id, away_team_id FROM games \
             WHERE season = $1 AND natstat_id IS NOT NULL AND game_date IS NOT NULL \
               AND home_team_id IS NOT NULL AND away_team_id IS NOT NULL",
        )
        .bind(season)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(code, id, game_date, home_team_id, away_team_id)| {
            (
                code,
                GameRef {
                    id,
                    game_date,
                    home_team_id,
                    away_team_id,
                },
            )
        })
        .collect();

    // The team the feed lists at home. Values are stored in the feed's frame
    // with this id beside them, and put in the `games` row's frame on read.
    // Orienting at write time would go wrong the night `correct_swapped_games`
    // relabels a game: this step runs before `compute_all`, so the line would
    // be stored against the pre-swap sides, and by the next night the game is
    // outside the window and the inversion would be permanent.
    let feed_home = |q: &Quote, g: &GameRef| -> Uuid {
        let home = code_to_team.get(&q.home_code);
        let visitor = code_to_team.get(&q.visitor_code);
        if home == Some(&g.away_team_id) || visitor == Some(&g.home_team_id) {
            g.away_team_id
        } else {
            g.home_team_id
        }
    };
    let blank = |home_team_id: Uuid| Merged {
        home_team_id,
        home_spread: None,
        spread_as_of: None,
        home_moneyline: None,
        away_moneyline: None,
        moneyline_as_of: None,
        total: None,
        total_as_of: None,
    };

    let spread_pages = fetch_range(client, "pointspread", &range, ttl).await?;
    let moneyline_pages = fetch_range(client, "moneyline", &range, ttl).await?;
    let total_pages = fetch_range(client, "overunder", &range, ttl).await?;

    let mut merged: HashMap<Uuid, Merged> = HashMap::new();
    // Feed-oriented moneylines by game code, the fallback that settles a
    // spread whose favourite id does not resolve.
    let mut feed_moneylines: HashMap<String, (i32, i32)> = HashMap::new();

    // A later quote replaces an earlier one within a run, as it does across
    // runs in the upsert.
    fn newer(slot: Option<NaiveDateTime>, as_of: NaiveDateTime) -> bool {
        slot.is_none_or(|s| as_of >= s)
    }

    for entry in moneyline_pages.iter().flat_map(extract_results) {
        let Some((q, home, visitor)) = parse_moneyline(entry) else {
            report.rejected += 1;
            continue;
        };
        let Some(g) = games.get(&q.game_code) else {
            report.unmatched += 1;
            continue;
        };
        feed_moneylines.insert(q.game_code.clone(), (home, visitor));
        let m = merged
            .entry(g.id)
            .or_insert_with(|| blank(feed_home(&q, g)));
        if newer(m.moneyline_as_of, q.as_of) {
            m.home_moneyline = Some(home);
            m.away_moneyline = Some(visitor);
            m.moneyline_as_of = Some(q.as_of);
        }
    }

    for entry in spread_pages.iter().flat_map(extract_results) {
        let Some((q, fav_id, spread)) = parse_spread(entry) else {
            report.rejected += 1;
            continue;
        };
        let Some(g) = games.get(&q.game_code) else {
            report.unmatched += 1;
            continue;
        };
        let fav_code = id_to_code.get(&fav_id).map(String::as_str);
        let Some(home_spread) = feed_home_spread(
            spread,
            fav_code,
            &q,
            feed_moneylines.get(&q.game_code).copied(),
        ) else {
            report.rejected += 1;
            continue;
        };
        let m = merged
            .entry(g.id)
            .or_insert_with(|| blank(feed_home(&q, g)));
        if newer(m.spread_as_of, q.as_of) {
            m.home_spread = Some(home_spread);
            m.spread_as_of = Some(q.as_of);
        }
    }

    for entry in total_pages.iter().flat_map(extract_results) {
        let Some((q, total)) = parse_total(entry) else {
            report.rejected += 1;
            continue;
        };
        let Some(g) = games.get(&q.game_code) else {
            report.unmatched += 1;
            continue;
        };
        let m = merged
            .entry(g.id)
            .or_insert_with(|| blank(feed_home(&q, g)));
        if newer(m.total_as_of, q.as_of) {
            m.total = Some(total);
            m.total_as_of = Some(q.as_of);
        }
    }

    for m in merged.values_mut() {
        if let (Some(s), Some(h), Some(a)) = (m.home_spread, m.home_moneyline, m.away_moneyline)
            && moneyline_contradicts(s, h, a)
        {
            m.home_moneyline = None;
            m.away_moneyline = None;
            m.moneyline_as_of = None;
            report.rejected += 1;
        }
    }

    let game_dates: HashMap<Uuid, NaiveDate> =
        games.values().map(|g| (g.id, g.game_date)).collect();

    let mut ids = Vec::with_capacity(merged.len());
    let mut dates = Vec::with_capacity(merged.len());
    let mut home_ids = Vec::with_capacity(merged.len());
    let mut spreads = Vec::with_capacity(merged.len());
    let mut spread_as_ofs = Vec::with_capacity(merged.len());
    let mut home_mls = Vec::with_capacity(merged.len());
    let mut away_mls = Vec::with_capacity(merged.len());
    let mut ml_as_ofs = Vec::with_capacity(merged.len());
    let mut totals = Vec::with_capacity(merged.len());
    let mut total_as_ofs = Vec::with_capacity(merged.len());
    for (id, m) in &merged {
        ids.push(*id);
        dates.push(game_dates[id]);
        home_ids.push(m.home_team_id);
        spreads.push(m.home_spread);
        spread_as_ofs.push(m.spread_as_of);
        home_mls.push(m.home_moneyline);
        away_mls.push(m.away_moneyline);
        ml_as_ofs.push(m.moneyline_as_of);
        totals.push(m.total);
        total_as_ofs.push(m.total_as_of);
        report.spreads += m.home_spread.is_some() as usize;
        report.moneylines += m.home_moneyline.is_some() as usize;
        report.totals += m.total.is_some() as usize;
    }
    report.games = ids.len();

    if !ids.is_empty() {
        // A stored row in another frame than this run's (the feed's home side
        // resolved differently, which needs an unresolvable team code AND a
        // relabelled game in between) cannot be merged market by market: its
        // values and this run's would be stated from different sides. This
        // run's quotes replace it whole.
        sqlx::query(
            "DELETE FROM game_lines l USING UNNEST($1::uuid[], $2::uuid[]) AS u(g, h) \
             WHERE l.game_id = u.g AND l.home_team_id <> u.h",
        )
        .bind(&ids)
        .bind(&home_ids)
        .execute(pool)
        .await?;

        // Each market keeps whichever quote is newer, the stored one or this
        // run's, so a stale cached page or an overlapping backfill cannot roll
        // a line back. A market this run did not see leaves the stored one.
        sqlx::query(
            r#"
            INSERT INTO game_lines
                (game_id, season, game_date, home_team_id, home_spread, spread_as_of,
                 home_moneyline, away_moneyline, moneyline_as_of, total, total_as_of, updated_at)
            SELECT g, $2, d, h, s, sa, hm, am, ma, t, ta, now()
            FROM UNNEST(
                $1::uuid[], $3::date[], $4::float8[], $5::timestamp[],
                $6::int[], $7::int[], $8::timestamp[], $9::float8[], $10::timestamp[],
                $11::uuid[]
            ) AS u(g, d, s, sa, hm, am, ma, t, ta, h)
            ON CONFLICT (game_id) DO UPDATE SET
                season = EXCLUDED.season,
                game_date = EXCLUDED.game_date,
                home_spread = CASE WHEN EXCLUDED.spread_as_of >= game_lines.spread_as_of
                                     OR game_lines.spread_as_of IS NULL
                                   THEN COALESCE(EXCLUDED.home_spread, game_lines.home_spread)
                                   ELSE game_lines.home_spread END,
                spread_as_of = GREATEST(EXCLUDED.spread_as_of, game_lines.spread_as_of),
                home_moneyline = CASE WHEN EXCLUDED.moneyline_as_of >= game_lines.moneyline_as_of
                                        OR game_lines.moneyline_as_of IS NULL
                                      THEN COALESCE(EXCLUDED.home_moneyline, game_lines.home_moneyline)
                                      ELSE game_lines.home_moneyline END,
                away_moneyline = CASE WHEN EXCLUDED.moneyline_as_of >= game_lines.moneyline_as_of
                                        OR game_lines.moneyline_as_of IS NULL
                                      THEN COALESCE(EXCLUDED.away_moneyline, game_lines.away_moneyline)
                                      ELSE game_lines.away_moneyline END,
                moneyline_as_of = GREATEST(EXCLUDED.moneyline_as_of, game_lines.moneyline_as_of),
                total = CASE WHEN EXCLUDED.total_as_of >= game_lines.total_as_of
                               OR game_lines.total_as_of IS NULL
                             THEN COALESCE(EXCLUDED.total, game_lines.total)
                             ELSE game_lines.total END,
                total_as_of = GREATEST(EXCLUDED.total_as_of, game_lines.total_as_of),
                updated_at = now()
            "#,
        )
        .bind(&ids)
        .bind(season)
        .bind(&dates)
        .bind(&spreads)
        .bind(&spread_as_ofs)
        .bind(&home_mls)
        .bind(&away_mls)
        .bind(&ml_as_ofs)
        .bind(&totals)
        .bind(&total_as_ofs)
        .bind(&home_ids)
        .execute(pool)
        .await?;
    }

    if report.rejected > 0 {
        warn!(
            season,
            rejected = report.rejected,
            "dropped stale or unresolvable line quotes"
        );
    }
    info!(
        season,
        range = %range,
        games = report.games,
        spreads = report.spreads,
        moneylines = report.moneylines,
        totals = report.totals,
        unmatched = report.unmatched,
        "ingested betting lines"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quote(home: &str, visitor: &str) -> Quote {
        Quote {
            game_code: "1".into(),
            game_day: NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
            home_code: home.into(),
            visitor_code: visitor.into(),
            as_of: NaiveDate::from_ymd_opt(2026, 4, 5)
                .unwrap()
                .and_hms_opt(0, 15, 8)
                .unwrap(),
        }
    }

    #[test]
    fn spread_follows_the_resolved_favourite() {
        let q = quote("AUB", "TULS");
        assert_eq!(feed_home_spread(-4.5, Some("AUB"), &q, None), Some(-4.5));
        assert_eq!(feed_home_spread(-4.5, Some("TULS"), &q, None), Some(4.5));
    }

    #[test]
    fn unresolved_favourite_falls_back_to_the_moneyline_or_gives_up() {
        let q = quote("AUB", "TULS");
        assert_eq!(
            feed_home_spread(-4.5, None, &q, Some((-220, 180))),
            Some(-4.5)
        );
        assert_eq!(
            feed_home_spread(-4.5, Some("XYZ"), &q, Some((180, -220))),
            Some(4.5)
        );
        assert_eq!(feed_home_spread(-4.5, None, &q, None), None);
        assert_eq!(feed_home_spread(-4.5, None, &q, Some((-110, -110))), None);
        // A pick'em needs no favourite.
        assert_eq!(feed_home_spread(0.0, None, &q, None), Some(0.0));
    }

    #[test]
    fn an_empty_code_never_matches_a_favourite() {
        // A non-Division I side arrives with a blank code; a blank favourite
        // must not read as that side.
        let q = quote("BOIS", "");
        assert_eq!(feed_home_spread(-9.5, Some(""), &q, None), None);
    }

    #[test]
    fn a_moneyline_against_the_spread_is_a_contradiction() {
        // 2026-11-02 High Point (home) -7.5, moneyline favouring Liberty.
        assert!(moneyline_contradicts(-7.5, 244, -333));
        assert!(!moneyline_contradicts(-7.5, -333, 244));
        assert!(!moneyline_contradicts(3.5, 140, -181));
        assert!(!moneyline_contradicts(0.0, 140, -181));
        assert!(!moneyline_contradicts(-1.5, -110, -110));
    }

    #[test]
    fn parses_the_feed_shapes() {
        let spread = json!({"game-code": "1510796", "gameday": "2026-04-05", "visitor-code": "TULS",
            "home-code": "AUB", "spread": "-4.5", "favorite_id": "348", "as_of": "2026-04-05 00:15:08"});
        let (q, fav, s) = parse_spread(&spread).unwrap();
        assert_eq!(
            (q.game_code.as_str(), fav.as_str(), s),
            ("1510796", "348", -4.5)
        );

        let ml = json!({"game-code": "1510796", "gameday": "2026-04-05", "visitor": "+180",
            "visitor-code": "TULS", "home": "-220", "home-code": "AUB", "as_of": "2026-04-05 00:15:08"});
        let (_, h, v) = parse_moneyline(&ml).unwrap();
        assert_eq!((h, v), (-220, 180));

        let total = json!({"game-code": "1515691", "gameday": "2026-11-03", "home-code": "LSU",
            "visitor-code": "UNO", "total": "159", "as_of": "2026-11-03 00:09:08"});
        assert_eq!(parse_total(&total).unwrap().1, 159.0);
    }

    #[test]
    fn drops_stale_and_implausible_quotes() {
        // Quoted a year before the game.
        let stale = json!({"game-code": "1515204", "gameday": "2026-11-04", "home-code": "BOIS",
            "visitor-code": "", "spread": "-9.5", "favorite_id": "1068", "as_of": "2025-11-03 16:30:30"});
        assert!(parse_spread(&stale).is_none());

        let bad_ml = json!({"game-code": "1", "gameday": "2026-04-05", "home": "-50", "visitor": "+40",
            "as_of": "2026-04-05 00:15:08"});
        assert!(parse_moneyline(&bad_ml).is_none());

        let bad_total = json!({"game-code": "1", "gameday": "2026-04-05", "total": "12",
            "as_of": "2026-04-05 00:15:08"});
        assert!(parse_total(&bad_total).is_none());
    }
}
