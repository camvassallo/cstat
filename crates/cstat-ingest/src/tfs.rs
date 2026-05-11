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
const USER_AGENT: &str = "cstat-ingest/0.1 (+https://campom.org)";

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

impl TfsClient {
    /// Build a client from `TFS_247_JWT` (required) and optional
    /// `TFS_247_RATE_PER_HOUR` (default 3600).
    pub fn from_env() -> Result<Self, TfsError> {
        let jwt = std::env::var("TFS_247_JWT").map_err(|_| TfsError::MissingJwt)?;
        let rate = std::env::var("TFS_247_RATE_PER_HOUR")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(DEFAULT_RATE_PER_HOUR);
        Ok(Self::new(jwt, rate))
    }

    /// Build with an explicit JWT + rate (handy for tests).
    pub fn new(jwt: String, max_per_hour: u32) -> Self {
        Self {
            http: Client::builder()
                .user_agent(USER_AGENT)
                .gzip(true)
                .build()
                .expect("failed to build HTTP client"),
            jwt,
            rate_limiter: RateLimiter::new(max_per_hour),
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
