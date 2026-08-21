//! 247Sports transfer-portal API client (`ipa.247sports.com/rdb/v1/transfers/`).
//!
//! The endpoint is paginated (~25 players/page, ~105 pages for the spring-2026
//! cycle = 2,620 total) and gated by a Bearer JWT tied to a 247 subscription.
//! The JWT expires ~6 hours after issue and must be re-captured manually from
//! DevTools — see ROADMAP §5b "DB-backed transfers ingest pipeline" for the
//! capture flow.
//!
//! Returned shape:
//! ```json
//! {
//!   "lastUpdated": "2026-05-10T23:30:00Z",
//!   "pagination": { "count": 2620, "currentPage": 1, "pageCount": 105, ... },
//!   "players": [ { "player": { "key": 12345, "firstName": "...", ... } }, ... ]
//! }
//! ```
//!
//! We deliberately keep this client lean compared to `NatStatClient`: 247's API
//! is a straightforward REST endpoint with stable shape — no v3/v4 fallback,
//! no string-vs-int meta quirks, no NO_DATA sentinel. The cstat-side
//! complexity lives in the ingest module instead.

use crate::rate_limiter::RateLimiter;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const BASE_URL: &str = "https://ipa.247sports.com/rdb/v1/transfers/";
const DEFAULT_PAGE_SIZE: u32 = 25;
/// Names the client without a contact URL, matching `torvik.rs`'s `cstat/0.1`
/// and `client.rs`'s `cstat/0.1.0`. Distinct enough for an operator to identify
/// and rate-limit or block this ingest specifically; this is not a browser
/// impersonation (see [`GUEST_PAGE_UA`] below for the one place that is, and
/// why).
const USER_AGENT: &str = "cstat-ingest/0.1";

/// Public portal page whose bootstrap JSON embeds a server-minted **guest**
/// JWT. `{year}` is the portal class year. Fetched with a browser UA — the
/// page is ordinary HTML served to anyone.
const GUEST_PAGE_TMPL: &str = "https://247sports.com/season/{year}-basketball/transferportal/";

/// Browser UA for the guest-page fetch only. The API calls keep [`USER_AGENT`]
/// — this one exists because the public page is served by a CDN that varies
/// its response for non-browser agents.
const GUEST_PAGE_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Default rate budget: 3,600 requests/hr ≈ 1 req/sec. 247 doesn't publish a
/// limit; this is our self-imposed politeness ceiling. Overridable via
/// `TFS_247_RATE_PER_HOUR` if we ever need to slow further.
const DEFAULT_RATE_PER_HOUR: u32 = 3_600;

const MAX_RETRIES: u32 = 4;

#[derive(Debug, Error)]
pub enum TfsError {
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

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("could not extract a guest JWT from {url} (247 page layout changed?)")]
    GuestJwtNotFound { url: String },
}

/// Lean HTTP client for the 247Sports transfer-portal API.
///
/// Holds the bearer JWT (read once from `TFS_247_JWT`) and a token-bucket
/// rate limiter. All requests funnel through `fetch_page`, which handles
/// rate-limit waits, retries on 5xx, and JWT-expiry detection.
pub struct TfsClient {
    http: Client,
    jwt: String,
    rate_limiter: RateLimiter,
}

/// Outcome of a lightweight 247 auth probe ([`TfsClient::probe_auth`]).
#[derive(Debug)]
pub enum AuthProbe {
    /// The JWT was accepted and page 1 returned; `count` is the portal size.
    Valid { count: u32 },
    /// 247 rejected the JWT (401/403) — expired or revoked; re-capture needed.
    Expired { status: u16 },
    /// The feed was unreachable for another reason (network / 5xx / parse).
    Unreachable(String),
}

/// Parsed pagination metadata from a `getTransferRanking` response.
#[derive(Debug, Clone)]
pub struct TfsPagination {
    pub count: u32,
    pub current_page: u32,
    pub page_count: u32,
    pub items_per_page: u32,
}

/// One page of transfer-portal results.
#[derive(Debug, Clone)]
pub struct TfsPage {
    /// Server-reported `lastUpdated` for the whole dataset (drives incremental refresh).
    pub last_updated: Option<String>,
    pub pagination: TfsPagination,
    /// The unwrapped player array — each element is the inner `player` object
    /// (the outer `{"player": {...}}` wrapper is stripped).
    pub players: Vec<Value>,
}

/// Read the rate budget from `TFS_247_RATE_PER_HOUR` (default 3600).
fn rate_from_env() -> u32 {
    std::env::var("TFS_247_RATE_PER_HOUR")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_RATE_PER_HOUR)
}

/// Pull the first `"jwt":"…"` value out of a 247 page body.
///
/// Split out from the fetch so the brittle half is unit-testable without a
/// network call — this is the piece a 247 redesign breaks.
fn extract_guest_jwt(body: &str) -> Option<String> {
    // JWTs are `header.payload.signature`, all base64url. Anchored on the JSON
    // key so we don't pick up an unrelated token-shaped string elsewhere on the
    // page, and length-floored so a truncated or placeholder value doesn't pass.
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r#""jwt"\s*:\s*"([A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})""#,
        )
        .expect("static regex")
    });
    re.captures(body)?.get(1).map(|m| m.as_str().to_string())
}

