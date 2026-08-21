//! 247Sports composite recruit-rankings client.
//!
//! Sister to [`crate::tfs::TfsClient`]. Same rate limiter, same retry/backoff.
//!
//! # Two transports
//!
//! The client speaks either of two 247 surfaces for the same two feeds, chosen
//! by which constructor built it:
//!
//! * **JSON** (default) — `ipa.247sports.com/rdb/v1/{recruits,commits}/`,
//!   authenticated with a **guest** bearer JWT minted per run off a public page
//!   ([`Recruit247Client::guest`]). No hand-captured credential, which is what
//!   makes an unattended run possible.
//! * **HTML** (fallback) — the original `247sports.com/season/…` scrape,
//!   authenticated with a subscriber `Cookie:` header ([`Recruit247Client::from_env`])
//!   for the composite rankings, cookie-free ([`Recruit247Client::public`]) for
//!   the commits feed. Kept because it is the only source for two columns the
//!   JSON does not expose (`previous_rank`, `committed_school_slug`) and because
//!   a 247 JSON change shouldn'''t leave the ingest with no path at all.
//!
//! Both transports produce [`RecruitRow`], so [`crate::ingest::recruits`] is
//! transport-agnostic — see [`Self::fetch_page`] / [`Self::fetch_commits_page`],
//! which dispatch on the credential the client carries.
//!
//! ## What differs between them
//!
//! The JSON feeds are a **superset in rows** (712 vs 611 for the 2026 class on
//! 2026-08-19) and a **subset in columns**. Specifically:
//!
//! | Column | JSON rankings | JSON commits | HTML |
//! | --- | --- | --- | --- |
//! | `height` / `weight` / `high_school` | absent | present | present |
//! | `previous_rank` | absent | absent | present |
//! | `committed_school_slug` | absent | absent | present |
//!
//! `height` and `previous_rank` are **served model inputs** (the freshman and
//! trajectory projections read them), so the composite upsert COALESCEs rather
//! than overwrites — a JSON pass must not blank what an HTML pass captured.
//! See `ingest::recruits::upsert_player`.
//!
//! ## The scale trap
//!
//! JSON `compositeRating` is on a **0–100** scale; the `recruits.composite_rating`
//! column and every consumer of it are **0–1** (verified against 363 rows that
//! overlap the HTML-scraped 2026 class: Tyran Stokes is `1.0` in the DB and
//! `100` in the JSON). [`parse_recruits_json`] divides by 100. Getting this
//! wrong would silently feed a 100x feature into the freshman model.
//!
//! ## The star trap
//!
//! The field next to it, `compositeStarRating`, is **not** the composite star
//! rating and is not read. The two JSON feeds disagree with each other on it —
//! `recruits/` calls class-of-2026 rank 76 (composite 0.9738) a 5-star while
//! `commits/` calls rank 69 (composite 0.9763) a 4-star — and the rendered
//! rankings page, which the HTML transport reads by counting star glyphs, shows
//! every player from rank 51 down as a 4-star. Both are 4-stars.
//! [`composite_star_rating`] bands the composite rating instead.
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
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const RECRUITS_URL_BASE: &str = "https://247sports.com/season";

/// JSON rankings feed — the composite recruit universe for a class, ranked and
/// unranked alike. Rows under `players[]`, keyed on `key`.
const JSON_RANKINGS_URL: &str = "https://ipa.247sports.com/rdb/v1/recruits/";

/// JSON commits feed — every commit in a class, including the unranked,
/// international and prep players the rankings feed carries without ratings.
/// Rows under `list[]`, keyed on `playerKey` (NOT `key`, which is the
/// recruit-interest id — they are different numbers on the same row).
const JSON_COMMITS_URL: &str = "https://ipa.247sports.com/rdb/v1/commits/";

/// Rows per JSON request. 247 honors this up to at least 250; 100 keeps a class
/// to ~8 requests without asking for an unusually large page.
const JSON_PAGE_SIZE: u32 = 100;
const RECRUITS_VIEW_PATH: &str = "~/Views/SkyNet/PlayerSportRanking/_SimpleSetForSeason.ascx";
/// Names the client without a contact URL, matching `tfs.rs` and `torvik.rs`.
/// Distinct enough for an operator to identify and rate-limit or block this
/// ingest specifically.
const USER_AGENT: &str = "cstat-ingest/0.1";

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
        "missing TFS_247_COOKIE (or legacy TFS_247_JWT) env var — capture a fresh session via DevTools Copy-as-cURL and export the Cookie header value as TFS_247_COOKIE"
    )]
    MissingJwt,

    #[error(
        "JWT rejected by 247 (HTTP {status}). Token expires ~6h after issue; re-capture from DevTools"
    )]
    JwtExpired { status: u16 },

    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("could not mint a 247 guest JWT: {0}")]
    GuestJwt(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
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
    /// True when the caller should stop paging. The two transports know this
    /// differently and [`json_page`] / the scrape constructors normalize it:
    /// the JSON feeds publish `pagination.pageCount`, so the last page is known
    /// up front and carries real rows; the HTML scrape publishes no count or
    /// end-marker, so walking past the last data page and getting an empty
    /// fragment is the only signal available. Callers must therefore consume a
    /// page's rows BEFORE testing this flag.
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
    /// The unparsed source row, preserved for forensics — the original `<li>`
    /// HTML on the scrape path, the row's JSON object on the API path. Lives in
    /// `raw_player` alongside the parsed fields so a parser bug doesn't require
    /// a re-fetch.
    #[serde(alias = "raw_html")]
    pub raw_source: String,
}

