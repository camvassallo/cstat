//! Ops self-test for the error-alert pipeline: `GET /api/alert-selftest`.
//!
//! Posts a clearly-labelled synthetic message to a chosen error channel so an
//! operator (or a periodic monitor) can confirm the alert path — webhook
//! reachable + env var set — stays healthy **without** waiting for a real fault.
//! This matters once the only known 500 is fixed: otherwise there'd be no way to
//! spot-check that `#errors-api` still fires.
//!
//! Security posture:
//! - **Token via the `X-Selftest-Token` header, not the URL** — a query token
//!   would be logged verbatim by `TraceLayer` on every call. Compared in
//!   constant time so response latency can't leak the secret byte-by-byte.
//! - Returns **404** (not 401/403) when the token is unset or wrong, so the
//!   endpoint's existence isn't leaked to an unauthenticated prober.
//! - **Always responds 200** on an authorized call (outcome is in the body, not
//!   the status): a 5xx here would itself trip the `#errors-api` 5xx tap and
//!   alert on the self-test. A monitor should assert on `posted == true`.
//!
//! Mounted un-guarded (like the health routes) in `main.rs`.

use axum::{
    Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
};
use cstat_ingest::notify::{self, SlackChannel, SlackPostOutcome};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

/// Env var holding the shared secret required to invoke the self-test. Unset →
/// the endpoint is effectively disabled (always 404).
const TOKEN_ENV: &str = "ALERT_SELFTEST_TOKEN";
/// Header carrying the caller's token (kept out of the URL so it isn't logged).
const TOKEN_HEADER: &str = "x-selftest-token";

#[derive(Debug, Deserialize)]
struct SelfTestParams {
    /// `api` (default) or `web` — which error channel to exercise. Non-secret,
    /// so it's fine in the query string.
    channel: Option<String>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/alert-selftest", get(selftest))
}

/// Constant-time byte comparison — avoids leaking the token via response-timing
/// differences on a byte-by-byte guess. (Length is allowed to differ fast; the
/// secret's length isn't the sensitive part.)
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn selftest(headers: HeaderMap, Query(p): Query<SelfTestParams>) -> impl IntoResponse {
    // Gate on the configured token; 404 on any miss so we never confirm the
    // endpoint exists to an unauthenticated caller.
    let configured_token = std::env::var(TOKEN_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let presented = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok());
    let authorized = match (configured_token.as_deref(), presented) {
        (Some(expected), Some(got)) => constant_time_eq(expected.as_bytes(), got.as_bytes()),
        _ => false,
    };
    if !authorized {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })));
    }

    let (channel, name) = match p.channel.as_deref() {
        Some("web") => (SlackChannel::ErrorsWeb, "errors-web"),
        _ => (SlackChannel::ErrorsApi, "errors-api"),
    };

    // Use the *checked* post so we report whether the message actually landed —
    // a fire-and-forget post would let this endpoint claim success on a silently
    // failed webhook, defeating its whole purpose.
    let outcome = notify::post_slack_checked(
        channel,
        &format!(
            ":test_tube: *cstat alert self-test* — synthetic check of the {name} pipeline \
             (no real fault). Seeing this means alerting is healthy."
        ),
    )
    .await;

    let (posted, detail) = match outcome {
        SlackPostOutcome::Sent => (true, "sent"),
        SlackPostOutcome::NotConfigured => (false, "webhook env var not set for this channel"),
        SlackPostOutcome::Failed => (
            false,
            "webhook is set but the post failed — see server logs",
        ),
    };

    // Always 200 (see module docs): the outcome is in the body so a monitor
    // asserts on `posted`, and a non-2xx here would trip the 5xx tap.
    (
        StatusCode::OK,
        Json(json!({
            "posted": posted,
            "channel": name,
            "webhook_configured": channel.is_configured(),
            "detail": detail,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_std_equality() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-toker"));
        assert!(!constant_time_eq(b"secret", b"secret-token")); // length mismatch
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
