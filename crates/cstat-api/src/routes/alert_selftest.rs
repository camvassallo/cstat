//! Ops self-test for the error-alert pipeline: `GET /api/alert-selftest`.
//!
//! Posts a clearly-labelled synthetic message to a chosen error channel so an
//! operator (or a periodic monitor) can confirm the alert path — webhook
//! reachable + env var set — stays healthy **without** waiting for a real fault.
//! This matters once the only known 500 is fixed: otherwise there'd be no way to
//! spot-check that `#errors-api` still fires.
//!
//! Token-gated so it isn't a public spam vector, and returns **404** (not 401/
//! 403) when the token is unset or wrong, so the endpoint's existence isn't
//! leaked to an unauthenticated prober. Mounted un-guarded (like the health
//! routes) in `main.rs`.

use axum::{
    Router,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use cstat_ingest::notify::{self, SlackChannel};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

/// Env var holding the shared secret required to invoke the self-test. Unset →
/// the endpoint is effectively disabled (always 404).
const TOKEN_ENV: &str = "ALERT_SELFTEST_TOKEN";

#[derive(Debug, Deserialize)]
struct SelfTestParams {
    token: Option<String>,
    /// `api` (default) or `web` — which error channel to exercise.
    channel: Option<String>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/alert-selftest", get(selftest))
}

async fn selftest(Query(p): Query<SelfTestParams>) -> impl IntoResponse {
    // Gate on the configured token; 404 on any miss so we never confirm the
    // endpoint exists to an unauthenticated caller.
    let configured_token = std::env::var(TOKEN_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let authorized = configured_token
        .as_deref()
        .is_some_and(|expected| p.token.as_deref() == Some(expected));
    if !authorized {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })));
    }

    let (channel, name) = match p.channel.as_deref() {
        Some("web") => (SlackChannel::ErrorsWeb, "errors-web"),
        _ => (SlackChannel::ErrorsApi, "errors-api"),
    };

    // Report whether the channel's webhook is even set, so a monitor can assert
    // on the response body rather than having to eyeball Slack.
    let webhook_configured = channel.is_configured();
    notify::post_slack(
        channel,
        &format!(
            ":test_tube: *cstat alert self-test* — synthetic check of the {name} pipeline \
             (no real fault). Seeing this means alerting is healthy."
        ),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "posted": true,
            "channel": name,
            "webhook_configured": webhook_configured,
        })),
    )
}
