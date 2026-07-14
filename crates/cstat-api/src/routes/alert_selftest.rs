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
//! - Returns a uniform **404** (never a 400/401) when the token is unset or
//!   wrong, regardless of query — an unauthorized caller always gets the same
//!   response, giving away nothing beyond that `/api/*` is handled here.
//! - **Always responds 200** on an authorized call (outcome is in the body, not
//!   the status): a 5xx here would itself trip the `#errors-api` 5xx tap and
//!   alert on the self-test. A monitor should assert on `posted == true`.
//!
//! Mounted un-guarded (like the health routes) in `main.rs`.

use axum::{
    Router,
    extract::RawQuery,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
};
use cstat_ingest::notify::{self, SlackChannel, SlackPostOutcome};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Deserialize)]
struct SelfTestParams {
    /// `api` (default) or `web` — which error channel to exercise.
    channel: Option<String>,
}

/// Env var holding the shared secret required to invoke the self-test. Unset →
/// the endpoint is effectively disabled (always 404).
const TOKEN_ENV: &str = "ALERT_SELFTEST_TOKEN";
/// Header carrying the caller's token (kept out of the URL so it isn't logged).
const TOKEN_HEADER: &str = "x-selftest-token";

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/alert-selftest", get(selftest))
}

/// Extract the `channel` value from a raw query string. Parsed here (rather than
/// via a typed `Query` extractor in the handler signature) so a malformed query
/// can't produce a 400 *before* the token gate — an unauthorized caller must
/// always get the same 404, never a 400 that confirms the route exists. Uses
/// `serde_urlencoded` so percent-encoded values decode correctly (a hand-rolled
/// substring match would send `%77eb` to the wrong channel); a parse error just
/// yields `None` (→ default channel), never an error to the caller.
fn channel_from_query(raw: Option<&str>) -> Option<String> {
    serde_urlencoded::from_str::<SelfTestParams>(raw?)
        .ok()?
        .channel
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

async fn selftest(headers: HeaderMap, RawQuery(raw_query): RawQuery) -> impl IntoResponse {
    // Gate on the configured token; 404 on any miss so an unauthorized caller
    // gets a uniform response. Trim the env value so a secret set with a trailing
    // newline (common with file/secret-manager injection) still matches the token
    // the caller sends — the enabled-check trims, so the compare must too.
    let configured_token = std::env::var(TOKEN_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let presented = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok());
    let authorized = match (configured_token.as_deref(), presented) {
        (Some(expected), Some(got)) => constant_time_eq(expected.as_bytes(), got.as_bytes()),
        _ => false,
    };
    if !authorized {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })));
    }

    let (channel, name) = match channel_from_query(raw_query.as_deref()).as_deref() {
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

    // Derive both flags from the single source of truth (the post outcome) rather
    // than re-reading the env — avoids a redundant lookup and a possible disagree.
    let (posted, webhook_configured, detail) = match outcome {
        SlackPostOutcome::Sent => (true, true, "sent"),
        SlackPostOutcome::NotConfigured => {
            (false, false, "webhook env var not set for this channel")
        }
        SlackPostOutcome::Failed => (
            false,
            true,
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
            "webhook_configured": webhook_configured,
            "detail": detail,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parsed_from_raw_query() {
        assert_eq!(
            channel_from_query(Some("channel=web")).as_deref(),
            Some("web")
        );
        assert_eq!(
            channel_from_query(Some("foo=1&channel=api&bar=2")).as_deref(),
            Some("api")
        );
        assert_eq!(channel_from_query(Some("nothing=here")), None);
        assert_eq!(channel_from_query(None), None);
        // Percent-encoded values decode correctly (not sent to the wrong channel).
        assert_eq!(
            channel_from_query(Some("channel=%77eb")).as_deref(),
            Some("web")
        );
        // A malformed query yields None (default channel), never a 400.
        assert_eq!(channel_from_query(Some("%ZZ")), None);
    }

    #[test]
    fn constant_time_eq_matches_std_equality() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-toker"));
        assert!(!constant_time_eq(b"secret", b"secret-token")); // length mismatch
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