/// 247Sports recruit-rankings HTML client. Mirrors [`crate::tfs::TfsClient`]
/// in shape but parses HTML rather than JSON.
///
/// Auth model: a full `Cookie:` header string is sent on every request. The
/// constructor accepts either a multi-cookie session string (preferred,
/// captured via DevTools Copy-as-cURL) or a legacy single `JWT=…` value
/// (when wrapped as `JWT={value}`). 247 stopped honoring a bare `JWT`
/// cookie at some point — current sessions auth via `REF_TKN` /
/// `cbsiaa` / `minUnifiedSessionToken10` / etc., so we pass whatever the
/// browser was using.
pub struct Recruit247Client {
    http: Client,
    cookie_header: String,
    /// Bearer token for the JSON transport. `Some` selects JSON; `None` falls
    /// back to the HTML scrape. Set by [`Recruit247Client::guest`].
    jwt: Option<String>,
    rate_limiter: RateLimiter,
}

/// Self-imposed request rate from `TFS_247_RATE_PER_HOUR`, falling back to
/// [`DEFAULT_RATE_PER_HOUR`]. Shared by both constructors so the authenticated
/// and cookie-free clients can't drift to different limits.
fn rate_from_env() -> u32 {
    std::env::var("TFS_247_RATE_PER_HOUR")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_RATE_PER_HOUR)
}

impl Recruit247Client {
    /// Build a client from env. Prefers `TFS_247_COOKIE` (the full
    /// `Cookie:` header string from DevTools Copy-as-cURL); falls back to
    /// the legacy `TFS_247_JWT` (wrapped as `JWT={value}`) for backward
    /// compat with pre-2026-05 captures. Rate is optional, default 3600/hr
    /// shared with `TfsClient`.
    pub fn from_env() -> Result<Self, RecruitError> {
        let cookie_header = std::env::var("TFS_247_COOKIE")
            .ok()
            .or_else(|| {
                std::env::var("TFS_247_JWT")
                    .ok()
                    .map(|j| format!("JWT={j}"))
            })
            .ok_or(RecruitError::MissingJwt)?;
        Ok(Self::new(cookie_header, rate_from_env()))
    }

    /// Build a cookie-free client for the public national commits feed
    /// (`/season/{year}-basketball/commits/`), which — unlike the composite
    /// rankings endpoint — needs no subscriber session. Use with
    /// [`Self::fetch_commits_page`]. The empty cookie header is simply not
    /// sent (see [`Self::fetch_commits_page`]).
    pub fn public() -> Self {
        Self::new(String::new(), rate_from_env())
    }

    /// Build with an explicit cookie header + rate (handy for tests).
    /// Prefer [`Self::from_env`] in production code. The string is sent
    /// verbatim as the `Cookie:` header value, so callers pass either a
    /// multi-cookie session (`a=1; b=2; …`) or a single `JWT=xxx`.
    pub fn new(cookie_header: String, max_per_hour: u32) -> Self {
        Self {
            http: Client::builder()
                .user_agent(USER_AGENT)
                .gzip(true)
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            cookie_header,
            jwt: None,
            rate_limiter: RateLimiter::new(max_per_hour),
        }
    }

    /// Build a JSON-transport client on a **guest** bearer token, minted per
    /// run off a public 247 page — no hand-captured credential.
    ///
    /// This is the default path and the reason the recruit feeds can now run
    /// unattended: the subscriber cookie the HTML scrape needs expires ~6h after
    /// issue with no renewal, so anything scheduled on it spends most of its life
    /// failing on a dead credential. Precedence matches
    /// [`crate::tfs::TfsClient::from_env_or_guest`] — an explicit `TFS_247_JWT`
    /// wins when set, since a subscriber token also reaches the guest routes.
    ///
    /// `year` is the class year, used only to pick the public page to mint from.
    pub async fn guest(year: i32) -> Result<Self, RecruitError> {
        let jwt = match std::env::var("TFS_247_JWT") {
            Ok(j) if !j.trim().is_empty() => {
                info!("using subscriber TFS_247_JWT for the 247 JSON feeds");
                j
            }
            _ => {
                let j = crate::tfs::TfsClient::fetch_guest_jwt(year)
                    .await
                    .map_err(|e| RecruitError::GuestJwt(e.to_string()))?;
                info!(year, "minted a 247 guest JWT for the recruit JSON feeds");
                j
            }
        };
        let mut c = Self::new(String::new(), rate_from_env());
        c.jwt = Some(jwt);
        Ok(c)
    }

