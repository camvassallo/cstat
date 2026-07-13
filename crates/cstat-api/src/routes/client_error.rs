//! Frontend error sink: `POST /api/client-error`.
//!
//! The SPA's `window.onerror` / `unhandledrejection` reporter (see
//! `web/src/lib/errorReporter.ts`) posts uncaught browser errors here, and we
//! forward a compact summary to `#errors-web`. This is the only way a
//! client-side crash becomes visible server-side — otherwise it lives and dies
//! in the user's console.
//!
//! Hardened against abuse: the endpoint is public and unauthenticated, so every
//! field is length-capped and the whole channel is globally throttled (a single
//! looping client can't flood Slack). No-op unless `SLACK_WEBHOOK_ERRORS_WEB` is
//! set, so it's silent locally.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::{Router, http::StatusCode, response::IntoResponse, routing::post};
use cstat_ingest::notify::{self, SlackChannel};
use serde::Deserialize;
use std::sync::Arc;

use crate::AppState;

/// Minimum spacing between forwarded client-error alerts. Browser errors arrive
/// in bursts (one bad deploy hits every visitor at once), so we forward at most
/// one per window; the rest are dropped rather than queued.
const CLIENT_ERROR_COOLDOWN: Duration = Duration::from_secs(30);

/// Max characters kept from any single client-supplied field before forwarding.
/// Stacks/user-agents can be long and the body is untrusted.
const MAX_FIELD_LEN: usize = 600;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/client-error", post(report))
}

/// Uncaught-error payload from the browser reporter. Every field is optional so
/// a partial report (e.g. a cross-origin "Script error." with no stack) still
/// gets through with whatever context exists.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClientErrorReport {
    /// "error" | "unhandledrejection" — what fired the report.
    kind: String,
    /// The error message / rejection reason.
    message: String,
    /// Page URL where it happened.
    page: String,
    /// Script source file (from `onerror`).
    source: String,
    /// Stack trace, if the browser exposed one.
    stack: String,
    /// `navigator.userAgent`.
    user_agent: String,
}

async fn report(body: Option<axum::Json<ClientErrorReport>>) -> impl IntoResponse {
    // Tolerate a malformed/empty body — a broken client shouldn't get a 4xx it
    // will just retry. Take whatever parsed; default the rest.
    let r = body.map(|axum::Json(r)| r).unwrap_or_default();

    // Drop empties and floods before doing any work.
    if r.message.trim().is_empty() && r.stack.trim().is_empty() {
        return StatusCode::NO_CONTENT;
    }
    if !client_error_allowed() {
        return StatusCode::NO_CONTENT;
    }

    let kind = if r.kind.is_empty() {
        "error".to_string()
    } else {
        cap(&r.kind)
    };
    let mut msg = format!(
        ":globe_with_meridians: *cstat-web {kind}* — {message}",
        message = cap(&r.message),
    );
    if !r.page.is_empty() {
        msg.push_str(&format!("\n*Page:*  {}", cap(&r.page)));
    }
    if !r.source.is_empty() {
        msg.push_str(&format!("\n*Source:*  {}", cap(&r.source)));
    }
    if !r.stack.is_empty() {
        msg.push_str(&format!("\n```{}```", cap(&r.stack)));
    }
    if !r.user_agent.is_empty() {
        msg.push_str(&format!("\n_UA: {}_", cap(&r.user_agent)));
    }
    msg.push_str(&format!(
        "\n_(further web-error alerts throttled for {}s)_",
        CLIENT_ERROR_COOLDOWN.as_secs()
    ));

    tokio::spawn(async move { notify::post_slack(SlackChannel::ErrorsWeb, &msg).await });
    StatusCode::NO_CONTENT
}

/// Truncate an untrusted field to [`MAX_FIELD_LEN`] chars (char-boundary safe).
fn cap(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(MAX_FIELD_LEN) {
        Some((byte_idx, _)) => format!("{}…", &s[..byte_idx]),
        None => s.to_string(),
    }
}

/// Admit a forwarded client error at most once per [`CLIENT_ERROR_COOLDOWN`].
/// Same monotonic-atomic scheme as the API error tap in `guards.rs`, kept
/// separate so a web-error burst and an API-error burst don't share a budget.
fn client_error_allowed() -> bool {
    static START: OnceLock<Instant> = OnceLock::new();
    // u64::MAX = "never" — 0 is a legitimate now_ms in the first ms after start.
    static LAST_MS: AtomicU64 = AtomicU64::new(u64::MAX);
    let now_ms = START.get_or_init(Instant::now).elapsed().as_millis() as u64;
    let cooldown_ms = CLIENT_ERROR_COOLDOWN.as_millis() as u64;
    loop {
        let last = LAST_MS.load(Ordering::Relaxed);
        if last != u64::MAX && now_ms.saturating_sub(last) < cooldown_ms {
            return false;
        }
        if LAST_MS
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_truncates_long_fields_on_char_boundary() {
        let short = "hello";
        assert_eq!(cap(short), "hello");
        let long = "x".repeat(MAX_FIELD_LEN + 50);
        let capped = cap(&long);
        assert!(capped.ends_with('…'));
        assert!(capped.chars().count() <= MAX_FIELD_LEN + 1);
        // Multi-byte chars must not be split mid-codepoint.
        let emoji = "🏀".repeat(MAX_FIELD_LEN + 10);
        let _ = cap(&emoji); // must not panic
    }

    #[test]
    fn throttle_admits_first_then_suppresses() {
        assert!(client_error_allowed(), "first report admitted");
        assert!(
            !client_error_allowed(),
            "second report inside cooldown suppressed"
        );
    }
}
