use crate::cache::ApiCache;
use crate::rate_limiter::RateLimiter;
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use thiserror::Error;
use tracing::{error, info, warn};

#[derive(Debug, Error)]
pub enum NatStatError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API returned error: {code} — {message}")]
    ApiError { code: String, message: String },

    #[error("API returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Missing API key")]
    MissingApiKey,
}

/// NatStat API v4 client with rate limiting, concurrency control, and response caching.
///
/// URL format: `https://api4.natst.at/{apikey}/{endpoint}/{service}/{range}/{offset}`
pub struct NatStatClient {
    http: Client,
    api_key: String,
    rate_limiter: RateLimiter,
    cache: ApiCache,
    /// Default cache TTL in seconds (default: 6 hours).
    pub default_ttl: i64,
    /// When true, use v3 base URL instead of v4. Flipped on first v4 504 to
    /// work around v4 outages (e.g., the /players endpoint).
    use_v3: AtomicBool,
}

/// Parsed metadata from NatStat API responses.
#[derive(Debug, Clone)]
pub struct ResponseMeta {
    pub results_total: Option<u64>,
    pub results_max: Option<u64>,
    pub page: Option<u64>,
    pub pages_total: Option<u64>,
    pub page_next: Option<String>,
}

const BASE_URL_V4: &str = "https://api4.natst.at";
const BASE_URL_V3: &str = "https://api3.natst.at";
const SERVICE: &str = "mbb";

impl NatStatClient {
    /// Create a new NatStat API v4 client.
    ///
    /// - `api_key`: your NatStat API key (format: `xxxx-xxxxxx`)
    /// - `max_per_hour`: rate limit (500 for standard accounts)
    pub fn new(pool: PgPool, api_key: String, max_per_hour: u32) -> Self {
        Self {
            http: Client::builder()
                .user_agent("cstat/0.1.0")
                .gzip(true)
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            rate_limiter: RateLimiter::new(max_per_hour),
            cache: ApiCache::new(pool),
            default_ttl: 6 * 3600, // 6 hours
            use_v3: AtomicBool::new(false),
        }
    }

