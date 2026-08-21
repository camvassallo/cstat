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
/// immediately — the HTML shell gets an explicit `no-cache` from
/// `spa_cache_control`, and the rest fall through to `ServeDir`'s
/// `Last-Modified` revalidation (it emits no ETag).
fn is_immutable_asset_path(path: &str) -> bool {
    path.starts_with("/assets/")
}

/// Whether a response is the SPA HTML shell rather than the file that was asked
/// for. `ServeDir` is wired with `.fallback(ServeFile::new(index.html))` so a
/// path it cannot find yields `index.html` with a 200 — right for client-side
/// routes (`/predict`, `/teams/<id>`), wrong for `/assets/*`, which are real
/// files and never routes.
fn is_spa_html_fallback(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"))
}

/// The response for an `/assets/*` path that is not on disk.
///
/// `no-store` is load-bearing and not merely tidy. This 404 is reachable in the
/// same rolling-deploy window as the HTML fallback it replaces — a client
/// holding the new `index.html` whose chunk request lands on a container still
/// serving the old build — so the URL it is denying is a **live** one that will
/// resolve moments later. A cacheable error there just re-runs the original bug
/// with a shorter fuse: the edge pins a 404 on a good chunk URL, every visitor
/// behind it fails, and `RouteErrorBoundary`'s reload cannot help, because the
/// fresh `index.html` names the very URL the edge is refusing. Caches apply
/// their own default TTL to a 404 that carries no directive, so the directive
/// has to be explicit.
fn asset_miss_response() -> Response {
    let mut resp = StatusCode::NOT_FOUND.into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// The SPA's whole cache policy, in one layer over the static file service.
/// Three cases, each of which has to be explicit because `ServeDir` sets no
/// `Cache-Control` of its own:
///
/// 1. A content-hashed build asset (`/assets/*`) is immutable — the hash is the
///    cache-buster, so it can be pinned for a year.
/// 2. A MISSING `/assets/*` becomes an uncacheable 404 (see below, and
///    `asset_miss_response`).
/// 3. An HTML document that arrived WITHOUT a `Cache-Control` of its own gets
///    `no-cache` — store it, but revalidate before use — so a document can
///    never be merely heuristically cacheable (RFC 9111 §4.2.2).
///
///    Read this as a backstop, not as a live guarantee: **it does not fire in
///    production today**. Since #279 every document is rendered by
///    `meta::spa_document` -> `meta::page`, which sets `public, max-age=300` in
///    order to inject `rel="canonical"`, and this layer deliberately never
///    overwrites a header a handler already chose. So the shell IS cacheable
///    for five minutes, and the deploy hole that opens is real and tracked in
///    #276: an intermediary can serve the PREVIOUS build's HTML after a deploy,
///    every chunk URL it names then 404s by rule 2, `RouteErrorBoundary`
///    reloads straight back into the same stale document, spends its one-shot
///    guard and strands the tab — `location.reload()` bypasses the browser's
///    own cache but not an intermediary's. Do not build on rule 3 holding
///    until #276 closes it.
///
/// A missing `/assets/*` is turned into a 404 rather than being allowed through
/// as the HTML shell. Both halves of that matter, and the combination is what
/// makes it severe. `ServeDir`'s fallback answers an unknown hashed chunk with
/// `200 text/html`, and the immutable stamp below keyed only on the `/assets/`
/// prefix — so a CDN would cache `index.html` under a **live** chunk URL for a
/// year and every visitor behind that edge would get a permanently broken app.
/// Dropping only the header is not enough: Cloudflare caches `.js` by extension
/// on its own, so the response has to stop being a success. Reachable during a
/// rolling deploy, when a client holding the new `index.html` can have its chunk
/// request routed to a container still serving the old build — and route
/// code-splitting (issue #267) took the app from 2 hashed URLs fetched with the
/// document to ~35 fetched lazily, long afterwards.
pub async fn spa_cache_control(req: Request, next: Next) -> Response {
    let is_asset = is_immutable_asset_path(req.uri().path());
    let mut resp = next.run(req).await;
    if !resp.status().is_success() {
        return resp;
    }
    let is_html = is_spa_html_fallback(&resp);
    if is_asset {
        // An asset path that came back as HTML is the SPA fallback standing in
        // for a file that is not there.
        if is_html {
            return asset_miss_response();
        }
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(IMMUTABLE_ASSET_CACHE_CONTROL),
        );
    } else if is_html && !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
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
    fn spa_html_fallback_is_detected_by_content_type() {
        let with_ct = |ct: &str| {
            let mut r = StatusCode::OK.into_response();
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
            r
        };
        // A missing /assets/* falls through to index.html — the case that must
        // never be stamped immutable (see `spa_cache_control`).
        assert!(is_spa_html_fallback(&with_ct("text/html")));
        assert!(is_spa_html_fallback(&with_ct("text/html; charset=utf-8")));
        // Real build assets are served as themselves and stay cacheable.
        assert!(!is_spa_html_fallback(&with_ct("text/javascript")));
        assert!(!is_spa_html_fallback(&with_ct("text/css")));
        // No content-type at all is not a reason to reject.
        assert!(!is_spa_html_fallback(&StatusCode::OK.into_response()));
    }

    /// The middleware over a `ServeDir` + fallback, because the branches ARE
    /// the site's caching contract and the helper tests above only pin their
    /// pieces. A regression in any of them is invisible locally and then pinned
    /// at an edge — for a year, in the `immutable` case.
    ///
    /// The fallback here returns HTML with its OWN `Cache-Control`, mirroring
    /// `meta::spa_document` in `main.rs`, which since #279 renders every
    /// document through `meta::page` with `public, max-age=300` so it can
    /// inject `rel="canonical"`. That is the case production actually
    /// exercises, and asserting the layer leaves that header alone is the point
    /// — an earlier version of this test used a bare `ServeFile` and asserted
    /// the shell came back `no-cache`, which prod does not do.
    #[tokio::test]
    async fn spa_cache_control_covers_asset_hit_asset_miss_and_shell() {
        use axum::Router;
        use axum::body::Body;
        use axum::http::Request;
        use axum::response::Html;
        use axum::routing::get as get_route;
        use tower::ServiceExt;
        use tower_http::services::ServeDir;

        // Minimal stand-in for a Vite build: one hashed asset + the shell.
        let dir = std::env::temp_dir().join(format!("cstat-spa-test-{}", std::process::id()));
        let assets = dir.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("index-abc123.js"), "export default 1;").unwrap();
        std::fs::write(dir.join("index.html"), "<!doctype html><html></html>").unwrap();

        // Stands in for `meta::spa_document`: HTML, with its own header.
        let document = get_route(|| async {
            (
                [(header::CACHE_CONTROL, "public, max-age=300")],
                Html("<!doctype html><html></html>"),
            )
        });
        // ...and one that sets none, to exercise the backstop branch.
        let bare_document = get_route(|| async { Html("<!doctype html><html></html>") });

        let app: Router<()> = Router::new()
            .fallback_service(
                ServeDir::new(&dir)
                    // Mirrors main.rs. Left on, ServeDir answers `/` out of
                    // index.html itself and never reaches the handler — which is
                    // why #279 turned it off, so the homepage gets a canonical
                    // like every other document.
                    .append_index_html_on_directories(false)
                    .fallback(document),
            )
            .layer(axum::middleware::from_fn(spa_cache_control));
        let bare_app: Router<()> = Router::new()
            .fallback_service(
                ServeDir::new(&dir)
                    .append_index_html_on_directories(false)
                    .fallback(bare_document),
            )
            .layer(axum::middleware::from_fn(spa_cache_control));

        let get = |uri: &str| {
            let app = app.clone();
            let uri = uri.to_string();
            async move {
                app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap()
            }
        };
        let cc = |r: &Response| {
            r.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap().to_string())
        };

        // 1. A real hashed asset is pinned forever.
        let hit = get("/assets/index-abc123.js").await;
        assert_eq!(hit.status(), StatusCode::OK);
        assert_eq!(cc(&hit).as_deref(), Some(IMMUTABLE_ASSET_CACHE_CONTROL));

        // 2. A missing one must NOT come back as the 200-HTML shell, and must
        //    not be cacheable — the deploy window makes that URL live again.
        let miss = get("/assets/index-deadbeef.js").await;
        assert_eq!(miss.status(), StatusCode::NOT_FOUND);
        assert_eq!(cc(&miss).as_deref(), Some("no-store"));

        // 3. A document that chose its own policy keeps it — the layer must not
        //    overwrite what a handler decided. This is the live behaviour: every
        //    SPA document is `public, max-age=300` from `meta::page`. The deploy
        //    window that TTL opens on documents naming hashed assets is tracked
        //    in #276, not papered over here.
        for uri in ["/", "/predict"] {
            let shell = get(uri).await;
            assert_eq!(shell.status(), StatusCode::OK, "{uri}");
            assert_eq!(cc(&shell).as_deref(), Some("public, max-age=300"), "{uri}");
        }

        // 4. The backstop: HTML arriving with no policy of its own gets
        //    `no-cache`, so a document can never be heuristically cached.
        for uri in ["/", "/predict"] {
            let shell = bare_app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(shell.status(), StatusCode::OK, "{uri}");
            assert_eq!(cc(&shell).as_deref(), Some("no-cache"), "{uri}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn asset_miss_is_a_404_that_caches_cannot_pin() {
        let resp = asset_miss_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // The denied URL is live again as soon as the deploy settles, so an
        // edge must not be allowed to hold this answer for its default 404 TTL.
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
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
