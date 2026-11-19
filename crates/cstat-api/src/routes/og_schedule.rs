//! `GET /og/schedule.png?date=YYYY-MM-DD` — the Schedule page's link-preview
//! card: the night's five most watchable games, each with its tip time and
//! network, and our projected favourite (or the final, once played).
//!
//! Built from the same handler `/api/schedule` runs, called in-process, so
//! the card cannot disagree with the page. Fail-soft like the other cards
//! (`routes::og_image`): a date that does not parse, a night with no games, or
//! a slate that errors or times out redirects to the static `/og-image.png`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, RawQuery, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;
use serde_json::Value;

use super::og_image::{
    ACCENT, BG, CARD_H, CARD_W, FG, LIGHT, MUTED, RULE, fit_px, rasterize, truncate, xml_escape,
};
use super::schedule::{self, ScheduleParams};
use crate::AppState;

/// Games the card lists.
const ROWS: usize = 5;

/// The slate can project up to a couple dozen games live when the nightly
/// has not materialized them, so the card gets the same kind of guard as the
/// matchup card: a timeout, and a small cap on renders at once, past which a
/// request gets the static card. Any date is a distinct, cache-missing URL.
const SLATE_TIMEOUT: Duration = Duration::from_secs(12);
const CONCURRENCY: usize = 2;

fn permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    PERMITS.get_or_init(|| tokio::sync::Semaphore::new(CONCURRENCY))
}

#[derive(Deserialize, Default)]
struct DateQuery {
    date: Option<NaiveDate>,
}

/// The night a link names: its `?date=`, or tonight. A date that does not
/// parse is `None` (the static card / default head), never a 400.
pub(crate) fn requested_date(raw: Option<&str>) -> Option<NaiveDate> {
    let q: DateQuery = match raw {
        Some(r) => serde_urlencoded::from_str(r).ok()?,
        None => DateQuery::default(),
    };
    Some(q.date.unwrap_or_else(cstat_ingest::schedule_today))
}

