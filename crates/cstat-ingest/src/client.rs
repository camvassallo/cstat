use crate::cache::ApiCache;
use crate::rate_limiter::RateLimiter;
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

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

const BASE_URL: &str = "https://api4.natst.at";
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
        }
    }

    /// Build a NatStat API URL.
    ///
    /// - `endpoint`: e.g., "teams", "games;playbyplay,lineups"
    /// - `range`: optional comma-separated range params (season, date, team code, etc.)
    /// - `offset`: optional pagination offset
    fn build_url(&self, endpoint: &str, range: Option<&str>, offset: Option<u64>) -> String {
        let mut url = format!("{}/{}/{}/{}", BASE_URL, self.api_key, endpoint, SERVICE);
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

        let url = self.build_url(endpoint, range, offset);

        let mut last_err = None;
        for attempt in 0..=Self::MAX_RETRIES {
            // Rate limit + concurrency control
            let available = self.rate_limiter.available().await;
            if available < 50 {
                warn!(available, "rate limit tokens running low");
            }
            self.rate_limiter.acquire().await;

            if attempt > 0 {
                info!(endpoint, attempt, "retrying request");
            } else {
                info!(endpoint, "fetching from NatStat API");
            }

            let response = match self.http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    // Network errors are retryable
                    warn!(endpoint, attempt, error = %e, "request failed");
                    last_err = Some(NatStatError::Http(e));
                    let backoff = Duration::from_secs(2u64.pow(attempt));
                    tokio::time::sleep(backoff).await;
                    continue;
                }
            };

            let status = response.status();

            if status.as_u16() == 429 || status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                warn!(endpoint, attempt, status = status.as_u16(), "retryable HTTP error");
                last_err = Some(NatStatError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });
                let backoff = Duration::from_secs(2u64.pow(attempt));
                tokio::time::sleep(backoff).await;
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

            // Check for API-level errors in response
            if let Some(error) = body.get("error").and_then(|e| e.as_str())
                && !error.is_empty()
            {
                return Err(NatStatError::ApiError {
                    code: error.to_string(),
                    message: body
                        .get("meta")
                        .and_then(|m| m.get("description"))
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }

            // Cache the successful response
            let ttl = ttl.unwrap_or(self.default_ttl);
            self.cache.set(&cache_key, &cache_key, &body, ttl).await?;

            return Ok(body);
        }

        // All retries exhausted
        Err(last_err.unwrap_or(NatStatError::HttpStatus {
            status: 0,
            body: "all retries exhausted".to_string(),
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

        loop {
            let response = self.get(endpoint, range, offset, ttl).await?;
            all_results.push(response.clone());

            // Check if there are more pages
            let has_next = response
                .get("meta")
                .and_then(|m| m.get("page-next"))
                .and_then(|p| p.as_str())
                .is_some();

            if !has_next {
                break;
            }

            // Extract current page info to compute next offset
            let results_max = response
                .get("meta")
                .and_then(|m| m.get("results-max"))
                .and_then(|v| v.as_u64())
                .unwrap_or(100);

            let current_offset = offset.unwrap_or(0);
            offset = Some(current_offset + results_max);
        }

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
