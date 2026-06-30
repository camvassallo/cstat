//! Out-of-band notifications fired by the nightly orchestrator: Slack failure
//! alerts and an optional edge cache-purge. Both are **fail-soft** side effects
//! — a failure to notify must never abort (or fail) the ingest it observes. The
//! pipeline's job is to refresh data; telling someone about it is best-effort.
//!
//! Configuration is entirely env-driven so the same binary is a no-op locally
//! (no webhook configured) and alerting in prod (webhook set on the Railway
//! cron service):
//!   - `INGEST_ALERT_WEBHOOK`   — Slack incoming-webhook URL. Absent → no alerts.
//!   - `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` — Cloudflare zone + scoped API
//!     token. Both absent → no purge (the 5-min `Cache-Control` TTL still makes
//!     fresh data land within minutes; the purge just makes it instant).

use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};

/// HTTP timeout for every notification call. Notifications are best-effort, so
/// we never want a stalled Slack/Cloudflare socket to hold the run open.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Slack incoming-webhook URL from `INGEST_ALERT_WEBHOOK`, if configured.
pub fn alert_webhook_from_env() -> Option<String> {
    non_empty_env("INGEST_ALERT_WEBHOOK")
}

/// Post a message to the configured Slack webhook. No-op (and `Ok`-equivalent)
/// when no webhook is set. Never panics, never propagates an error — a failed
/// alert is logged and swallowed.
pub async fn post_slack_alert(text: &str) {
    let Some(webhook) = alert_webhook_from_env() else {
        // No webhook configured (e.g. local runs) — surface the would-be alert
        // in the logs so it isn't lost, then return.
        info!(alert = %text, "INGEST_ALERT_WEBHOOK unset; skipping Slack alert");
        return;
    };

    let client = match reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build Slack HTTP client; skipping alert");
            return;
        }
    };

    match client
        .post(&webhook)
        .json(&json!({ "text": text }))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            info!("posted ingest alert to Slack");
        }
        Ok(resp) => {
            warn!(status = %resp.status(), "Slack alert returned non-success; continuing");
        }
        Err(e) => {
            warn!(error = %e, "failed to post Slack alert; continuing");
        }
    }
}

/// Purge the Cloudflare edge cache for the configured zone after a successful
/// nightly compute, so fresh rankings/predictions propagate instantly rather
/// than within the 5-minute `Cache-Control` TTL. No-op when `CF_ZONE_ID` /
/// `CF_CACHE_PURGE_TOKEN` are unset (the TTL alone keeps the site correct, just
/// not instant). Fail-soft.
pub async fn purge_edge_cache() {
    let (Some(zone), Some(token)) = (
        non_empty_env("CF_ZONE_ID"),
        non_empty_env("CF_CACHE_PURGE_TOKEN"),
    ) else {
        info!(
            "CF_ZONE_ID/CF_CACHE_PURGE_TOKEN unset; skipping edge cache purge (5-min TTL still applies)"
        );
        return;
    };

    let client = match reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build Cloudflare HTTP client; skipping purge");
            return;
        }
    };

    let url = format!("https://api.cloudflare.com/client/v4/zones/{zone}/purge_cache");
    match client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({ "purge_everything": true }))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => info!("purged Cloudflare edge cache"),
        Ok(resp) => {
            warn!(status = %resp.status(), "Cloudflare purge returned non-success; continuing")
        }
        Err(e) => warn!(error = %e, "failed to purge Cloudflare cache; continuing"),
    }
}

/// Read an env var, treating a present-but-empty value as absent. Railway and
/// other platforms sometimes inject empty strings for unset config.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