pub async fn schedule_card(
    State(state): State<Arc<AppState>>,
    RawQuery(raw): RawQuery,
) -> Response {
    let png = match (requested_date(raw.as_deref()), permits().try_acquire()) {
        (None, _) => None,
        (Some(_), Err(_)) => {
            tracing::warn!("og schedule card: at capacity, serving the static card");
            None
        }
        (Some(date), Ok(_permit)) => {
            match tokio::time::timeout(SLATE_TIMEOUT, render(&state, date)).await {
                Ok(png) => png,
                Err(_) => {
                    tracing::warn!(%date, "og schedule card: slate timed out");
                    None
                }
            }
        }
    };
    match png {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

async fn render(state: &Arc<AppState>, date: NaiveDate) -> Option<Vec<u8>> {
    let params = ScheduleParams { date: Some(date) };
    let Json(v) = schedule::schedule(State(state.clone()), Query(params))
        .await
        .ok()?;
    let card = card_from_slate(date, &v)?;
    rasterize(&schedule_svg(&card))
}

/// One listed game.
pub(crate) struct Row {
    /// "Purdue at Gonzaga" pieces, each with its rank.
    pub away: (String, Option<i64>),
    pub home: (String, Option<i64>),
    pub neutral: bool,
    /// "7:30 PM ET · ESPN".
    pub when: String,
    /// The headline on the right: "Gonzaga 64%" before, "Purdue 81-74" after.
    pub call: Option<String>,
    /// Under it: "projected 78-71" or "Final" / "Final (OT)".
    pub call_sub: Option<String>,
    /// The favourite's win probability, for the small bar; `None` once final.
    pub prob: Option<f64>,
}

pub(crate) struct ScheduleOgCard {
    /// "Saturday, November 21".
    pub day: String,
    /// "64 games · our five most watchable".
    pub sub: String,
    pub rows: Vec<Row>,
}

pub(crate) fn card_from_slate(date: NaiveDate, v: &Value) -> Option<ScheduleOgCard> {
    let games = v["games"].as_array()?;
    if games.is_empty() {
        return None;
    }
    let mut picked: Vec<&Value> = games
        .iter()
        .filter(|g| g["home_team_name"].is_string() && g["away_team_name"].is_string())
        .collect();
    // Most watchable first; a game with no score (unprojected) after them.
    picked.sort_by(|a, b| {
        let w = |g: &Value| g["watchability"].as_f64().unwrap_or(f64::MIN);
        w(b).total_cmp(&w(a))
    });
    picked.truncate(ROWS);
    if picked.is_empty() {
        return None;
    }
    let rows = picked.into_iter().map(row).collect();
    let n = games.len();
    let sub = format!(
        "{n} game{} · our {} most watchable",
        if n == 1 { "" } else { "s" },
        match n.min(ROWS) {
            1 => "one".to_string(),
            2 => "two".to_string(),
            3 => "three".to_string(),
            4 => "four".to_string(),
            _ => "five".to_string(),
        }
    );
    Some(ScheduleOgCard {
        day: date.format("%A, %B %-d").to_string(),
        sub,
        rows,
    })
}

fn row(g: &Value) -> Row {
    let name = |k: &str| g[k].as_str().unwrap_or("TBD").to_string();
    let (home, away) = (name("home_team_name"), name("away_team_name"));
    let when = [
        g["tip_time"]
            .as_str()
            .and_then(|t| NaiveTime::parse_from_str(t, "%H:%M:%S").ok())
            .map(|t| format!("{} ET", t.format("%-I:%M %p"))),
        g["network"].as_str().map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");

    let score = |k: &str| g[k].as_i64();
    let (call, call_sub, prob) = match (score("home_score"), score("away_score")) {
        (Some(h), Some(a)) => {
            let (w, ws, ls) = if h > a { (&home, h, a) } else { (&away, a, h) };
            let ot = g["overtime"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!("Final ({s})"))
                .unwrap_or_else(|| "Final".into());
            (Some(format!("{w} {ws}-{ls}")), Some(ot), None)
        }
        _ => match g["home_win_probability"].as_f64() {
            Some(p) => {
                let (fav, fp) = if p >= 0.5 {
                    (&home, p)
                } else {
                    (&away, 1.0 - p)
                };
                let proj = match (score("predicted_home_score"), score("predicted_away_score")) {
                    (Some(h), Some(a)) => Some(format!("projected {}-{}", h.max(a), h.min(a))),
                    _ => None,
                };
                (Some(format!("{fav} {:.0}%", fp * 100.0)), proj, Some(fp))
            }
            None => (None, None, None),
        },
    };
    Row {
        away: (away, g["away_rank"].as_i64()),
        home: (home, g["home_rank"].as_i64()),
        neutral: g["is_neutral_site"].as_bool().unwrap_or(false),
        when,
        call,
        call_sub,
        prob,
    }
}

const ROW_TOP: f64 = 214.0;
const ROW_STEP: f64 = 74.0;

pub(crate) fn schedule_svg(c: &ScheduleOgCard) -> String {
    let mut out = String::new();
    for (i, r) in c.rows.iter().enumerate() {
        let top = ROW_TOP + ROW_STEP * i as f64;
        out.push_str(&format!(
            r#"<rect x="64" y="{top}" width="1072" height="1" fill="{RULE}"/>"#
        ));
        // "12 Purdue at 3 Gonzaga", ranks in the accent color and sized with the names.
        let plain = format!(
            "{} {} {} {} {}",
            r.away.1.map(|k| k.to_string()).unwrap_or_default(),
            truncate(&r.away.0, 24),
            if r.neutral { "vs" } else { "at" },
            r.home.1.map(|k| k.to_string()).unwrap_or_default(),
            truncate(&r.home.0, 24)
        );
        // Left and right columns each get a fixed width so neither can run into
        // the other on a long pair of names.
        let px = fit_px(&plain, 28.0, 640.0, 0.62);
        let side = |(n, rank): &(String, Option<i64>)| {
            let n = xml_escape(&truncate(n, 24));
            match rank {
                Some(k) => format!(
                    r#"<tspan fill="{ACCENT}" font-size="{:.1}">{k}&#160;</tspan>{n}"#,
                    px * 0.75
                ),
                None => n,
            }
        };
        out.push_str(&format!(
            r#"<text x="64" y="{:.0}" font-family="DejaVu Sans" font-weight="bold" font-size="{px:.1}" fill="{FG}">{}<tspan font-weight="normal" fill="{MUTED}">&#160;{}&#160;</tspan>{}</text>"#,
            top + 36.0,
            side(&r.away),
            if r.neutral { "vs" } else { "at" },
            side(&r.home),
        ));
        if !r.when.is_empty() {
            out.push_str(&format!(
                r#"<text x="64" y="{:.0}" font-family="DejaVu Sans" font-size="18" fill="{MUTED}">{}</text>"#,
                top + 62.0,
                xml_escape(&r.when)
            ));
        }
        if let Some(call) = &r.call {
            let call = truncate(call, 30);
            let cpx = fit_px(&call, 24.0, 380.0, 0.66);
            out.push_str(&format!(
                r#"<text x="1136" y="{:.0}" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="{cpx:.1}" fill="{LIGHT}">{}</text>"#,
                top + 36.0,
                xml_escape(&call)
            ));
        }
        if let Some(sub) = &r.call_sub {
            out.push_str(&format!(
                r#"<text x="{}" y="{:.0}" text-anchor="end" font-family="DejaVu Sans" font-size="18" fill="{MUTED}">{}</text>"#,
                if r.prob.is_some() { 976 } else { 1136 },
                top + 62.0,
                xml_escape(sub)
            ));
        }
        if let Some(p) = r.prob {
            // A 150px bar filled to the favourite's probability.
            let (bx, bw) = (986.0, 150.0);
            out.push_str(&format!(
                r#"<rect x="{bx}" y="{:.0}" width="{bw}" height="8" rx="4" fill="{RULE}"/><rect x="{bx}" y="{:.0}" width="{:.1}" height="8" rx="4" fill="{ACCENT}"/>"#,
                top + 51.0,
                top + 51.0,
                bw * p.clamp(0.0, 1.0)
            ));
        }
    }

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="12" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="64" y="78" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
  <text x="1136" y="78" text-anchor="end" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">Schedule</text>
  <text x="64" y="146" font-family="DejaVu Sans" font-weight="bold" font-size="48" fill="{FG}">{}</text>
  <text x="64" y="188" font-family="DejaVu Sans" font-size="24" fill="{MUTED}">{}</text>
  {out}
  <text x="1136" y="602" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{MUTED}">camalytics.org</text>
</svg>"##,
        xml_escape(&c.day),
        xml_escape(&c.sub),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn slate() -> Value {
        json!({ "games": [
            { "home_team_name": "Gonzaga", "away_team_name": "Purdue", "home_rank": 3, "away_rank": 12,
              "tip_time": "19:30:00", "network": "TNT", "is_neutral_site": false,
              "home_win_probability": 0.64, "predicted_home_score": 78, "predicted_away_score": 71,
              "watchability": 88.0 },
            { "home_team_name": "Duke", "away_team_name": "North Carolina", "home_rank": 1, "away_rank": 9,
              "home_score": 74, "away_score": 81, "overtime": "OT", "watchability": 91.0 },
            { "home_team_name": "Kansas", "away_team_name": "Saint Mary's", "is_neutral_site": true,
              "home_win_probability": 0.31, "watchability": 70.0 },
            { "home_team_name": "Western Carolina", "away_team_name": "Mars Hill" },
        ]})
    }

    #[test]
    fn rows_rank_by_watchability_and_read_both_states() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        let c = card_from_slate(d, &slate()).unwrap();
        assert_eq!(c.day, "Saturday, November 21");
        assert_eq!(c.sub, "4 games · our four most watchable");
        assert_eq!(c.rows[0].home.0, "Duke");
        assert_eq!(c.rows[0].call.as_deref(), Some("North Carolina 81-74"));
        assert_eq!(c.rows[0].call_sub.as_deref(), Some("Final (OT)"));
        assert_eq!(c.rows[1].call.as_deref(), Some("Gonzaga 64%"));
        assert_eq!(c.rows[1].call_sub.as_deref(), Some("projected 78-71"));
        assert_eq!(c.rows[1].when, "7:30 PM ET · TNT");
        // The away side is the favourite when the home side is under 50%.
        assert_eq!(c.rows[2].call.as_deref(), Some("Saint Mary's 69%"));
        assert!(c.rows[2].neutral);
        // Unprojected and untimed: listed last, with nothing on the right.
        assert!(c.rows[3].call.is_none() && c.rows[3].when.is_empty());
        assert!(rasterize(&schedule_svg(&c)).is_some());
    }

    #[test]
    fn an_empty_night_has_no_card() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 4).unwrap();
        assert!(card_from_slate(d, &json!({ "games": [] })).is_none());
    }

    #[test]
    fn dates_parse_leniently() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert_eq!(requested_date(Some("date=2026-11-21")), Some(d));
        assert_eq!(requested_date(Some("date=2026-11-21&v=x")), Some(d));
        assert!(requested_date(Some("date=nope")).is_none());
        assert!(requested_date(None).is_some());
    }
}
