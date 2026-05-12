//! 247Sports composite recruit-rankings HTML client.
//!
//! Sister to [`crate::tfs::TfsClient`]. Same JWT plumbing, same rate limiter,
//! same retry/backoff — but the recruit endpoint returns gzipped HTML
//! fragments instead of JSON, so it lives in its own module with its own
//! [`scraper`]-based row parser.
//!
//! URL pattern (recovered from browser DevTools):
//! ```text
//! https://247sports.com/season/{YEAR}-basketball/compositerecruitrankings/
//!   ?ViewPath=~%2FViews%2FSkyNet%2FPlayerSportRanking%2F_SimpleSetForSeason.ascx
//!   &InstitutionGroup={highschool|juco|prep}
//!   &Page={N}
//! ```
//! ~50 rows per page; iterate `Page` 1..N until the parser returns an empty
//! row vector (247 serves an empty fragment past the last data page; no
//! `pageCount` field).
//!
//! The client is JSON-free by design — see [`parse_recruits_html`] for the
//! CSS-selector layout the parser is keyed to.

use crate::rate_limiter::RateLimiter;
use regex::Regex;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const RECRUITS_URL_BASE: &str = "https://247sports.com/season";
const RECRUITS_VIEW_PATH: &str = "~/Views/SkyNet/PlayerSportRanking/_SimpleSetForSeason.ascx";
const USER_AGENT: &str = "cstat-ingest/0.1 (+https://campom.org)";

/// Same self-imposed politeness ceiling as [`crate::tfs::TfsClient`]: 1 req/sec
/// by default, overridable via `TFS_247_RATE_PER_HOUR`. The two clients share
/// the env var because they hit the same vendor; in practice they don't run
/// concurrently so the limiter doesn't need to be shared at the process level.
const DEFAULT_RATE_PER_HOUR: u32 = 3_600;

const MAX_RETRIES: u32 = 4;

#[derive(Debug, Error)]
pub enum RecruitError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error(
        "missing TFS_247_JWT environment variable — capture a fresh JWT from DevTools and export it"
    )]
    MissingJwt,

    #[error(
        "JWT rejected by 247 (HTTP {status}). Token expires ~6h after issue; re-capture from DevTools"
    )]
    JwtExpired { status: u16 },

    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
}

/// Which 247 composite-ranking view to scrape.
///
/// Empirically (verified against class-of-2026), the `compositerecruitrankings`
/// endpoint returns **identical content** for all three `InstitutionGroup`
/// values when called with only the subscriber `JWT` cookie — the filter
/// likely depends on session state we don't carry. The CLI therefore defaults
/// to `HighSchool` only; the `Juco` / `Prep` variants are kept so the schema
/// vocab is ready for when we find the right endpoint(s) for those cohorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstitutionGroup {
    HighSchool,
    Juco,
    Prep,
}

impl InstitutionGroup {
    /// Match the `InstitutionGroup=` URL param vocab (lowercase, no spaces).
    pub fn as_url_param(self) -> &'static str {
        match self {
            Self::HighSchool => "highschool",
            Self::Juco => "juco",
            Self::Prep => "prep",
        }
    }

    /// The string written into the `recruits.institution_group` column.
    /// Identical to the URL param today; kept as a separate method so we can
    /// diverge later without breaking the URL builder.
    pub fn as_db_value(self) -> &'static str {
        self.as_url_param()
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "highschool" | "high_school" | "hs" => Some(Self::HighSchool),
            "juco" => Some(Self::Juco),
            "prep" => Some(Self::Prep),
            _ => None,
        }
    }
}

/// One page of recruit rankings.
#[derive(Debug, Clone)]
pub struct RecruitPage {
    pub players: Vec<RecruitRow>,
    /// True when the parser found zero rows — caller should stop paging.
    /// 247 doesn't publish a page count or empty-marker; an empty row vector
    /// is the only reliable signal that we walked past the last data page.
    pub is_last_page: bool,
}

