//! Edge-friendly serving guards layered onto the data API routes:
//! short-TTL cache headers, a per-request timeout, and a concurrency-based
//! load-shed. Each is a small `from_fn` middleware so no extra tower-http
//! features are pulled in. Applied to the `/api` data routes only — kept off
//! `/api/health` so a saturated server can still pass its platform
//! healthcheck (and get restarted rather than silently wedged).

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use tokio::sync::Semaphore;

/// Default seconds a single request may run before it's abandoned with 408.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default max concurrent in-flight data-API requests before shedding with 503.
/// Generous — this is an abuse/runaway backstop, not fine-grained fairness;
/// the DB pool + `acquire_timeout` already serialize work past its capacity.
const DEFAULT_MAX_INFLIGHT: usize = 256;

/// `Cache-Control` served on successful data responses. Short enough that a
/// nightly ingest is visible within minutes; `stale-while-revalidate` lets a
/// CDN serve instantly while it refreshes in the background. The site is
/// read-only with no per-user state, so shared (`public`) caching is safe.
const CACHE_CONTROL_VALUE: &str = "public, max-age=300, stale-while-revalidate=600";

/// Per-request timeout (`REQUEST_TIMEOUT_SECS`, default 30s). Read once at
/// startup and threaded in as middleware state so it isn't re-parsed per
/// request.
pub fn timeout_duration() -> Duration {
    let secs = std::env::var("REQUEST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// In-flight request limiter (`MAX_INFLIGHT_REQUESTS`, default 256). Shared
/// across all data routes via middleware state.
pub fn inflight_semaphore() -> Arc<Semaphore> {
    let max = std::env::var("MAX_INFLIGHT_REQUESTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_INFLIGHT);
    Arc::new(Semaphore::new(max))
}

/// Add a short-TTL `Cache-Control` to successful responses that don't already
/// set one (so a handler can opt a route into a different TTL later). Errors
/// (4xx/5xx) are intentionally left uncached — a transient failure must not be
/// pinned at the edge.
pub async fn cache_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    if resp.status().is_success() && !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL_VALUE),
        );
    }
    resp
}

/// Abandon a request that runs past the timeout, returning 408 instead of
/// holding the connection (and its DB handle) open indefinitely. Dropping the
/// inner future cancels the in-flight query; the pool's `statement_timeout` is
/// the server-side backstop.
pub async fn enforce_timeout(State(dur): State<Duration>, req: Request, next: Next) -> Response {
    match tokio::time::timeout(dur, next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(json!({ "error": "request timed out" })),
        )
            .into_response(),
    }
}

/// Shed load past `MAX_INFLIGHT_REQUESTS` with 503 + `Retry-After`, so a flood
/// returns fast instead of piling up against the connection pool. The permit
/// is held for the request's lifetime and released when it completes.
pub async fn load_shed(State(sem): State<Arc<Semaphore>>, req: Request, next: Next) -> Response {
    match sem.try_acquire() {
        Ok(_permit) => next.run(req).await,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, HeaderValue::from_static("2"))],
            Json(json!({ "error": "server busy, please retry shortly" })),
        )
            .into_response(),
    }
}