impl TfsClient {
    /// Build a client from `TFS_247_JWT` (required) and optional
    /// `TFS_247_RATE_PER_HOUR` (default 3600).
    ///
    /// Prefer [`Self::from_env_or_guest`] for anything that needs to run
    /// unattended — this constructor hard-fails when the subscriber token is
    /// absent or stale.
    pub fn from_env() -> Result<Self, TfsError> {
        let jwt = std::env::var("TFS_247_JWT").map_err(|_| TfsError::MissingJwt)?;
        Ok(Self::new(jwt, rate_from_env()))
    }

    /// Build a client without requiring a hand-captured subscriber token.
    ///
    /// Precedence: `TFS_247_JWT` when set (a subscriber token — keep using it,
    /// it is the only way to reach subscriber-gated fields), otherwise mint a
    /// **guest** token off the public portal page via [`Self::fetch_guest_jwt`].
    ///
    /// This is what makes an unattended run possible. The subscriber token
    /// expires ~6h after issue with no renewal path, so any scheduled job built
    /// on `from_env` spends most of its life failing on a dead credential —
    /// which is precisely why ROADMAP S5/P3 declined to schedule the 247 feeds
    /// at all. A guest token is minted on demand, per run, from a page that
    /// needs no login.
    ///
    /// Verified 2026-08-19 against the live feed: guest reaches `/transfers/`,
    /// `/recruits/`, `/commits/` and `/decommits/` with full ratings, ranks and
    /// destinations. It does **not** reach `/unrankedRecruits/`, `/sports/` or
    /// `/institutionGroups/` (403) — set `TFS_247_JWT` if you need those.
    pub async fn from_env_or_guest(year: i32) -> Result<Self, TfsError> {
        let rate = rate_from_env();
        match std::env::var("TFS_247_JWT") {
            Ok(jwt) if !jwt.trim().is_empty() => {
                info!("using subscriber TFS_247_JWT");
                Ok(Self::new(jwt, rate))
            }
            _ => {
                let jwt = Self::fetch_guest_jwt(year).await?;
                info!(year, "minted a 247 guest JWT from the public portal page");
                Ok(Self::new(jwt, rate))
            }
        }
    }

    /// GET the public portal page and pull the guest JWT out of its bootstrap
    /// JSON. No credentials, no headless browser — the page ships the token it
    /// uses for its own client-side calls.
    ///
    /// Brittle to a 247 redesign by construction (it reads a `"jwt":"…"` key
    /// out of markup), which is why the failure is a distinct error variant:
    /// callers can fall back to `TFS_247_JWT` or to a snapshot rather than
    /// treating it as an outage.
    pub async fn fetch_guest_jwt(year: i32) -> Result<String, TfsError> {
        let url = GUEST_PAGE_TMPL.replace("{year}", &year.to_string());
        let http = Client::builder()
            .user_agent(GUEST_PAGE_UA)
            .gzip(true)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        let body = http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        extract_guest_jwt(&body).ok_or(TfsError::GuestJwtNotFound { url })
    }

    /// Build with an explicit JWT + rate (handy for tests).
    pub fn new(jwt: String, max_per_hour: u32) -> Self {
        Self {
            http: Client::builder()
                .user_agent(USER_AGENT)
                .gzip(true)
                // 247 responses are normally ~200ms; a 30s ceiling stops a
                // single stalled request from blocking the whole ingest.
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            jwt,
            rate_limiter: RateLimiter::new(max_per_hour),
        }
    }

    /// Lightweight auth probe: fetch page 1 for `year` and classify the result.
    ///
    /// 247's JWT expires ~6h after issue with no renewal, so it is the single
    /// biggest connectivity risk in the pipeline. This "peek page 1" check is the
    /// ground truth (does 247 accept the token *right now*), used by the
    /// `preflight` command and the transfers ingest to distinguish an expired
    /// token (skip + keep the last snapshot) from a transient outage (retry).
    /// Costs one API call.
    pub async fn probe_auth(&self, year: i32) -> AuthProbe {
        match self.fetch_page(year, 1).await {
            Ok(page) => AuthProbe::Valid {
                count: page.pagination.count,
            },
            Err(TfsError::JwtExpired { status }) => AuthProbe::Expired { status },
            Err(e) => AuthProbe::Unreachable(e.to_string()),
        }
    }

