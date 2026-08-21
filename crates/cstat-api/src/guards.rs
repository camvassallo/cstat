//! Edge-friendly serving guards layered onto the data API routes:
//! short-TTL cache headers, a per-request timeout, and a concurrency-based
//! load-shed. Each is a small `from_fn` middleware so no extra tower-http
//! features are pulled in. Applied to the `/api` data routes only — kept off
//! `/api/health` so a saturated server can still pass its platform
//! healthcheck (and get restarted rather than silently wedged).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use cstat_ingest::notify::{self, SlackChannel};
use serde_json::json;
use tokio::sync::Semaphore;

/// Minimum spacing between `#errors-api` alerts. A crash loop or a burst of 5xx
/// would otherwise flood the channel; one alert per window is enough to prompt a
/// look at the logs, and the throttle is shared across the 5xx tap and the panic
/// hook so a panic that also produces a 5xx doesn't double-post.
const ERROR_ALERT_COOLDOWN: Duration = Duration::from_secs(60);

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

/// `Cache-Control` for content-hashed SPA build assets — cache effectively
/// forever. Safe because the filename hash changes whenever the content does,
/// so a new build is a new URL (cache-busting is automatic).
const IMMUTABLE_ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

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

/// Whether a response with this status should receive the default
/// `Cache-Control`. Pure so the contract — cache only 2xx, never override a
/// header a handler already set — is unit-testable without a router harness.
fn should_set_cache_header(status: StatusCode, already_present: bool) -> bool {
    status.is_success() && !already_present
}

/// Add a short-TTL `Cache-Control` to successful responses that don't already
/// set one (so a handler can opt a route into a different TTL later). Errors
/// (4xx/5xx) are intentionally left uncached — a transient failure must not be
/// pinned at the edge.
pub async fn cache_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    if should_set_cache_header(
        resp.status(),
        resp.headers().contains_key(header::CACHE_CONTROL),
    ) {
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

/// Whether a request path serves a content-hashed, immutable build asset. Vite
/// emits `/assets/<name>-<hash>.{js,css}`; the hash changes on every content
/// change, so these are safe to cache forever. `index.html`, `/favicon.png`,
/// and other un-hashed files are deliberately excluded so a deploy is picked up
/// immediately (they fall through to `ServeDir`'s ETag/Last-Modified
/// revalidation instead).
fn is_immutable_asset_path(path: &str) -> bool {
    path.starts_with("/assets/")
}

/// Long-cache content-hashed SPA build assets (`/assets/*`). Applied app-wide
/// (outermost) so it wraps the static fallback service; a no-op on every other
/// path, including `/api/*` (which carry their own short-TTL `Cache-Control`).
pub async fn static_asset_cache(req: Request, next: Next) -> Response {
    let is_asset = is_immutable_asset_path(req.uri().path());
    let mut resp = next.run(req).await;
    if is_asset && resp.status().is_success() {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(IMMUTABLE_ASSET_CACHE_CONTROL),
        );
    }
    resp
}

// ---------------------------------------------------------------------------
// #errors-api alerting: a 5xx response tap + a process panic hook. Each alert
// source gets its OWN throttle instance so a burst on one (e.g. transient 5xx)
// can't swallow the alert for a distinct, more severe fault (a panic).
// ---------------------------------------------------------------------------

/// A lock-free "admit at most once per cooldown" gate, shared by every alert
/// source (5xx tap, panic hook, and the `#errors-web` client-error sink) so the
/// subtle throttle mechanism lives in exactly one place. Construct one `static`
/// per independent budget.
///
/// Stores millis-since-first-use of the last admitted alert in an atomic; a CAS
/// makes racing callers agree on who wins the window. `u64::MAX` is the "never
/// admitted" sentinel — `0` can't be, since the first call's elapsed-millis is
/// legitimately `0` in the first millisecond after start.
pub(crate) struct AlertThrottle {
    epoch: OnceLock<Instant>,
    last_ms: AtomicU64,
    cooldown: Duration,
}

impl AlertThrottle {
    pub(crate) const fn new(cooldown: Duration) -> Self {
        Self {
            epoch: OnceLock::new(),
            last_ms: AtomicU64::new(u64::MAX),
            cooldown,
        }
    }