    /// Build a JSON-transport client on an explicit token (handy for tests).
    pub fn with_jwt(jwt: String, max_per_hour: u32) -> Self {
        let mut c = Self::new(String::new(), max_per_hour);
        c.jwt = Some(jwt);
        c
    }

    /// True when this client speaks JSON rather than scraping HTML.
    pub fn is_json(&self) -> bool {
        self.jwt.is_some()
    }

    /// Fetch a single page of composite recruit rankings. Page is 1-indexed.
    ///
    /// Dispatches on the credential the client carries: a JSON client
    /// ([`Self::guest`]) reads `rdb/v1/recruits/`, a cookie client
    /// ([`Self::from_env`]) scrapes the rankings page.
    ///
    /// `group` is honored only on the HTML path, and even there 247 ignores it
    /// (all three values return identical content — see [`InstitutionGroup`]).
    /// The JSON feed has an `institutionGroup` param with the same non-effect,
    /// verified 2026-08-19, so it is not sent.
    pub async fn fetch_page(
        &self,
        year: i32,
        group: InstitutionGroup,
        page: u32,
    ) -> Result<RecruitPage, RecruitError> {
        if self.jwt.is_some() {
            return self.fetch_rankings_json(year, page).await;
        }
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
        let body = self.fetch_html(&url, &referer, year, page).await?;
        let players = parse_recruits_html(&body);
        Ok(RecruitPage {
            is_last_page: players.is_empty(),
            players,
        })
    }

    /// Fetch a single page of the national commits feed
    /// (`/season/{year}-basketball/commits/?Page=N`). Page is 1-indexed.
    ///
    /// Unlike [`Self::fetch_page`], this endpoint is public — build the client
    /// with [`Self::public`] (cookie-free). It lists every commit including
    /// unranked/international/G-League players the composite rankings omit,
    /// each row carrying its committed school in the `.status` img. 247 serves
    /// a nav-only sentinel (no recruit rows) past the last data page, which
    /// [`parse_commits_html`] returns empty — the caller's stop signal.
    pub async fn fetch_commits_page(
        &self,
        year: i32,
        page: u32,
    ) -> Result<RecruitPage, RecruitError> {
        if self.jwt.is_some() {
            return self.fetch_commits_json(year, page).await;
        }
        let url = format!("{RECRUITS_URL_BASE}/{year}-basketball/commits/?Page={page}");
        let referer = format!("https://247sports.com/season/{year}-basketball/commits/");
        let body = self.fetch_html(&url, &referer, year, page).await?;
        let players = parse_commits_html(&body);
        Ok(RecruitPage {
            is_last_page: players.is_empty(),
            players,
        })
    }

    /// Fetch one page of the JSON rankings feed. Page is 1-indexed.
    async fn fetch_rankings_json(&self, year: i32, page: u32) -> Result<RecruitPage, RecruitError> {
        let body = self.fetch_json(JSON_RANKINGS_URL, year, page).await?;
        Ok(json_page(&body, "players", parse_recruits_json, page))
    }

    /// Fetch one page of the JSON commits feed. Page is 1-indexed.
    async fn fetch_commits_json(&self, year: i32, page: u32) -> Result<RecruitPage, RecruitError> {
        let body = self.fetch_json(JSON_COMMITS_URL, year, page).await?;
        Ok(json_page(&body, "list", parse_commits_json, page))
    }