    /// Fetch a single page of transfers for a class year. `page` is 1-indexed.
    pub async fn fetch_page(&self, year: i32, page: u32) -> Result<TfsPage, TfsError> {
        let url = format!(
            "{BASE_URL}?listType=1&page={page}&pageSize={DEFAULT_PAGE_SIZE}\
             &param1=college&param2=transfer-portal\
             &sport=basketball&sportKey=2&year={year}",
        );

        let mut attempt: u32 = 0;
        loop {
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                let backoff_secs = 2u64.pow(attempt);
                info!(year, page, attempt, backoff_secs, "retrying 247 request");
            } else {
                info!(year, page, "fetching 247 transfers page");
            }

            let response = match self
                .http
                .get(&url)
                .bearer_auth(&self.jwt)
                .header("origin", "https://247sports.com")
                .header("referer", "https://247sports.com/")
                .header("x-tfs-guest", "false")
                .header("accept", "application/json")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(TfsError::Http(e));
                    }
                    warn!(year, page, attempt, error = %e, "network error — retrying");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    attempt += 1;
                    continue;
                }
            };

            let status = response.status();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(TfsError::JwtExpired {
                    status: status.as_u16(),
                });
            }

            if status.as_u16() == 429 || status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                if attempt >= MAX_RETRIES {
                    return Err(TfsError::HttpStatus {
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
                return Err(TfsError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });
            }

            let body: Value = response.json().await?;
            return Ok(parse_page(body));
        }
    }
}

/// Convert a raw API response body into a `TfsPage`, unwrapping the
/// `players[].player` envelope so callers see flat player objects.
pub fn parse_page(body: Value) -> TfsPage {
    let last_updated = body
        .get("lastUpdated")
        .and_then(|v| v.as_str())
        .map(String::from);

    let p = body.get("pagination");
    let pagination = TfsPagination {
        count: p
            .and_then(|v| v.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        current_page: p
            .and_then(|v| v.get("currentPage"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        page_count: p
            .and_then(|v| v.get("pageCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        items_per_page: p
            .and_then(|v| v.get("itemsPerPage"))
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_PAGE_SIZE as u64) as u32,
    };

    let players = body
        .get("players")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|wrapper| wrapper.get("player").cloned())
                .collect()
        })
        .unwrap_or_default();

    TfsPage {
        last_updated,
        pagination,
        players,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shape of the real bootstrap blob (2026-08-19): the token sits in a
    /// larger JSON object inline in the page, alongside other keys.
    const PAGE_SAMPLE: &str = r#"<script>window.__data = {"site":"247sports.com",
        "jwt":"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJnaWQiOiI1NzNmZWRhYiIsInNzIjoiR3Vlc3QifQ.abc-DEF_123",
        "year":2027};</script>"#;

    #[test]
    fn extract_guest_jwt_pulls_token_from_page_bootstrap() {
        let jwt = extract_guest_jwt(PAGE_SAMPLE).expect("token");
        assert!(jwt.starts_with("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9."));
        assert_eq!(jwt.split('.').count(), 3, "must be a full three-part JWT");
    }

    #[test]
    fn extract_guest_jwt_returns_none_when_absent() {
        // The failure mode that matters: 247 redesigns the page and the key is
        // simply gone. Must be None (→ `GuestJwtNotFound`, which callers can
        // fall back on) rather than a partial match.
        assert!(extract_guest_jwt("<html><body>no token here</body></html>").is_none());
        assert!(extract_guest_jwt(r#"{"jwt":""}"#).is_none());
        assert!(extract_guest_jwt(r#"{"jwt":"not-a-jwt"}"#).is_none());
    }

    #[test]
    fn extract_guest_jwt_ignores_token_shaped_strings_under_other_keys() {
        // Anchoring on the `"jwt"` key is what keeps an unrelated base64 blob
        // (analytics payloads, CSRF tokens) from being handed to the API as a
        // bearer credential.
        let body = r#"{"csrf":"aaaaaaaa.bbbbbbbb.cccccccc","jwt":"head1234.body5678.sig90abc"}"#;
        assert_eq!(
            extract_guest_jwt(body).as_deref(),
            Some("head1234.body5678.sig90abc")
        );
    }

    #[test]
    fn parse_page_unwraps_player_envelope() {
        let body = json!({
            "lastUpdated": "2026-05-10T23:30:00Z",
            "pagination": {
                "count": 2620,
                "currentPage": 1,
                "pageCount": 105,
                "itemsPerPage": 25
            },
            "players": [
                { "player": { "key": 1, "firstName": "Cameron", "lastName": "Boozer" } },
                { "player": { "key": 2, "firstName": "AJ", "lastName": "Dybantsa" } }
            ]
        });
        let page = parse_page(body);
        assert_eq!(page.last_updated.as_deref(), Some("2026-05-10T23:30:00Z"));
        assert_eq!(page.pagination.count, 2620);
        assert_eq!(page.pagination.page_count, 105);
        assert_eq!(page.players.len(), 2);
        assert_eq!(
            page.players[0].get("firstName").and_then(|v| v.as_str()),
            Some("Cameron")
        );
    }

    #[test]
    fn parse_page_handles_missing_fields() {
        let body = json!({});
        let page = parse_page(body);
        assert!(page.last_updated.is_none());
        assert_eq!(page.pagination.count, 0);
        assert!(page.players.is_empty());
    }
}
