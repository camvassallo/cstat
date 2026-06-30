//! Out-of-band notifications fired by the nightly orchestrator: Slack run
//! notifications (success / degraded / aborted) and an optional edge cache-purge.
//! Both are **fail-soft** side effects — a failure to notify must never abort (or
//! fail) the ingest it observes. The pipeline's job is to refresh data; telling
//! someone about it is best-effort.
//!
//! Configuration is entirely env-driven so the same binary is a no-op locally
//! (no webhook configured) and posting in prod (webhook set on the Railway
//! services):
//!   - one Slack incoming-webhook URL **per channel** — see [`SlackChannel`].
//!     A Slack incoming webhook is locked to the single channel it was created
//!     for, so routing to a different channel means a different webhook URL in a
//!     different env var, not a `channel` field on the payload.
//!   - `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` — Cloudflare zone + scoped API
//!     token. Both absent → no purge (the 5-min `Cache-Control` TTL still makes
//!     fresh data land within minutes; the purge just makes it instant).

use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};

/// HTTP timeout for every notification call. Notifications are best-effort, so
/// we never want a stalled Slack/Cloudflare socket to hold the run open.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// A logical Slack destination. Each maps to its own incoming-webhook URL in
/// env, because a Slack webhook is bound to exactly one channel.
///
/// **To add a channel** (e.g. `#errors-api`): add a variant here, map it to a
/// new `SLACK_WEBHOOK_*` env var in [`SlackChannel::env_var`], document it in
/// `.env.example`, and create the webhook + env var on the relevant service.
/// Nothing else changes — call sites just pass the new variant to [`post_slack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackChannel {
    /// `#cron-job-alerts` — nightly ingest run notifications (success heartbeat /
    /// degraded / aborted). Set on the Railway cron service.
    Cron,
    // Future buckets, wired the day their producer exists:
    //   ErrorsApi,  // #errors-api — cstat-api runtime errors  → SLACK_WEBHOOK_ERRORS_API
    //   ErrorsWeb,  // #errors-web — frontend error reports     → SLACK_WEBHOOK_ERRORS_WEB
}

impl SlackChannel {
    /// Primary env var holding this channel's incoming-webhook URL.
    pub fn env_var(self) -> &'static str {
        match self {
            SlackChannel::Cron => "SLACK_WEBHOOK_CRON",
        }
    }

    /// Resolve the webhook URL for this channel from env, if configured.
    /// `Cron` also honours the legacy `INGEST_ALERT_WEBHOOK` name so an existing
    /// deployment keeps working after the rename.
    fn webhook(self) -> Option<String> {
        non_empty_env(self.env_var()).or_else(|| match self {
            SlackChannel::Cron => non_empty_env("INGEST_ALERT_WEBHOOK"),
        })
    }
}

/// Post a message to a Slack channel (used for success heartbeats as well as
/// failure alerts). No-op when that channel's webhook env var is unset. Never
/// panics, never propagates an error — a failed post is logged and swallowed.
pub async fn post_slack(channel: SlackChannel, text: &str) {
    let Some(webhook) = channel.webhook() else {
        // No webhook configured (e.g. local runs) — surface the would-be message
        // in the logs so it isn't lost, then return.
        info!(message = %text, env = channel.env_var(), "Slack webhook unset; skipping post");
        return;
    };

    let client = match reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build Slack HTTP client; skipping post");
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
            info!(channel = ?channel, "posted notification to Slack");
        }
        Ok(resp) => {
            warn!(status = %resp.status(), "Slack post returned non-success; continuing");
        }
        Err(e) => {
            warn!(error = %e, "failed to post to Slack; continuing");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_env_vars_are_stable() {
        // These names are a contract with the deployed Railway env config — a
        // silent rename here stops prod alerts from posting. Pin them.
        assert_eq!(SlackChannel::Cron.env_var(), "SLACK_WEBHOOK_CRON");
    }
}