/// One recruit row, parsed from `<li class="rankings-page__list-item">`.
///
/// All fields are `Option` so a partial row (uncommitted player, missing
/// metrics, etc.) lands intact rather than getting dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecruitRow {
    pub recruit_key: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub composite_rank: Option<i32>,
    pub previous_rank: Option<i32>,
    pub composite_rating: Option<f32>,
    pub star_rating: Option<i16>,
    pub position_rank: Option<i32>,
    pub state_rank: Option<i32>,
    pub position: Option<String>,
    pub height: Option<String>,
    pub weight: Option<i32>,
    pub high_school: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub committed_school: Option<String>,
    pub committed_school_slug: Option<String>,
    /// "Signed" / "Committed" / "Uncommitted" — derived from row markers, not
    /// a direct field on the HTML. See [`parse_recruits_html`] for the rules.
    pub commit_status: Option<String>,
    pub profile_url: Option<String>,
    pub photo_url: Option<String>,
    /// The raw `<li>` HTML, preserved for forensics. Lives in `raw_player`
    /// alongside the parsed fields so a parser bug doesn't require a re-scrape.
    pub raw_html: String,
}

/// 247Sports recruit-rankings HTML client. Mirrors [`crate::tfs::TfsClient`]
/// in shape but parses HTML rather than JSON.
pub struct Recruit247Client {
    http: Client,
    jwt: String,
    rate_limiter: RateLimiter,
}

impl Recruit247Client {
    /// Build a client from `TFS_247_JWT` (required) and optional
    /// `TFS_247_RATE_PER_HOUR` (default 3600). Same env vars as `TfsClient` —
    /// both clients hit the same vendor and accept the same subscriber JWT.
    pub fn from_env() -> Result<Self, RecruitError> {
        let jwt = std::env::var("TFS_247_JWT").map_err(|_| RecruitError::MissingJwt)?;
        let rate = std::env::var("TFS_247_RATE_PER_HOUR")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(DEFAULT_RATE_PER_HOUR);
        Ok(Self::new(jwt, rate))
    }

    /// Build with an explicit JWT + rate (handy for tests). Prefer
    /// [`Self::from_env`] in production code.
    pub fn new(jwt: String, max_per_hour: u32) -> Self {
        Self {
            http: Client::builder()
                .user_agent(USER_AGENT)
                .gzip(true)
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            jwt,
            rate_limiter: RateLimiter::new(max_per_hour),
        }
    }