    /// Returns true at most once per `cooldown`; false while inside the window.
    pub(crate) fn allow(&self) -> bool {
        let now_ms = self.epoch.get_or_init(Instant::now).elapsed().as_millis() as u64;
        let cooldown_ms = self.cooldown.as_millis() as u64;
        loop {
            let last = self.last_ms.load(Ordering::Relaxed);
            if last != u64::MAX && now_ms.saturating_sub(last) < cooldown_ms {
                return false;
            }
            if self
                .last_ms
                .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }
}

/// Throttle for the 5xx response tap.
static ERROR_5XX_THROTTLE: AlertThrottle = AlertThrottle::new(ERROR_ALERT_COOLDOWN);
/// Separate throttle for panics, so a panic is never suppressed by an unrelated
/// 5xx that happened to fire within the cooldown (a panic is the more severe
/// signal and must get through).
static PANIC_THROTTLE: AlertThrottle = AlertThrottle::new(ERROR_ALERT_COOLDOWN);

/// Substrings that, if present in a query-param **key** (case-insensitive), cause
/// its value to be redacted before an error alert is posted — so a secret that
/// rode in on the URL never lands in Slack. Substring (not exact) match so
/// compound keys like `access_token` / `api_key` / `x-auth-token` are caught.
/// Erring toward over-redaction is deliberate: dropping a benign value from an
/// alert is harmless, leaking a credential is not.
const REDACT_QUERY_SUBSTRINGS: &[&str] = &[
    "token",
    "key",
    "secret",
    "password",
    "passwd",
    "pwd",
    "apikey",
    "jwt",
    "credential",
    "bearer",
    "session",
    "auth",
    "sig",
    "otp",
    "passphrase",
    // Deliberately NOT "code" — it's a common benign param here (team/conf codes).
];

/// The request target (path + query) for an error alert, so the failing request
/// is reproducible from the message — with sensitive query values redacted (see
/// [`REDACT_QUERY_SUBSTRINGS`]). Returns just the path when there's no query.
fn alert_target(uri: &axum::http::Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };
    let redacted = query
        .split('&')
        .map(|pair| {
            let key = pair.split_once('=').map(|(k, _)| k).unwrap_or(pair);
            let lower = key.to_ascii_lowercase();
            if REDACT_QUERY_SUBSTRINGS.iter().any(|s| lower.contains(s)) {
                format!("{key}=<redacted>")
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{redacted}")
}

/// Tap responses and alert `#errors-api` on a 5xx — a genuine server fault.
/// Deliberately ignores 4xx (client errors: bad params, 404s) and the two
/// *intentional* backpressure statuses that are technically 5xx-adjacent: 503
/// load-shed (this file) is deliberate, and 408 timeout is a 4xx anyway. The
/// post is spawned + throttled so it never adds latency to the request path and
/// a flood can't spam Slack. No-op unless `SLACK_WEBHOOK_ERRORS_API` is set.
///
/// The alert includes the full request target (`{method} {path}?{query}`,
/// secrets redacted) so the failing request is reproducible. The method + URI
/// are cheap to clone (small enum / refcounted `Bytes`); the target string is
/// only built on the rare 5xx branch, so the >99.9% success path allocates
/// nothing here.
pub async fn error_alert(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let resp = next.run(req).await;
    let status = resp.status();
    if status.is_server_error()
        && status != StatusCode::SERVICE_UNAVAILABLE
        && ERROR_5XX_THROTTLE.allow()
    {
        let msg = format!(
            ":rotating_light: *cstat-api {status}* on `{method} {target}` \
             _(further error alerts throttled for {}s)_",
            ERROR_ALERT_COOLDOWN.as_secs(),
            target = alert_target(&uri),
        );
        tokio::spawn(async move { notify::post_slack(SlackChannel::ErrorsApi, &msg).await });
    }
    resp
}

/// Install a panic hook that forwards an unexpected panic (in a request handler
/// or anywhere) to `#errors-api`, in addition to the default backtrace logging.
/// Uses its own throttle (see [`PANIC_THROTTLE`]). Call once at startup, before
/// serving.
pub fn install_panic_alert_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Keep the default behaviour (backtrace → stderr/logs) first.
        prev(info);
        if !PANIC_THROTTLE.allow() {
            return;
        }
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let msg = format!(":rotating_light: *cstat-api panic* at `{location}` — {payload}");
        // The hook is sync; hand the post to the runtime if one is live (it is,
        // during request handling). Best-effort — a panic during shutdown may
        // have no runtime to spawn onto, which is fine.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { notify::post_slack(SlackChannel::ErrorsApi, &msg).await });
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_only_successful_responses_without_an_existing_header() {
        // 2xx with no header → set it.
        assert!(should_set_cache_header(StatusCode::OK, false));
        assert!(should_set_cache_header(StatusCode::NO_CONTENT, false));
        // Never override a header a handler already set (per-route TTL wins).
        assert!(!should_set_cache_header(StatusCode::OK, true));
        // Never cache errors — a transient 4xx/5xx must not be pinned at the edge.
        assert!(!should_set_cache_header(StatusCode::NOT_FOUND, false));
        assert!(!should_set_cache_header(
            StatusCode::SERVICE_UNAVAILABLE,
            false
        ));
        assert!(!should_set_cache_header(
            StatusCode::INTERNAL_SERVER_ERROR,
            false
        ));
        assert!(!should_set_cache_header(StatusCode::REQUEST_TIMEOUT, false));
    }

    #[test]
    fn only_hashed_asset_paths_are_immutable() {
        // Content-hashed build assets → cache forever.
        assert!(is_immutable_asset_path("/assets/index-BgU49MO1.js"));
        assert!(is_immutable_asset_path("/assets/index-Bg26EKn4.css"));
        // Un-hashed files must stay revalidatable so deploys are picked up.
        assert!(!is_immutable_asset_path("/"));
        assert!(!is_immutable_asset_path("/index.html"));
        assert!(!is_immutable_asset_path("/favicon.png"));
        // API paths carry their own short-TTL header, not the immutable one.
        assert!(!is_immutable_asset_path("/api/teams/rankings"));
    }

    #[test]
    fn alert_throttle_admits_first_then_suppresses() {
        // First candidate in a fresh window is admitted; an immediate follow-up
        // (inside the cooldown) is suppressed.
        let throttle = AlertThrottle::new(Duration::from_secs(60));
        assert!(throttle.allow(), "first alert should be admitted");
        assert!(
            !throttle.allow(),
            "a second alert inside the cooldown must be suppressed"
        );
    }

    #[test]
    fn alert_target_includes_query_and_redacts_secrets() {
        let uri = |s: &str| s.parse::<axum::http::Uri>().unwrap();
        // Query is kept so the failing request is reproducible.
        assert_eq!(
            alert_target(&uri("/api/predict?home=Gonzaga&away=Utah+Tech&season=2021")),
            "/api/predict?home=Gonzaga&away=Utah+Tech&season=2021"
        );
        // No query → just the path.
        assert_eq!(alert_target(&uri("/api/health")), "/api/health");
        // Secret values are redacted (case-insensitive key), non-secrets kept.
        assert_eq!(
            alert_target(&uri("/api/alert-selftest?channel=api&Token=abc123")),
            "/api/alert-selftest?channel=api&Token=<redacted>"
        );
        // Compound / aliased credential keys are caught by substring match.
        assert_eq!(
            alert_target(&uri(
                "/x?access_token=a&api_key=b&authorization=c&home=Duke"
            )),
            "/x?access_token=<redacted>&api_key=<redacted>&authorization=<redacted>&home=Duke"
        );
    }

    #[test]
    fn alert_throttle_instances_have_independent_budgets() {
        // A burst on one source must not suppress an alert on another (the panic-
        // vs-5xx separation): distinct instances don't share a window.
        let a = AlertThrottle::new(Duration::from_secs(60));
        let b = AlertThrottle::new(Duration::from_secs(60));
        assert!(a.allow());
        assert!(!a.allow());
        assert!(b.allow(), "a separate throttle has its own budget");
    }

    #[test]
    fn load_shed_semaphore_hands_out_then_refuses() {
        // Mirrors `load_shed`'s admission test: permits are granted up to the
        // cap, then `try_acquire` fails (→ 503) until one is released.
        let sem = Semaphore::new(2);
        let p1 = sem.try_acquire().expect("permit 1");
        let _p2 = sem.try_acquire().expect("permit 2");
        assert!(sem.try_acquire().is_err(), "should shed past capacity");
        drop(p1);
        assert!(sem.try_acquire().is_ok(), "slot frees on release");
    }
}
