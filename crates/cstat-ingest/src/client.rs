use crate::cache::ApiCache;
use crate::rate_limiter::RateLimiter;
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum NatStatError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API returned error status {status}: {body}")]
    ApiError { status: u16, body: String },

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// NatStat API client with built-in rate limiting and response caching.
pub struct NatStatClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    rate_limiter: RateLimiter,
    cache: ApiCache,
    /// Default cache TTL in seconds (default: 6 hours).
    pub default_ttl: i64,
}

impl NatStatClient {
    /// Create a new client. `max_per_hour` defaults to 500 for NatStat.
    pub fn new(
        pool: PgPool,
        base_url: impl Into<String>,
        api_key: Option<String>,
        max_per_hour: u32,
    ) -> Self {
        Self {
            http: Client::builder()
                .user_agent("cstat/0.1.0")
                .gzip(true)
                .build()
                .expect("failed to build HTTP client"),
            base_url: base_url.into(),
            api_key,
            rate_limiter: RateLimiter::new(max_per_hour),
            cache: ApiCache::new(pool),
            default_ttl: 6 * 3600, // 6 hours
        }
    }

    /// Make a cached, rate-limited GET request to a NatStat endpoint.
    ///
    /// - `endpoint`: path relative to base URL (e.g., "/api/v4/players")
    /// - `params`: query string for cache key and URL
    /// - `ttl`: optional override for cache TTL in seconds
    pub async fn get(
        &self,
        endpoint: &str,
        params: &str,
        ttl: Option<i64>,
    ) -> Result<Value, NatStatError> {
        // Check cache first
        if let Some(cached) = self.cache.get(endpoint, params).await? {
            info!(endpoint, "cache hit");
            return Ok(cached);
        }

        // Rate limit before making the actual request
        let available = self.rate_limiter.available().await;
        if available < 50 {
            warn!(available, "rate limit tokens running low");
        }
        self.rate_limiter.acquire().await;

        // Build URL
        let url = if params.is_empty() {
            format!("{}{}", self.base_url, endpoint)
        } else {
            format!("{}{}?{}", self.base_url, endpoint, params)
        };

        // Add API key if configured
        let mut request = self.http.get(&url);
        if let Some(ref key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        info!(endpoint, "fetching from NatStat API");
        let response = request.send().await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(NatStatError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let body: Value = response.json().await?;

        // Cache the response
        let ttl = ttl.unwrap_or(self.default_ttl);
        self.cache.set(endpoint, params, &body, ttl).await?;

        Ok(body)
    }

    /// Get the number of rate limit tokens currently available.
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
    fn test_natstat_error_display() {
        let err = NatStatError::ApiError {
            status: 429,
            body: "rate limited".into(),
        };
        assert!(err.to_string().contains("429"));
    }
}
