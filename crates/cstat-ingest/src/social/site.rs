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

    /// This season's page for a player named in a story, through the site's
    /// own player search: `(player_id, team_id)`. Only a row from `season`
    /// itself counts (the list carries a player who has not played yet over
    /// from last season, with `source_season` set), and only an exact name
    /// match, on `team` when two share the name. `None` when the player has
    /// no row this season, or the search fails: the caller links the team.
    ///
    /// Read through the API rather than the database so the ids are the ones
    /// the public pages use, wherever the bot runs.
    pub async fn find_player(
        &self,
        name: &str,
        team: Option<&str>,
        season: i32,
    ) -> Option<(Uuid, Option<Uuid>)> {
        #[derive(Deserialize)]
        struct Row {
            player_id: Uuid,
            name: String,
            team_id: Option<Uuid>,
            team_name: Option<String>,
            source_season: Option<i32>,
        }
        #[derive(Deserialize)]
        struct Page {
            players: Vec<Row>,
        }
        let mut url = reqwest::Url::parse("https://x/").expect("static URL parses");
        url.query_pairs_mut()
            .append_pair("search", name)
            .append_pair("season", &season.to_string())
            .append_pair("limit", "8");
        let page: Page = self
            .get_json(&format!("/api/players?{}", url.query().unwrap_or_default()))
            .await
            .ok()?;
        pick_player(
            page.players.into_iter().map(|r| {
                (
                    r.player_id,
                    r.name,
                    r.team_id,
                    r.team_name,
                    r.source_season.is_none(),
                )
            }),
            name,
            team,
        )
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

    /// A Predict-page link for a scheduled game, in the shape the page and its
    /// link-preview card read: both teams by name, the venue from the home
    /// side's perspective, and the season.
    pub fn predict_url(&self, home: &str, away: &str, neutral: bool, season: i32) -> String {
        format!(
            "{}/predict?{}",
            self.public_base,
            predict_query(home, away, neutral, season)
        )
    }

    /// The card a [`Site::predict_url`] link unfurls into: the URL the page's
    /// `og:image` names, so Slack previews exactly what the tweet will show.
    pub fn predict_card(&self, home: &str, away: &str, neutral: bool, season: i32) -> String {
        format!(
            "{}/og/predict.png?{}",
            self.public_base,
            predict_query(home, away, neutral, season)
        )
    }

    /// The link-preview card the schedule page for `date` unfurls into.
    pub fn schedule_card(&self, date: NaiveDate) -> String {
        format!("{}/og/schedule.png?date={date}", self.public_base)
    }

    /// The link-preview card a player page unfurls into (its `og:image`).
    pub fn player_page_card(&self, id: Uuid, season: i32) -> String {
        format!("{}/og/players/{id}.png?season={season}", self.public_base)
    }

    /// The link-preview card a team page unfurls into (its `og:image`).
    pub fn team_page_card(&self, id: Uuid, season: i32) -> String {
        format!("{}/og/teams/{id}.png?season={season}", self.public_base)
    }

    /// Card images carry the date as a cache-buster: the edge caches them for
    /// hours, and the same game's card changes once it has a final score.
    pub fn matchup_card(&self, game_id: Uuid, v: NaiveDate) -> String {
        format!("{}/og/matchup/{game_id}.png?v={v}", self.public_base)
    }
}

type SearchRow = (Uuid, String, Option<Uuid>, Option<String>, bool);

/// The one current-season row for `name`, preferring `team` when the name
/// is shared. Ambiguous or absent is `None`.
fn pick_player(
    rows: impl Iterator<Item = SearchRow>,
    name: &str,
    team: Option<&str>,
) -> Option<(Uuid, Option<Uuid>)> {
    let named: Vec<SearchRow> = rows
        .filter(|(_, n, _, _, current)| *current && n.eq_ignore_ascii_case(name))
        .collect();
    let on_team = |r: &&SearchRow| team.is_some_and(|t| r.3.as_deref() == Some(t));
    let hit = match named.iter().filter(on_team).count() {
        1 => named.iter().find(on_team),
        _ if named.len() == 1 => named.first(),
        _ => None,
    }?;
    Some((hit.0, hit.2))
}

fn predict_query(home: &str, away: &str, neutral: bool, season: i32) -> String {
    let mut q = reqwest::Url::parse("https://x/").expect("static URL parses");
    q.query_pairs_mut()
        .append_pair("home", home)
        .append_pair("away", away)
        .append_pair("venue", if neutral { "neutral" } else { "home" })
        .append_pair("season", &season.to_string());
    q.query().unwrap_or_default().to_string()
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

    #[test]
    fn players_match_by_exact_name_this_season() {
        let (a, b, t) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let row = |id, n: &str, team: &str, cur| {
            (id, n.to_string(), Some(t), Some(team.to_string()), cur)
        };
        // A carried-over row and a near-miss name do not count.
        let rows = vec![
            row(a, "Jake Davis", "Illinois", true),
            row(b, "Jake Davis", "Cal Poly", false),
            row(b, "Jake Davison", "Cal Poly", true),
        ];
        assert_eq!(
            pick_player(rows.into_iter(), "jake davis", None),
            Some((a, Some(t)))
        );
        // Two current players with the name: only the team decides.
        let rows = || {
            vec![
                row(a, "Jake Davis", "Illinois", true),
                row(b, "Jake Davis", "Cal Poly", true),
            ]
        };
        assert_eq!(
            pick_player(rows().into_iter(), "Jake Davis", Some("Cal Poly")),
            Some((b, Some(t)))
        );
        assert_eq!(pick_player(rows().into_iter(), "Jake Davis", None), None);
        assert_eq!(pick_player(std::iter::empty(), "Nobody", None), None);
    }

    #[test]
    fn predict_links_escape_names_and_match_their_card() {
        let site = Site {
            http: reqwest::Client::new(),
            api_base: CANONICAL_ORIGIN.into(),
            public_base: CANONICAL_ORIGIN.into(),
        };
        assert_eq!(
            site.predict_url("Texas A&M", "St. John's", true, 2027),
            "https://camalytics.org/predict?home=Texas+A%26M&away=St.+John%27s&venue=neutral&season=2027"
        );
        assert_eq!(
            site.predict_card("Duke", "North Carolina", false, 2027),
            "https://camalytics.org/og/predict.png?home=Duke&away=North+Carolina&venue=home&season=2027"
        );
    }
}