    /// Fetch a single page of recruit rankings. Page is 1-indexed.
    pub async fn fetch_page(
        &self,
        year: i32,
        group: InstitutionGroup,
        page: u32,
    ) -> Result<RecruitPage, RecruitError> {
        let url = format!(
            "{RECRUITS_URL_BASE}/{year}-basketball/compositerecruitrankings/\
             ?ViewPath={view}&InstitutionGroup={grp}&Page={page}",
            view = urlencoding(RECRUITS_VIEW_PATH),
            grp = group.as_url_param(),
        );
        let referer = format!(
            "https://247sports.com/season/{year}-basketball/compositerecruitrankings/?InstitutionGroup={}",
            group.as_url_param()
        );

        let mut attempt: u32 = 0;
        loop {
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                let backoff_secs = 2u64.pow(attempt);
                info!(
                    year,
                    ?group,
                    page,
                    attempt,
                    backoff_secs,
                    "retrying 247 recruits request"
                );
            } else {
                info!(year, ?group, page, "fetching 247 recruits page");
            }

            let response = match self
                .http
                .get(&url)
                .header("Cookie", format!("JWT={}", self.jwt))
                .header("Referer", &referer)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Accept", "*/*")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(RecruitError::Http(e));
                    }
                    warn!(year, page, attempt, error = %e, "network error — retrying");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    attempt += 1;
                    continue;
                }
            };

            let status = response.status();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(RecruitError::JwtExpired {
                    status: status.as_u16(),
                });
            }

            if status.as_u16() == 429 || status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                if attempt >= MAX_RETRIES {
                    return Err(RecruitError::HttpStatus {
                        status: status.as_u16(),
                        body,
                    });
                }
                let backoff_secs = 2u64.pow(attempt);
                warn!(
                    year,
                    page,
                    attempt,
                    status = status.as_u16(),
                    backoff_secs,
                    "retryable HTTP error — backing off"
                );
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                attempt += 1;
                continue;
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(RecruitError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });
            }

            let body = response.text().await?;
            let players = parse_recruits_html(&body);
            return Ok(RecruitPage {
                is_last_page: players.is_empty(),
                players,
            });
        }
    }
}

/// Minimal URL-encoder for the `ViewPath` query parameter. Avoids pulling in a
/// full URL-building crate just to encode `~/Views/.../File.ascx` → `~%2FViews%2F...%2FFile.ascx`.
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// HTML parser
// ---------------------------------------------------------------------------

/// CSS selectors lazily compiled once per process. `Selector::parse` returns a
/// `Result<_, SelectorErrorKind<'_>>` with a non-Send lifetime, so it can't go
/// in a normal `static` — use `OnceLock` to compile on first access.
struct Selectors {
    item: Selector,
    showmore: Selector,
    rank_primary: Selector,
    rank_other: Selector,
    name_link: Selector,
    meta: Selector,
    position: Selector,
    metrics: Selector,
    star_yellow: Selector,
    score: Selector,
    natrank: Selector,
    posrank: Selector,
    sttrank: Selector,
    status_img_link: Selector,
    status_img: Selector,
    status_bare_img: Selector,
    status_checkmark: Selector,
    status_crystal_ball: Selector,
    photo_img: Selector,
}

fn selectors() -> &'static Selectors {
    static SEL: OnceLock<Selectors> = OnceLock::new();
    SEL.get_or_init(|| Selectors {
        // Exclude the trailing "show more" sentinel <li> 247 appends to each page.
        item: Selector::parse("li.rankings-page__list-item:not(.showmore_blk)").unwrap(),
        showmore: Selector::parse("li.showmore_blk").unwrap(),
        rank_primary: Selector::parse(".rank-column .primary").unwrap(),
        rank_other: Selector::parse(".rank-column .other").unwrap(),
        name_link: Selector::parse("a.rankings-page__name-link").unwrap(),
        meta: Selector::parse(".recruit .meta").unwrap(),
        position: Selector::parse(".position").unwrap(),
        metrics: Selector::parse(".metrics").unwrap(),
        star_yellow: Selector::parse(".rankings-page__star-and-score .icon-starsolid.yellow")
            .unwrap(),
        score: Selector::parse(".rankings-page__star-and-score .score").unwrap(),
        natrank: Selector::parse(".rank .natrank").unwrap(),
        posrank: Selector::parse(".rank .posrank").unwrap(),
        sttrank: Selector::parse(".rank .sttrank").unwrap(),
        status_img_link: Selector::parse(".status a.img-link").unwrap(),
        status_img: Selector::parse("img").unwrap(),
        // Direct-child `<img>` of `.status` — fires for schools without a 247
        // college landing page (e.g. small D-I programs like California
        // Baptist) where 247 renders just `<img alt="..." title="...">` with
        // no surrounding `<a class="img-link">`.
        status_bare_img: Selector::parse(".status > img").unwrap(),
        status_checkmark: Selector::parse(".status b.checkmark").unwrap(),
        status_crystal_ball: Selector::parse(".status .rankings-page__crystal-ball").unwrap(),
        photo_img: Selector::parse(".circle-image-block img").unwrap(),
    })
}

/// Parse a 247 composite-ranking HTML fragment into a vector of recruit rows.
///
/// Empty fragment (or fragment past the last data page) returns `vec![]` —
/// the caller uses that as the stop-paging signal.
pub fn parse_recruits_html(body: &str) -> Vec<RecruitRow> {
    let doc = Html::parse_fragment(body);
    let sel = selectors();

    let mut out = Vec::new();
    for item in doc.select(&sel.item) {
        // Belt-and-suspenders: the selector already excludes `.showmore_blk`,
        // but check again in case the upstream class set drifts.
        if sel.showmore.matches(&item) {
            continue;
        }

        let Some(profile_url) = first_attr(&item, &sel.name_link, "href") else {
            // No profile link → no stable key → skip with a warning.
            warn!("recruit row has no `a.rankings-page__name-link href` — skipping");
            continue;
        };
        let Some(recruit_key) = recruit_key_from_url(&profile_url) else {
            warn!(
                profile_url,
                "could not extract recruit_key from URL — skipping"
            );
            continue;
        };

        let display_name = first_text(&item, &sel.name_link);
        let (first_name, last_name) = split_name(display_name.as_deref());

        let (high_school, city, state) =
            parse_meta(first_text(&item, &sel.meta).as_deref().unwrap_or(""));
        let (height, weight) =
            parse_metrics(first_text(&item, &sel.metrics).as_deref().unwrap_or(""));

        let composite_rank =
            first_int(&item, &sel.rank_primary).or_else(|| first_int(&item, &sel.natrank));
        let previous_rank = first_int(&item, &sel.rank_other);
        let composite_rating = first_float(&item, &sel.score);
        let star_rating = Some(item.select(&sel.star_yellow).count() as i16);
        let position_rank = first_int(&item, &sel.posrank);
        let state_rank = first_int(&item, &sel.sttrank);

        let (committed_school, committed_school_slug, commit_status) = parse_commit(&item, sel);

        let photo_url = item
            .select(&sel.photo_img)
            .next()
            .and_then(|img| {
                img.value()
                    .attr("data-src")
                    .or_else(|| img.value().attr("src"))
            })
            .map(|s| s.to_string());

        let raw_html = item.html();

        out.push(RecruitRow {
            recruit_key,
            first_name,
            last_name,
            composite_rank,
            previous_rank,
            composite_rating,
            star_rating,
            position_rank,
            state_rank,
            position: first_text(&item, &sel.position),
            height,
            weight,
            high_school,
            city,
            state,
            committed_school,
            committed_school_slug,
            commit_status,
            profile_url: Some(profile_url),
            photo_url,
            raw_html,
        });
    }
    out
}

/// Derive `(committed_school, slug, commit_status)` from a row's `.status`
/// block. Four observed states, signalled by row markers:
///
/// * `<a class="img-link" href="...">` + `<b class="checkmark">` → "Signed"
/// * `<a class="img-link" href="...">` only → "Committed"
/// * direct-child `<img>` of `.status` (no `<a>` wrapper) → "Committed"
///   without a slug — fires for schools that don't have a 247 college
///   landing page (small D-I programs like California Baptist).
/// * `.rankings-page__crystal-ball` (none of the above) → "Uncommitted"
fn parse_commit(
    item: &ElementRef<'_>,
    sel: &Selectors,
) -> (Option<String>, Option<String>, Option<String>) {
    if let Some(link) = item.select(&sel.status_img_link).next() {
        let school = link
            .select(&sel.status_img)
            .next()
            .and_then(|img| img.value().attr("alt"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let slug = link
            .value()
            .attr("href")
            .and_then(school_slug_from_college_url);
        let signed = item.select(&sel.status_checkmark).next().is_some();
        let status = if signed { "Signed" } else { "Committed" };
        return (school, slug, Some(status.to_string()));
    }
    if let Some(img) = item.select(&sel.status_bare_img).next() {
        let school = img
            .value()
            .attr("alt")
            .or_else(|| img.value().attr("title"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let signed = item.select(&sel.status_checkmark).next().is_some();
        let status = if signed { "Signed" } else { "Committed" };
        return (school, None, Some(status.to_string()));
    }
    if item.select(&sel.status_crystal_ball).next().is_some() {
        return (None, None, Some("Uncommitted".to_string()));
    }
    (None, None, None)
}

/// Extract the trailing numeric ID from a 247 player profile URL.
/// `/player/alex-constanza-46134907/` → `Some(46134907)`. The trailing slash
/// is optional — the regex matches either form.
pub fn recruit_key_from_url(url: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"-(\d+)/?$").unwrap());
    re.captures(url)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok())
}

/// Extract the school slug from a 247 college URL. Tolerates URLs with or
/// without a trailing path segment / slash.
/// `https://247sports.com/college/north-carolina/season/2026-basketball/commits/`
/// → `Some("north-carolina")`.
pub fn school_slug_from_college_url(url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"/college/([^/]+)(?:/|$)").unwrap());
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Parse a `.recruit .meta` blob into `(high_school, city, state)`.
/// Format observed: `"{HS_NAME} ({CITY}, {STATE})"`. Returns `(None, None, None)`
/// on unparseable input — the row still lands, just without origin metadata.
pub fn parse_meta(meta: &str) -> (Option<String>, Option<String>, Option<String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^(.*?)\s*\(\s*([^,]+?)\s*,\s*([^)]+?)\s*\)\s*$").unwrap());
    let s = meta.trim();
    if s.is_empty() {
        return (None, None, None);
    }
    if let Some(caps) = re.captures(s) {
        let hs = caps
            .get(1)
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty());
        let city = caps.get(2).map(|m| m.as_str().trim().to_string());
        let state = caps.get(3).map(|m| m.as_str().trim().to_string());
        return (hs, city, state);
    }
    // Fall back to treating the whole string as the HS name when 247 omits
    // the parenthetical city/state.
    (Some(s.to_string()), None, None)
}

/// Parse a `.metrics` blob into `(height, weight)`.
/// Format observed: `"6-8 / 205"`. Either side may be missing.
pub fn parse_metrics(metrics: &str) -> (Option<String>, Option<i32>) {
    let s = metrics.trim();
    if s.is_empty() {
        return (None, None);
    }
    let mut it = s.splitn(2, '/');
    let height = it
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let weight = it.next().and_then(|s| s.trim().parse::<i32>().ok());
    (height, weight)
}

/// Split a display name into `(first_name, last_name)` by the first whitespace.
/// `"Alex Constanza"` → `(Some("Alex"), Some("Constanza"))`,
/// `"AJ Dybantsa"` → `(Some("AJ"), Some("Dybantsa"))`,
/// `"Carlos De La Hoya"` → `(Some("Carlos"), Some("De La Hoya"))`.
/// A single-word name lands as `(Some(name), None)`.
fn split_name(name: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(name) = name else {
        return (None, None);
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let mut it = trimmed.splitn(2, char::is_whitespace);
    let first = it.next().map(|s| s.to_string());
    let last = it
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (first, last)
}

// --- Small selector helpers -----------------------------------------------

fn first_text(item: &ElementRef<'_>, sel: &Selector) -> Option<String> {
    item.select(sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_attr(item: &ElementRef<'_>, sel: &Selector, attr: &str) -> Option<String> {
    item.select(sel)
        .next()
        .and_then(|el| el.value().attr(attr))
        .map(|s| s.to_string())
}

fn first_int(item: &ElementRef<'_>, sel: &Selector) -> Option<i32> {
    first_text(item, sel).and_then(|s| s.parse::<i32>().ok())
}

fn first_float(item: &ElementRef<'_>, sel: &Selector) -> Option<f32> {
    first_text(item, sel).and_then(|s| s.parse::<f32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recruit_key_from_url_trailing_id() {
        assert_eq!(
            recruit_key_from_url("/player/alex-constanza-46134907/"),
            Some(46_134_907)
        );
        assert_eq!(
            recruit_key_from_url("/player/sayon-keita-46160428"),
            Some(46_160_428)
        );
        assert_eq!(recruit_key_from_url("/player/no-id/"), None);
        assert_eq!(recruit_key_from_url(""), None);
    }

    #[test]
    fn school_slug_extracts_segment() {
        assert_eq!(
            school_slug_from_college_url(
                "https://247sports.com/college/north-carolina/season/2026-basketball/commits/"
            ),
            Some("north-carolina".to_string())
        );
        assert_eq!(
            school_slug_from_college_url("https://247sports.com/college/duke/"),
            Some("duke".to_string())
        );
        assert_eq!(school_slug_from_college_url("not a college url"), None);
    }

    #[test]
    fn parse_meta_splits_hs_city_state() {
        assert_eq!(
            parse_meta(" SPIRE Academy (Geneva, OH)  "),
            (
                Some("SPIRE Academy".to_string()),
                Some("Geneva".to_string()),
                Some("OH".to_string())
            )
        );
        assert_eq!(
            parse_meta("Spain (Spain, SPAI)"),
            (
                Some("Spain".to_string()),
                Some("Spain".to_string()),
                Some("SPAI".to_string())
            )
        );
    }

    #[test]
    fn parse_meta_falls_back_to_whole_string() {
        assert_eq!(
            parse_meta("Some High School"),
            (Some("Some High School".to_string()), None, None)
        );
        assert_eq!(parse_meta(""), (None, None, None));
    }

    #[test]
    fn parse_metrics_splits_height_weight() {
        assert_eq!(
            parse_metrics(" 6-8 / 205 "),
            (Some("6-8".to_string()), Some(205))
        );
        assert_eq!(
            parse_metrics("6-11 / 215"),
            (Some("6-11".to_string()), Some(215))
        );
    }

    #[test]
    fn parse_metrics_handles_missing_weight() {
        assert_eq!(parse_metrics("6-8 / "), (Some("6-8".to_string()), None));
        assert_eq!(parse_metrics("6-8"), (Some("6-8".to_string()), None));
        assert_eq!(parse_metrics(""), (None, None));
    }

    #[test]
    fn split_name_handles_common_cases() {
        assert_eq!(
            split_name(Some("Alex Constanza")),
            (Some("Alex".to_string()), Some("Constanza".to_string()))
        );
        assert_eq!(
            split_name(Some("AJ Dybantsa")),
            (Some("AJ".to_string()), Some("Dybantsa".to_string()))
        );
        assert_eq!(
            split_name(Some("Carlos De La Hoya")),
            (Some("Carlos".to_string()), Some("De La Hoya".to_string()))
        );
        assert_eq!(split_name(Some("Pelé")), (Some("Pelé".to_string()), None));
        assert_eq!(split_name(Some("")), (None, None));
        assert_eq!(split_name(None), (None, None));
    }

    #[test]
    fn institution_group_round_trip() {
        assert_eq!(InstitutionGroup::HighSchool.as_url_param(), "highschool");
        assert_eq!(InstitutionGroup::Juco.as_url_param(), "juco");
        assert_eq!(InstitutionGroup::Prep.as_url_param(), "prep");
        assert_eq!(
            InstitutionGroup::parse("highschool"),
            Some(InstitutionGroup::HighSchool)
        );
        assert_eq!(
            InstitutionGroup::parse("HS"),
            Some(InstitutionGroup::HighSchool)
        );
        assert_eq!(InstitutionGroup::parse("nope"), None);
    }

    #[test]
    fn parse_empty_html_returns_no_rows() {
        assert!(parse_recruits_html("").is_empty());
        assert!(parse_recruits_html("<html><body></body></html>").is_empty());
    }

    #[test]
    fn parse_minimal_committed_row() {
        let html = r##"
        <li class="rankings-page__list-item">
          <div class="wrapper">
            <div class="rank-column"><div class="primary">54</div></div>
            <div class="circle-image-block">
              <img alt="Sayon Keita" data-src="https://example.test/photo.jpg" />
            </div>
            <div class="recruit">
              <a class="rankings-page__name-link" href="/player/sayon-keita-46160428/">Sayon Keita</a>
              <span class="meta"> Spain (Spain, SPAI) </span>
            </div>
            <div class="position"> C </div>
            <div class="metrics"> 6-11 / 215 </div>
            <div class="rating">
              <div class="rankings-page__star-and-score">
                <span class="icon-starsolid yellow"></span>
                <span class="icon-starsolid yellow"></span>
                <span class="icon-starsolid yellow"></span>
                <span class="icon-starsolid yellow"></span>
                <span class="icon-starsolid lightgrey"></span>
                <span class="score">0.9800</span>
              </div>
              <div class="rank">
                <a class="natrank" href="#">54</a>
                <a class="posrank" href="#">6</a>
                <a class="sttrank" href="#">1</a>
              </div>
            </div>
            <div class="status">
              <a class="img-link" href="https://247sports.com/college/north-carolina/season/2026-basketball/commits/">
                <img alt="North Carolina" data-src="x.png" />
              </a>
            </div>
          </div>
        </li>
        "##;
        let rows = parse_recruits_html(html);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.recruit_key, 46_160_428);
        assert_eq!(r.first_name.as_deref(), Some("Sayon"));
        assert_eq!(r.last_name.as_deref(), Some("Keita"));
        assert_eq!(r.composite_rank, Some(54));
        assert_eq!(r.previous_rank, None);
        assert_eq!(r.composite_rating, Some(0.98));
        assert_eq!(r.star_rating, Some(4));
        assert_eq!(r.position_rank, Some(6));
        assert_eq!(r.state_rank, Some(1));
        assert_eq!(r.position.as_deref(), Some("C"));
        assert_eq!(r.height.as_deref(), Some("6-11"));
        assert_eq!(r.weight, Some(215));
        assert_eq!(r.high_school.as_deref(), Some("Spain"));
        assert_eq!(r.committed_school.as_deref(), Some("North Carolina"));
        assert_eq!(r.committed_school_slug.as_deref(), Some("north-carolina"));
        assert_eq!(r.commit_status.as_deref(), Some("Committed"));
    }

    #[test]
    fn parse_bare_img_committed_without_url() {
        // Schools without a 247 college landing page render the commit as a
        // bare `<img>` inside `.status`, not wrapped in `<a class="img-link">`.
        // Observed in the wild: California Baptist commits (Steven Reynolds,
        // rank 175 in class of 2026).
        let html = r##"
        <li class="rankings-page__list-item">
          <div class="recruit">
            <a class="rankings-page__name-link" href="/player/steven-reynolds-46143041/">Steven Reynolds</a>
          </div>
          <div class="status">
            <img alt="California Baptist" title="California Baptist" />
            <a class="icon-caret-down expand-anchor" href="#"></a>
          </div>
        </li>
        "##;
        let rows = parse_recruits_html(html);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.recruit_key, 46_143_041);
        assert_eq!(r.committed_school.as_deref(), Some("California Baptist"));
        // No URL → no slug to extract.
        assert!(r.committed_school_slug.is_none());
        assert_eq!(r.commit_status.as_deref(), Some("Committed"));
    }

    #[test]
    fn parse_uncommitted_and_signed_variants() {
        // Uncommitted: crystal-ball block, no img-link
        let uncommitted = r##"
        <li class="rankings-page__list-item">
          <div class="recruit">
            <a class="rankings-page__name-link" href="/player/test-uncommitted-1/">Test Uncommitted</a>
          </div>
          <div class="status">
            <div class="rankings-page__crystal-ball"><div class="cb-block"><span>N/A</span></div></div>
          </div>
        </li>
        "##;
        let rows = parse_recruits_html(uncommitted);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].commit_status.as_deref(), Some("Uncommitted"));
        assert!(rows[0].committed_school.is_none());

        // Signed: img-link + checkmark
        let signed = r##"
        <li class="rankings-page__list-item">
          <div class="recruit">
            <a class="rankings-page__name-link" href="/player/test-signed-2/">Test Signed</a>
          </div>
          <div class="status">
            <a class="img-link" href="https://247sports.com/college/duke/season/2026-basketball/commits/">
              <img alt="Duke" />
            </a>
            <b class="checkmark"></b>
          </div>
        </li>
        "##;
        let rows = parse_recruits_html(signed);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].commit_status.as_deref(), Some("Signed"));
        assert_eq!(rows[0].committed_school.as_deref(), Some("Duke"));
        assert_eq!(rows[0].committed_school_slug.as_deref(), Some("duke"));
    }

    #[test]
    fn parse_skips_showmore_sentinel() {
        let html = r##"
        <li class="rankings-page__list-item rankings-page__showmore showmore_blk">
          <div>Show more</div>
        </li>
        <li class="rankings-page__list-item">
          <div class="recruit">
            <a class="rankings-page__name-link" href="/player/test-9/">Test Player</a>
          </div>
        </li>
        "##;
        let rows = parse_recruits_html(html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recruit_key, 9);
    }
}
