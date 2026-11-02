//! The bot's view of the public site: it reads the same API the pages read,
//! so a draft quotes the numbers a reader will find when they click through,
//! and builds the links and card-image URLs it attaches.

use std::time::Duration;

use anyhow::Context;
use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;
use uuid::Uuid;

/// Fallback origin, matching `cstat-api`'s `routes::sitemap::CANONICAL_ORIGIN`.
const CANONICAL_ORIGIN: &str = "https://camalytics.org";

pub struct Site {
    http: reqwest::Client,
    /// Where the bot reads data from. Defaults to the public origin; point it
    /// at a local API to test against a local database.
    api_base: String,
    /// Where links and images point. Always public: Slack fetches the images
    /// itself, and the links are what gets tweeted.
    public_base: String,
}

/// One game on a night's slate, as `GET /api/schedule` serves it.
#[derive(Debug, Clone, Deserialize)]
pub struct SlateGame {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub tip_time: Option<NaiveTime>,
    pub network: Option<String>,
    pub home_team_id: Option<Uuid>,
    pub home_team_name: Option<String>,
    pub away_team_id: Option<Uuid>,
    pub away_team_name: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub is_neutral_site: bool,
    pub is_conference: Option<bool>,
    pub venue: Option<String>,
    pub overtime: Option<String>,
    pub home_rank: Option<usize>,
    pub away_rank: Option<usize>,
    pub predicted_margin: Option<f64>,
    pub home_win_probability: Option<f64>,
    pub predicted_home_score: Option<i32>,
    pub predicted_away_score: Option<i32>,
    pub prediction_basis: Option<String>,
    pub watchability: Option<f64>,
}

impl SlateGame {
    pub fn is_final(&self) -> bool {
        self.home_score.is_some() && self.away_score.is_some()
    }

    pub fn home(&self) -> &str {
        self.home_team_name.as_deref().unwrap_or("TBD")
    }

    pub fn away(&self) -> &str {
        self.away_team_name.as_deref().unwrap_or("TBD")
    }
}

#[derive(Debug, Deserialize)]
pub struct Slate {
    pub ranked_teams: usize,
    pub games: Vec<SlateGame>,
}

/// A team's row on the Future page, `GET /api/projections/{year}`. Only the
/// fields the drafts use.
#[derive(Debug, Clone, Deserialize)]
pub struct TeamProjection {
    pub team_id: Uuid,
    pub team_name: String,
    pub conference: Option<String>,
    pub midpoint_adj_em: Option<f64>,
    pub coach_name: Option<String>,
    pub coach_is_new_hc: Option<bool>,
    pub returning_count: Option<i32>,
    pub arrivals_count: Option<i32>,
    pub departures_count: Option<i32>,
    pub recruits_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct Projections {
    teams: Vec<TeamProjection>,
}

impl Site {
    pub fn from_env() -> Self {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        };
        let public_base = env("PUBLIC_BASE_URL").unwrap_or_else(|| CANONICAL_ORIGIN.to_string());
        let api_base = env("SOCIAL_API_BASE").unwrap_or_else(|| public_base.clone());
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client builds"),
            api_base,
            public_base,
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let url = format!("{}{path}", self.api_base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        anyhow::ensure!(status.is_success(), "GET {url} returned {status}");
        resp.json().await.with_context(|| format!("decoding {url}"))
    }

    pub async fn schedule(&self, date: NaiveDate) -> anyhow::Result<Slate> {
        self.get_json(&format!("/api/schedule?date={date}")).await
    }

    pub async fn projections(&self, year: i32) -> anyhow::Result<Vec<TeamProjection>> {
        Ok(self
            .get_json::<Projections>(&format!("/api/projections/{year}"))
            .await?
            .teams)
    }

    /// Whether `url` answers with an image, following redirects (a card's
    /// fallback is a redirect to the static share image, which is fine).
    pub async fn serves_image(&self, url: &str) -> bool {
        match self.http.get(url).send().await {
            Ok(r) if r.status().is_success() => r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("image/")),
            _ => false,
        }
    }

    pub fn schedule_url(&self, date: NaiveDate) -> String {
        format!("{}/schedule?date={date}", self.public_base)
    }

    pub fn team_url(&self, id: Uuid) -> String {
        format!("{}/teams/{id}", self.public_base)
    }

    pub fn player_url(&self, id: Uuid) -> String {
        format!("{}/players/{id}", self.public_base)
    }

    /// Card images carry the date as a cache-buster: the edge caches them for
    /// hours, and the same game's card changes once it has a final score.
    pub fn matchup_card(&self, game_id: Uuid, v: NaiveDate) -> String {
        format!("{}/og/matchup/{game_id}.png?v={v}", self.public_base)
    }

    pub fn radar_card(&self, player_id: Uuid, season: i32, v: NaiveDate) -> String {
        format!(
            "{}/og/radar/players/{player_id}.png?season={season}&v={v}",
            self.public_base
        )
    }

    pub fn team_card(&self, team_id: Uuid, season: i32, v: NaiveDate) -> String {
        format!(
            "{}/og/team/{team_id}.png?season={season}&v={v}",
            self.public_base
        )
    }
}

/// "7:30 PM ET" from a 24h Eastern wall clock.
pub fn format_tip(t: NaiveTime) -> String {
    format!("{} ET", t.format("%-I:%M %p"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_times_read_like_a_listing() {
        let t = NaiveTime::from_hms_opt(19, 30, 0).unwrap();
        assert_eq!(format_tip(t), "7:30 PM ET");
        let t = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert_eq!(format_tip(t), "12:00 PM ET");
    }
}