    /// Current base URL — v3 if the fallback flag is set, otherwise v4.
    fn base_url(&self) -> &'static str {
        if self.use_v3.load(Ordering::Relaxed) {
            BASE_URL_V3
        } else {
            BASE_URL_V4
        }
    }

    /// Flip to v3 mode for the rest of this client's lifetime.
    fn switch_to_v3(&self) {
        if !self.use_v3.swap(true, Ordering::Relaxed) {
            warn!("v4 endpoint persistently failing — falling back to v3 for remainder of run");
        }
    }

    /// Build a NatStat API URL.
    ///
    /// - `endpoint`: e.g., "teams", "games;playbyplay,lineups"
    /// - `range`: optional comma-separated range params (season, date, team code, etc.)
    /// - `offset`: optional pagination offset
    fn build_url(&self, endpoint: &str, range: Option<&str>, offset: Option<u64>) -> String {
        let mut url = format!(
            "{}/{}/{}/{}",
            self.base_url(),
            self.api_key,
            endpoint,
            SERVICE
        );
        match (range, offset) {
            (Some(r), Some(o)) => url = format!("{}/{}/{}", url, r, o),
            (Some(r), None) => url = format!("{}/{}", url, r),
            (None, Some(o)) => url = format!("{}/_/{}", url, o),
            (None, None) => {}
        }
        url
    }

    /// Cache key for a request (excludes API key for safety).
    fn cache_key(endpoint: &str, range: Option<&str>, offset: Option<u64>) -> String {
        format!(
            "{}/{}:{}/{}",
            SERVICE,
            endpoint,
            range.unwrap_or("_"),
            offset.unwrap_or(0)
        )
    }

    /// Maximum number of retries for transient errors (429, 5xx).
    const MAX_RETRIES: u32 = 5;

    /// Make a single cached, rate-limited GET request to NatStat.
    ///
    /// Retries with exponential backoff on 429 (rate limit) and 5xx (server) errors.
    /// Returns the full JSON response body.
    pub async fn get(
        &self,
        endpoint: &str,
        range: Option<&str>,
        offset: Option<u64>,
        ttl: Option<i64>,
    ) -> Result<Value, NatStatError> {
        let cache_key = Self::cache_key(endpoint, range, offset);

        // Check cache first
        if let Some(cached) = self.cache.get(&cache_key, &cache_key).await? {
            info!(endpoint, "cache hit");
            return Ok(cached);
        }

        let mut url = self.build_url(endpoint, range, offset);

        let range_str = range.unwrap_or("_");
        let offset_val = offset.unwrap_or(0);

        let mut last_err = None;
        let mut attempt: u32 = 0;
        while attempt <= Self::MAX_RETRIES {
            // Rate limit + concurrency control
            let available = self.rate_limiter.available().await;
            if available < 50 {
                warn!(available, "rate limit tokens running low");
            }
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                let backoff_secs = 2u64.pow(attempt);
                info!(
                    endpoint,
                    range = range_str,
                    offset = offset_val,
                    attempt,
                    backoff_secs,
                    "retrying request"
                );
            } else {
                info!(
                    endpoint,
                    range = range_str,
                    offset = offset_val,
                    "fetching from NatStat API"
                );
            }

            let response = match self.http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    // Network errors are retryable
                    warn!(
                        endpoint,
                        range = range_str,
                        offset = offset_val,
                        attempt,
                        error = %e,
                        "network error"
                    );
                    let is_timeout = e.is_timeout() || e.is_connect();
                    last_err = Some(NatStatError::Http(e));

                    // Timeouts on v4 → switch to v3 immediately and retry without
                    // counting this against the retry budget.
                    if is_timeout && !self.use_v3.load(Ordering::Relaxed) {
                        self.switch_to_v3();
                        url = self.build_url(endpoint, range, offset);
                        continue;
                    }

                    let backoff = Duration::from_secs(2u64.pow(attempt));
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                    continue;
                }
            };

            let status = response.status();

            if status.as_u16() == 429 || status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                let backoff_secs = 2u64.pow(attempt);
                warn!(
                    endpoint,
                    range = range_str,
                    offset = offset_val,
                    attempt,
                    status = status.as_u16(),
                    backoff_secs,
                    "retryable HTTP error — backing off"
                );
                last_err = Some(NatStatError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });

                // 5xx on v4 → switch to v3 immediately and retry without
                // counting this against the retry budget. (429 still backs off
                // normally since both APIs share the same rate limit.)
                if status.is_server_error() && !self.use_v3.load(Ordering::Relaxed) {
                    self.switch_to_v3();
                    url = self.build_url(endpoint, range, offset);
                    continue;
                }

                let backoff = Duration::from_secs(backoff_secs);
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(NatStatError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });
            }

            let body: Value = response.json().await?;

            // Check for API-level errors. NatStat error responses come in two
            // shapes:
            //   1. `error: "SOME_CODE"` (string)
            //   2. `error: {"message": "OUT_OF_CALLS", "detail": "..."}` (object)
            // Additionally, `success: "0"` (string) signals failure.
            let success_flag = body
                .get("success")
                .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())));
            let error_node = body.get("error");
            let error_code = match error_node {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(Value::Object(obj)) => obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(String::from),
                _ => None,
            };
            let success_failed = matches!(success_flag.as_deref(), Some("0") | Some("false"));

            if error_code.is_some() || success_failed {
                let code = error_code.unwrap_or_else(|| "API_ERROR".to_string());
                let message = error_node
                    .and_then(|e| e.get("detail"))
                    .and_then(|d| d.as_str())
                    .map(String::from)
                    .or_else(|| {
                        body.get("meta")
                            .and_then(|m| m.get("description"))
                            .and_then(|d| d.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_default();
                error!(
                    endpoint,
                    range = range_str,
                    offset = offset_val,
                    code = %code,
                    "API error response (not caching)"
                );
                return Err(NatStatError::ApiError { code, message });
            }

            // Cache the successful response
            let ttl = ttl.unwrap_or(self.default_ttl);
            self.cache.set(&cache_key, &cache_key, &body, ttl).await?;

            return Ok(body);
        }

        // All retries exhausted
        error!(
            endpoint,
            range = range_str,
            offset = offset_val,
            max_retries = Self::MAX_RETRIES,
            "all retries exhausted"
        );
        Err(last_err.unwrap_or(NatStatError::HttpStatus {
            status: 0,
            body: format!(
                "all {} retries exhausted for {endpoint} range={range_str} offset={offset_val}",
                Self::MAX_RETRIES
            ),
        }))
    }

    /// Fetch all pages for an endpoint, returning collected results.
    ///
    /// NatStat paginates with offset (increments of max results per page, usually 100).
    /// This method follows `page-next` until all pages are fetched.
    pub async fn get_all_pages(
        &self,
        endpoint: &str,
        range: Option<&str>,
        ttl: Option<i64>,
    ) -> Result<Vec<Value>, NatStatError> {
        let mut all_results = Vec::new();
        let mut offset: Option<u64> = None;
        let mut page_num: u64 = 1;

        loop {
            let response = self.get(endpoint, range, offset, ttl).await?;

            let meta = Self::parse_meta(&response);
            let results_total = meta.results_total.unwrap_or(0);
            let pages_total = meta.pages_total.unwrap_or(1);

            info!(
                endpoint,
                range = range.unwrap_or("_"),
                page = page_num,
                pages_total,
                results_total,
                "fetched page"
            );

            all_results.push(response);

            // Stop conditions, in priority order:
            //   1. No `page-next` link.
            //   2. `pages-total` known and we've reached it (defends against
            //      v3 endpoints that hand out infinite empty `page-next` links).
            //   3. `results-total` is 0 — nothing to paginate through.
            if meta.page_next.is_none() {
                break;
            }
            if meta.pages_total.is_some() && page_num >= pages_total {
                warn!(
                    endpoint,
                    range = range.unwrap_or("_"),
                    page = page_num,
                    pages_total,
                    "stopping pagination: reached pages_total despite page-next being set"
                );
                break;
            }
            if meta.results_total == Some(0) {
                warn!(
                    endpoint,
                    range = range.unwrap_or("_"),
                    page = page_num,
                    "stopping pagination: results_total is 0"
                );
                break;
            }

            let results_max = meta.results_max.unwrap_or(100);
            let current_offset = offset.unwrap_or(0);
            offset = Some(current_offset + results_max);
            page_num += 1;
        }

        info!(
            endpoint,
            range = range.unwrap_or("_"),
            pages = all_results.len(),
            "finished fetching all pages"
        );

        Ok(all_results)
    }

    /// Parse the `meta` node from a NatStat response.
    pub fn parse_meta(response: &Value) -> ResponseMeta {
        let meta = response.get("meta");
        ResponseMeta {
            results_total: meta
                .and_then(|m| m.get("results-total"))
                .and_then(|v| v.as_u64()),
            results_max: meta
                .and_then(|m| m.get("results-max"))
                .and_then(|v| v.as_u64()),
            page: meta.and_then(|m| m.get("page")).and_then(|v| v.as_u64()),
            pages_total: meta
                .and_then(|m| m.get("pages-total"))
                .and_then(|v| v.as_u64()),
            page_next: meta
                .and_then(|m| m.get("page-next"))
                .and_then(|v| v.as_str())
                .map(String::from),
        }
    }

    /// Parse the rate limit info from a response's `user` node.
    pub fn parse_rate_limit(response: &Value) -> Option<(u64, u64)> {
        let user = response.get("user")?;
        let limit = user.get("ratelimit")?.as_u64()?;
        let remaining = user.get("ratelimit-remaining")?.as_u64()?;
        Some((limit, remaining))
    }

    /// Get the number of rate limit tokens currently available (local tracker).
    pub async fn rate_limit_remaining(&self) -> u32 {
        self.rate_limiter.available().await
    }

    /// Purge expired cache entries.
    pub async fn cleanup_cache(&self) -> Result<u64, NatStatError> {
        Ok(self.cache.cleanup_expired().await?)
    }

    /// Clear all cache entries (forces fresh API fetches).
    pub async fn clear_all_cache(&self) -> Result<u64, NatStatError> {
        Ok(self.cache.clear_all().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url_no_range_no_offset() {
        let client = NatStatClient {
            http: Client::new(),
            api_key: "test-key123".into(),
            rate_limiter: RateLimiter::new(500),
            cache: ApiCache::new_noop(),
            default_ttl: 3600,
            use_v3: AtomicBool::new(false),
        };
        let url = client.build_url("teams", None, None);
        assert_eq!(url, "https://api4.natst.at/test-key123/teams/mbb");
    }

    #[test]
    fn test_build_url_with_range() {
        let client = NatStatClient {
            http: Client::new(),
            api_key: "test-key123".into(),
            rate_limiter: RateLimiter::new(500),
            cache: ApiCache::new_noop(),
            default_ttl: 3600,
            use_v3: AtomicBool::new(false),
        };
        let url = client.build_url("games", Some("2026,DUKE"), None);
        assert_eq!(url, "https://api4.natst.at/test-key123/games/mbb/2026,DUKE");
    }

    #[test]
    fn test_build_url_with_offset_no_range() {
        let client = NatStatClient {
            http: Client::new(),
            api_key: "test-key123".into(),
            rate_limiter: RateLimiter::new(500),
            cache: ApiCache::new_noop(),
            default_ttl: 3600,
            use_v3: AtomicBool::new(false),
        };
        let url = client.build_url("players", None, Some(100));
        assert_eq!(url, "https://api4.natst.at/test-key123/players/mbb/_/100");
    }

    #[test]
    fn test_build_url_with_range_and_offset() {
        let client = NatStatClient {
            http: Client::new(),
            api_key: "test-key123".into(),
            rate_limiter: RateLimiter::new(500),
            cache: ApiCache::new_noop(),
            default_ttl: 3600,
            use_v3: AtomicBool::new(false),
        };
        let url = client.build_url("games", Some("2026"), Some(200));
        assert_eq!(url, "https://api4.natst.at/test-key123/games/mbb/2026/200");
    }

    #[test]
    fn test_build_url_hydrated_endpoint() {
        let client = NatStatClient {
            http: Client::new(),
            api_key: "test-key123".into(),
            rate_limiter: RateLimiter::new(500),
            cache: ApiCache::new_noop(),
            default_ttl: 3600,
            use_v3: AtomicBool::new(false),
        };
        let url = client.build_url("games;playbyplay,lineups", Some("2026-03-15"), None);
        assert_eq!(
            url,
            "https://api4.natst.at/test-key123/games;playbyplay,lineups/mbb/2026-03-15"
        );
    }

    #[test]
    fn test_cache_key_excludes_api_key() {
        let key = NatStatClient::cache_key("teams", None, None);
        assert!(!key.contains("test-key"));
        assert!(key.contains("teams"));
    }

    #[test]
    fn test_parse_meta() {
        let response = serde_json::json!({
            "meta": {
                "results-total": 362,
                "results-max": 100,
                "page": 1,
                "pages-total": 4,
                "page-next": "https://api4.natst.at/xxxx/teams/mbb/_/100"
            }
        });
        let meta = NatStatClient::parse_meta(&response);
        assert_eq!(meta.results_total, Some(362));
        assert_eq!(meta.pages_total, Some(4));
        assert!(meta.page_next.is_some());
    }

    #[test]
    fn test_parse_rate_limit() {
        let response = serde_json::json!({
            "user": {
                "ratelimit": 500,
                "ratelimit-remaining": 423,
                "ratelimit-timeframe": "hour"
            }
        });
        let (limit, remaining) = NatStatClient::parse_rate_limit(&response).unwrap();
        assert_eq!(limit, 500);
        assert_eq!(remaining, 423);
    }

    #[test]
    fn test_natstat_error_display() {
        let err = NatStatError::ApiError {
            code: "OUT_OF_CALLS".into(),
            message: "Rate limit exceeded".into(),
        };
        assert!(err.to_string().contains("OUT_OF_CALLS"));
    }
}