    /// Shared GET-with-retry for the JSON transport. Mirrors [`Self::fetch_html`]'s
    /// retry policy, with one difference: a 401/403 is always an auth failure here
    /// (every `rdb/v1` route rejects an unauthenticated request), so it fails fast
    /// as [`RecruitError::JwtExpired`] rather than being retried as a bot-block.
    async fn fetch_json(&self, base: &str, year: i32, page: u32) -> Result<Value, RecruitError> {
        let jwt = self
            .jwt
            .as_deref()
            .expect("fetch_json called on a client with no JWT");
        let url = format!(
            "{base}?listType=1&page={page}&pageSize={JSON_PAGE_SIZE}\
             &sport=basketball&sportKey=2&year={year}&yearSport={year}-basketball",
        );

        let mut attempt: u32 = 0;
        loop {
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                let backoff_secs = 2u64.pow(attempt);
                info!(
                    year,
                    page, attempt, backoff_secs, "retrying 247 JSON request"
                );
            } else {
                info!(year, page, url, "fetching 247 JSON page");
            }

            let response = match self
                .http
                .get(&url)
                .bearer_auth(jwt)
                .header("origin", "https://247sports.com")
                .header("referer", "https://247sports.com/")
                .header("accept", "application/json")
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

            return Ok(response.json().await?);
        }
    }

    /// Shared GET-with-retry for both feeds. Returns the response body on
    /// success. The `Cookie` header is sent only when the client carries one
    /// (empty for [`Self::public`]), so the cookie-free commits feed doesn't
    /// send a stray blank cookie. `year`/`page` are for log context only.
    async fn fetch_html(
        &self,
        url: &str,
        referer: &str,
        year: i32,
        page: u32,
    ) -> Result<String, RecruitError> {
        let mut attempt: u32 = 0;
        loop {
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                let backoff_secs = 2u64.pow(attempt);
                info!(year, page, attempt, backoff_secs, "retrying 247 request");
            } else {
                info!(year, page, url, "fetching 247 page");
            }

            let mut req = self
                .http
                .get(url)
                .header("Referer", referer)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Accept", "*/*");
            if !self.cookie_header.is_empty() {
                req = req.header("Cookie", &self.cookie_header);
            }

            let response = match req.send().await {
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
            // Authenticated composite endpoint: a 401/403 means the subscriber
            // cookie expired and won't self-heal, so fail fast with a clear
            // message. The cookie-free commits feed sends no cookie, so a 403
            // there is a transient edge/WAF bot-block, not an auth failure —
            // let it fall through to the retryable branch below instead of
            // aborting the whole run with a misleading "JWT expired".
            if !self.cookie_header.is_empty() && (status.as_u16() == 401 || status.as_u16() == 403)
            {
                return Err(RecruitError::JwtExpired {
                    status: status.as_u16(),
                });
            }

            if status.as_u16() == 429 || status.as_u16() == 403 || status.is_server_error() {
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

            return Ok(response.text().await?);
        }
    }
}

/// Split a JSON feed response into a [`RecruitPage`].
///
/// `rows_key` differs per feed (`players` on the rankings feed, `list` on
/// commits — 247 is not consistent about this). `is_last_page` is taken from
/// `pagination.pageCount` when present so a full class costs no speculative
/// extra request, falling back to "the page came back empty" otherwise.
fn json_page(
    body: &Value,
    rows_key: &str,
    parse: fn(&Value) -> Option<RecruitRow>,
    page: u32,
) -> RecruitPage {
    let rows = body
        .get(rows_key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let players: Vec<RecruitRow> = rows.iter().filter_map(parse).collect();
    let page_count = body
        .pointer("/pagination/pageCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    // A page whose rows all failed to parse is NOT the end of the feed. Testing
    // `players.is_empty()` first would make it one: 247 renames a field, one
    // page mid-class yields zero rows, and the walk stops there — ingesting a
    // partial class and reporting success. Recruits feed the freshman
    // projection, so a silently short class is wrong team rosters, not a
    // cosmetic shortfall. When `pageCount` is published it is the only thing
    // worth believing; the empty-page heuristic is the fallback for a response
    // that omits it.
    let is_last_page = if page_count > 0 {
        page >= page_count
    } else {
        players.is_empty()
    };

    // Rows arrived and none of them parsed — the shape changed under us. Loud,
    // because the run now *continues* past it and would otherwise look clean.
    if players.is_empty() && !rows.is_empty() {
        warn!(
            page,
            rows = rows.len(),
            rows_key,
            "247 JSON page returned rows but none parsed — feed shape may have changed"
        );
    }

    RecruitPage {
        players,
        is_last_page,
    }
}

/// Read a `"key": value` string, treating an empty string as absent.
fn jstr(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read a nested string by JSON pointer, treating an empty string as absent.
fn jptr_str(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 247 Composite star bands, as `(rating floor, stars)` in descending order.
/// Anything rated below the last floor is a 2-star.
///
/// These are bands of the **0–1 composite rating**, which is the number
/// `recruits.composite_rating` holds — not of 247's proprietary 0–100 rating
/// and not of a national rank.
///
/// Calibrated against the captured composite-rankings page in
/// `tests/fixtures/recruits_2026_hs_p2.html`: 50 real rows, ranks 51–100 of the
/// 2026 class, ratings 0.9535–0.9816, every one rendered by 247 as 4-star. The
/// bands reproduce all 50 (`star_bands_match_the_captured_rankings_page`).
///
/// The 4/5 boundary is the one the repo's evidence only brackets rather than
/// pins: the capture starts at rank 51, so it proves the floor is **above
/// 0.9816** without showing a 5-star row. 0.9900 is 247's published figure and
/// is consistent with every row we hold. Confirm it against the HTML-scraped
/// history with `scripts/audit_recruit_stars.sql` (query 3) before assuming it.
pub const COMPOSITE_STAR_BANDS: [(f32, i16); 3] = [(0.9900, 5), (0.8900, 4), (0.7900, 3)];

/// Derive a 247 Composite star rating from a 0–1 composite rating.
///
/// **This deliberately ignores the JSON feeds' own `compositeStarRating`**,
/// which does not agree with the composite the rest of the row is on. On the
/// captured class-of-2026 rankings row for national rank 76 the feed pairs
/// `compositeRating: 97.377…` (= 0.9738) with `compositeStarRating: 5`, while
/// the composite rankings page renders rank 76 at 0.9736 as **4**-star — and
/// renders every player from rank 51 down as 4-star. Whatever star scale that
/// field is on, it is not the one `recruits.star_rating` has held since the
/// column was created, and taking it inflates the 5-star pool by roughly an
/// order of magnitude on any class ingested over the JSON transport.
///
/// That matters beyond the recruit table: `star_rating` is served to the
/// freshman and trajectory projection models as `recruit_star_rating`, a
/// **monotone-increasing** feature (`training/train_freshman_model.py`), so an
/// inflated star is an inflated freshman projection and an inflated preseason
/// AdjEM for whoever signed the class.
///
/// `None` in, `None` out: an unrated recruit has no star. The HTML transport is
/// untouched — it counts the rendered star glyphs, which is the ground truth
/// this function is calibrated to.
pub fn composite_star_rating(composite_rating: Option<f32>) -> Option<i16> {
    let rating = composite_rating?;
    Some(
        COMPOSITE_STAR_BANDS
            .iter()
            .find(|(floor, _)| rating >= *floor)
            .map_or(2, |&(_, stars)| stars),
    )
}

/// Read a number as `i32`. 247 sends weights (and occasionally ranks) as JSON
/// floats (`260.0`), so accept either representation.
fn jnum_i32(v: &Value) -> Option<i32> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f.round() as i64))
        .and_then(|n| i32::try_from(n).ok())
}

/// Classify a row's recruiting status from its institution objects.
///
/// Mirrors the vocabulary [`parse_recruits_html`] derives from row markers, so
/// the two transports write the same `commit_status` values.
fn commit_status_from(signed: bool, committed: bool) -> &'static str {
    if signed {
        "Signed"
    } else if committed {
        "Committed"
    } else {
        "Uncommitted"
    }
}

/// Parse one row of the JSON rankings feed (`rdb/v1/recruits/`, `players[]`).
///
/// Returns `None` for a row with no `key` — the natural key, and the one field
/// the ingest cannot work without.
///
/// **`compositeRating` is rescaled 0–100 → 0–1** to match the column and every
/// consumer of it; see the module docs for the verification.
///
/// **`star_rating` is derived from that rating, not read from the feed's
/// `compositeStarRating`** — that field is on some other star scale and
/// over-awards 5-stars by roughly an order of magnitude. See
/// [`composite_star_rating`].
///
/// `height`, `weight`, `high_school`, `previous_rank` and
/// `committed_school_slug` are absent from this feed and left `None`. The
/// composite upsert COALESCEs them so a JSON pass cannot blank a value an HTML
/// pass or the commits feed captured.
pub fn parse_recruits_json(row: &Value) -> Option<RecruitRow> {
    let recruit_key = row.get("key").and_then(Value::as_i64)?;

    let committed = jptr_str(row, "/committedInstitution/name");
    let signed = jptr_str(row, "/signedInstitution/name");
    let status = commit_status_from(signed.is_some(), committed.is_some());

    let composite_rating = row
        .get("compositeRating")
        .and_then(Value::as_f64)
        .map(|r| (r / 100.0) as f32);

    Some(RecruitRow {
        recruit_key,
        first_name: jstr(row, "firstName"),
        last_name: jstr(row, "lastName"),
        composite_rank: row.get("compositeNationalRank").and_then(jnum_i32),
        // Not exposed by this feed — see the doc comment.
        previous_rank: None,
        composite_rating,
        // NOT the feed's `compositeStarRating` — see [`composite_star_rating`].
        star_rating: composite_star_rating(composite_rating),
        position_rank: row.get("compositePositionRank").and_then(jnum_i32),
        state_rank: row.get("compositeStateRank").and_then(jnum_i32),
        position: jstr(row, "primaryPosition"),
        height: None,
        weight: None,
        high_school: None,
        city: jptr_str(row, "/homeTown/city"),
        state: jptr_str(row, "/homeTown/state"),
        // `signedInstitution` is the firmer of the two and implies the commit.
        committed_school: signed.or(committed),
        committed_school_slug: None,
        commit_status: Some(status.to_string()),
        profile_url: jstr(row, "profileUrl"),
        photo_url: jstr(row, "defaultAssetUrl"),
        raw_source: row.to_string(),
    })
}

/// Parse one row of the JSON commits feed (`rdb/v1/commits/`, `list[]`).
///
/// Keyed on **`playerKey`**, not `key` — on this feed `key` is the
/// recruit-interest id, a different number. `playerKey` is what matches the
/// rankings feed's `key` and the scraped `recruit_key` already in the table
/// (verified against 607 overlapping 2026 rows).
///
/// This feed is the only source of `height` / `weight` / `high_school` on the
/// JSON transport, which matters because both are served projection-model
/// inputs. `currentInstitution` is the player's *present* school — a high
/// school, JUCO, prep or overseas club for a recruit, but the committed college
/// for an early enrollee — so it is read as `high_school` only when its `group`
/// says it is not a college.
pub fn parse_commits_json(row: &Value) -> Option<RecruitRow> {
    let recruit_key = row.get("playerKey").and_then(Value::as_i64)?;

    let committed = jptr_str(row, "/committedInstitution/name");
    // This feed carries NO signing signal, so a commits-owned row is only ever
    // `Committed` or `Uncommitted` — never `Signed`.
    //
    // Worth stating outright because the obvious candidate looks like one and
    // is not: `earlySignee` reads false on 100/100 rows of the 2026 class, whose
    // early period closed in November 2025, and `signedInstitution` — the field
    // the rankings feed actually uses, present on 66 of its top 100 for the same
    // class — is not a key on this feed at all. Deriving `signed` from
    // `earlySignee` therefore produced a constant `false` wearing the costume of
    // a real check.
    //
    // The consequence is a provenance asymmetry, not a wrong row:
    // `commit_status` on a commits-owned row cannot express signing, while on a
    // rankings-owned row it can. The projection is unaffected (it only tests
    // `<> 'Uncommitted'`), but any UI that reads the Signed/Committed split as a
    // confidence gradient is reading provenance on part of the table. See
    // `docs/247_api.md`.
    let status = commit_status_from(false, committed.is_some());

    let current_group = jptr_str(row, "/currentInstitution/group").unwrap_or_default();
    let high_school = if current_group.eq_ignore_ascii_case("College") {
        None
    } else {
        jptr_str(row, "/currentInstitution/name")
    };

    let composite_rating = row
        .pointer("/ranking/compositeRating")
        .and_then(Value::as_f64)
        .map(|r| (r / 100.0) as f32);

    Some(RecruitRow {
        recruit_key,
        first_name: jstr(row, "firstName"),
        last_name: jstr(row, "lastName"),
        // Populated for the ranked minority so a snapshot round-trips faithfully.
        // `upsert_commit` deliberately does not write the rank columns — the
        // rankings feed owns them.
        composite_rank: row
            .pointer("/ranking/overallCompositeRank")
            .and_then(jnum_i32),
        previous_rank: None,
        composite_rating,
        // NOT the feed's `compositeStarRating` — see [`composite_star_rating`].
        star_rating: composite_star_rating(composite_rating),
        position_rank: row
            .pointer("/ranking/positionCompositeRank")
            .and_then(jnum_i32),
        state_rank: row
            .pointer("/ranking/stateCompositeRank")
            .and_then(jnum_i32),
        position: jptr_str(row, "/position/abbreviation")
            .or_else(|| jptr_str(row, "/position/name")),
        // `height` is inches as a float; `formattedHeight` is the "6-10" the
        // column has always held.
        height: jstr(row, "formattedHeight"),
        weight: row.get("weight").and_then(jnum_i32).filter(|&w| w > 0),
        high_school,
        city: jstr(row, "city"),
        state: jstr(row, "stateAbbr").or_else(|| jstr(row, "state")),
        committed_school: committed,
        committed_school_slug: None,
        commit_status: Some(status.to_string()),
        profile_url: jstr(row, "playerUrl"),
        photo_url: jstr(row, "primaryPlayerAvatar").or_else(|| jstr(row, "playerAvatar")),
        raw_source: row.to_string(),
    })
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

/// Build a [`Selectors`] set. The two 247 layouts (composite rankings vs the
/// national commits feed) differ only in the `{prefix}__list-item` /
/// `{prefix}__name-link` / `{prefix}__star-and-score` class prefix — everything
/// else (`.recruit .meta`, `.metrics`, `.status …`, `.circle-image-block img`)
/// is shared — so both callers funnel through here with their prefix.
fn make_selectors(list_item: &str, name_link: &str, star_score_container: &str) -> Selectors {
    Selectors {
        // Exclude the trailing "show more" sentinel <li> 247 appends to each page.
        item: Selector::parse(list_item).unwrap(),
        showmore: Selector::parse("li.showmore_blk").unwrap(),
        rank_primary: Selector::parse(".rank-column .primary").unwrap(),
        rank_other: Selector::parse(".rank-column .other").unwrap(),
        name_link: Selector::parse(name_link).unwrap(),
        meta: Selector::parse(".recruit .meta").unwrap(),
        position: Selector::parse(".position").unwrap(),
        metrics: Selector::parse(".metrics").unwrap(),
        star_yellow: Selector::parse(&format!("{star_score_container} .icon-starsolid.yellow"))
            .unwrap(),
        score: Selector::parse(&format!("{star_score_container} .score")).unwrap(),
        natrank: Selector::parse(".rank .natrank").unwrap(),
        posrank: Selector::parse(".rank .posrank").unwrap(),
        sttrank: Selector::parse(".rank .sttrank").unwrap(),
        status_img_link: Selector::parse(".status a.img-link").unwrap(),
        status_img: Selector::parse("img").unwrap(),
        // Direct-child `<img>` of `.status` — fires for schools without a 247
        // college landing page (e.g. small D-I programs like California
        // Baptist) where 247 renders just `<img alt="..." title="...">` with
        // no surrounding `<a class="img-link">`. The national commits feed
        // uses this bare-img form for *every* row.
        status_bare_img: Selector::parse(".status > img").unwrap(),
        status_checkmark: Selector::parse(".status b.checkmark").unwrap(),
        status_crystal_ball: Selector::parse(".status .rankings-page__crystal-ball").unwrap(),
        photo_img: Selector::parse(".circle-image-block img").unwrap(),
    }
}

/// Selectors for the composite-rankings HTML (`compositerecruitrankings`).
fn selectors() -> &'static Selectors {
    static SEL: OnceLock<Selectors> = OnceLock::new();
    SEL.get_or_init(|| {
        make_selectors(
            "li.rankings-page__list-item:not(.showmore_blk)",
            "a.rankings-page__name-link",
            ".rankings-page__star-and-score",
        )
    })
}

/// Selectors for the national commits feed (`/season/{year}-basketball/commits/`).
/// Same row layout as the rankings page but with an `ri-page__` class prefix.
fn commit_selectors() -> &'static Selectors {
    static SEL: OnceLock<Selectors> = OnceLock::new();
    SEL.get_or_init(|| {
        make_selectors(
            "li.ri-page__list-item:not(.showmore_blk)",
            "a.ri-page__name-link",
            ".ri-page__star-and-score",
        )
    })
}

/// Parse a 247 composite-ranking HTML fragment into a vector of recruit rows.
///
/// Empty fragment (or fragment past the last data page) returns `vec![]` —
/// the caller uses that as the stop-paging signal.
pub fn parse_recruits_html(body: &str) -> Vec<RecruitRow> {
    parse_items(body, selectors())
}

/// Parse a 247 national commits-feed HTML fragment
/// (`/season/{year}-basketball/commits/?Page=N`) into recruit rows.
///
/// Same per-row layout as the composite rankings, so it shares
/// [`parse_items`] with a different selector set. Two things differ from the
/// rankings feed:
/// * The `.score` shown is 247's proprietary 0–100 rating, NOT the 0–1
///   composite. It is **cleared here** (`composite_rating = None`) so nothing
///   downstream — including the forensic `raw_player` JSONB — mistakes the
///   0–100 value for a 0–1 composite. `composite_rank` (a real national rank
///   when the feed shows one) and `star_rating` are left intact.
/// * The committed school is a bare `.status > img` (never an `a.img-link`),
///   which [`parse_commit`]'s bare-img branch already handles.
///
/// Empty/terminal fragment (247 serves a 2-row nav sentinel with no
/// `a.ri-page__name-link` past the last data page) returns `vec![]`.
pub fn parse_commits_html(body: &str) -> Vec<RecruitRow> {
    let mut rows = parse_items(body, commit_selectors());
    for r in &mut rows {
        r.composite_rating = None;
    }
    rows
}

/// Shared per-row extraction for both 247 layouts. `sel` selects which class
/// prefix (`rankings-page__` vs `ri-page__`) to key off.
fn parse_items(body: &str, sel: &Selectors) -> Vec<RecruitRow> {
    let doc = Html::parse_fragment(body);

    let mut out = Vec::new();
    for item in doc.select(&sel.item) {
        // Belt-and-suspenders: the selector already excludes `.showmore_blk`,
        // but check again in case the upstream class set drifts.
        if sel.showmore.matches(&item) {
            continue;
        }

        let Some(profile_url) = first_attr(&item, &sel.name_link, "href") else {
            // No profile link → no stable key → skip with a warning. Fires on
            // the commits feed's terminal nav sentinel (expected) — kept
            // layout-agnostic since `parse_items` serves both feeds.
            warn!("recruit row has no name-link href — skipping");
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

        let raw_source = item.html();

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
            raw_source,
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

/// Extract the player ID from a 247 player profile URL — the trailing number of
/// the `/player/{slug}-{id}` segment.
/// `/player/alex-constanza-46134907/` → `Some(46134907)`.
///
/// Anchored to the `/player/` segment rather than the end of the string because
/// 247 began appending a `/high-school-{schoolId}/?Sport=2` suffix to these
/// URLs (mid-2026). A trailing-`-(\d+)$` regex matched the *school* ID (or
/// failed when `?Sport=2` was present), which silently re-keyed every recruit
/// and produced duplicate rows on refresh. `[^/]*` stops at the first slash, so
/// the capture stays inside the player segment regardless of trailing path.
pub fn recruit_key_from_url(url: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Case-insensitive: the composite-rankings feed emits lowercase
    // `/player/…`, but the national commits feed emits protocol-relative
    // `//247sports.com/Player/…` with a capital `P`.
    let re = RE.get_or_init(|| Regex::new(r"(?i)/player/[^/]*-(\d+)").unwrap());
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

    /// Body shaped like a real JSON feed response: `n` rows under `rows_key`,
    /// with `pageCount` pages in total.
    fn feed_body(rows_key: &str, n: usize, page_count: u64) -> Value {
        let rows: Vec<Value> = (0..n)
            .map(|i| serde_json::json!({ "key": 1000 + i as i64 }))
            .collect();
        serde_json::json!({
            "pagination": { "pageCount": page_count },
            rows_key: rows,
        })
    }

    #[test]
    fn commits_feed_never_claims_signed() {
        // Pins the asymmetry rather than the accident: this feed has no signing
        // field, so a row must come back `Committed` even when the row carries
        // the flag that used to be read as one. If 247 ever adds a real signing
        // signal, this test is the thing that should fail and force the choice
        // to be made deliberately.
        let row = serde_json::json!({
            "playerKey": 12345,
            "earlySignee": true,
            "committedInstitution": { "name": "Duke" },
        });
        let parsed = parse_commits_json(&row).expect("row parses");
        assert_eq!(parsed.commit_status.as_deref(), Some("Committed"));

        let uncommitted = serde_json::json!({ "playerKey": 999 });
        let parsed = parse_commits_json(&uncommitted).expect("row parses");
        assert_eq!(parsed.commit_status.as_deref(), Some("Uncommitted"));
    }

    #[test]
    fn json_page_stops_on_the_published_page_count() {
        // The last page carries real rows AND is flagged last, so the caller
        // must consume before testing. Page 3 of 3 is the end; page 2 is not.
        let mid = json_page(
            &feed_body("players", 100, 3),
            "players",
            parse_recruits_json,
            2,
        );
        assert!(!mid.is_last_page);
        assert_eq!(mid.players.len(), 100);

        let last = json_page(
            &feed_body("players", 12, 3),
            "players",
            parse_recruits_json,
            3,
        );
        assert!(last.is_last_page);
        assert_eq!(last.players.len(), 12, "the final page's rows must survive");
    }

    #[test]
    fn json_page_does_not_treat_an_unparseable_page_as_the_end() {
        // The regression this guards: 247 renames a field, one page mid-class
        // yields zero parsed rows, and an `is_empty()`-first stop signal ends
        // the walk there — ingesting a partial class and reporting success.
        // Recruits feed the freshman projection, so a short class is wrong
        // rosters, not a cosmetic shortfall. `pageCount` says otherwise and
        // `pageCount` wins.
        let body = serde_json::json!({
            "pagination": { "pageCount": 8 },
            "players": [ { "noKeyHere": 1 }, { "noKeyHere": 2 } ],
        });
        let p = json_page(&body, "players", parse_recruits_json, 3);
        assert!(p.players.is_empty());
        assert!(
            !p.is_last_page,
            "a page that parsed nothing is not the end of an 8-page feed"
        );
    }

    #[test]
    fn json_page_falls_back_to_empty_when_no_page_count() {
        // No `pagination.pageCount` (an error-shaped or truncated body): the
        // empty-page heuristic is all that's left, and it still terminates.
        let body = serde_json::json!({ "list": [] });
        let p = json_page(&body, "list", parse_commits_json, 1);
        assert!(p.is_last_page);

        let body = serde_json::json!({ "list": [ { "playerKey": 42 } ] });
        let p = json_page(&body, "list", parse_commits_json, 1);
        assert!(!p.is_last_page);
        assert_eq!(p.players.len(), 1);
    }

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
        // 247's mid-2026 URL change: a `/high-school-{id}/?Sport=2` suffix must
        // NOT be mistaken for the player id. The player id is the trailing
        // number of the `/player/{slug}-{id}` segment.
        assert_eq!(
            recruit_key_from_url("/player/aaron-mcgee-46160340/high-school-340449/?Sport=2"),
            Some(46_160_340)
        );
        assert_eq!(
            recruit_key_from_url("/player/aaron-mcgee-46160340/high-school-340449/"),
            Some(46_160_340)
        );
        assert_eq!(
            recruit_key_from_url(
                "https://247sports.com/player/kendre-harrison-46138185/high-school-318679/?Sport=2"
            ),
            Some(46_138_185)
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
