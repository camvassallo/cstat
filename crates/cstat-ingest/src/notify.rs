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
    /// `#errors-api` — cstat-api runtime failures: boot/startup aborts (bad ONNX
    /// export, missing config, migration failure) and 5xx/panic taps. Set on the
    /// API service.
    ErrorsApi,
    /// `#errors-web` — frontend error reports forwarded from the browser via
    /// `POST /api/client-error`. Set on the API service (the sink lives there).
    ErrorsWeb,
}

impl SlackChannel {
    /// Primary env var holding this channel's incoming-webhook URL.
    pub fn env_var(self) -> &'static str {
        match self {
            SlackChannel::Cron => "SLACK_WEBHOOK_CRON",
            SlackChannel::ErrorsApi => "SLACK_WEBHOOK_ERRORS_API",
            SlackChannel::ErrorsWeb => "SLACK_WEBHOOK_ERRORS_WEB",
        }
    }

    /// Resolve the webhook URL for this channel from env, if configured.
    /// `Cron` also honours the legacy `INGEST_ALERT_WEBHOOK` name so an existing
    /// deployment keeps working after the rename.
    fn webhook(self) -> Option<String> {
        non_empty_env(self.env_var()).or_else(|| match self {
            SlackChannel::Cron => non_empty_env("INGEST_ALERT_WEBHOOK"),
            _ => None,
        })
    }
}

/// Outcome of a Slack post, for callers that must know whether it actually
/// landed rather than fire-and-forget (the alert self-test endpoint). Ordinary
/// alert call sites use [`post_slack`] and ignore this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackPostOutcome {
    /// Posted and Slack returned 2xx.
    Sent,
    /// This channel's webhook env var is unset — nothing was sent.
    NotConfigured,
    /// The webhook is set but the POST failed (non-2xx or a transport error).
    Failed,
}

/// Post a message to a Slack channel (used for success heartbeats as well as
/// failure alerts). No-op when that channel's webhook env var is unset. Never
/// panics, never propagates an error — a failed post is logged and swallowed.
pub async fn post_slack(channel: SlackChannel, text: &str) {
    let _ = post_slack_checked(channel, text).await;
}

/// Like [`post_slack`] but returns whether the message actually landed. Callers
/// that report on the alert pipeline's health (the self-test) need this — a
/// fire-and-forget `post_slack` would let them claim success on a silently
/// failed POST. Still never panics or propagates an error.
pub async fn post_slack_checked(channel: SlackChannel, text: &str) -> SlackPostOutcome {
    let Some(webhook) = channel.webhook() else {
        // No webhook configured (e.g. local runs) — surface the would-be message
        // in the logs so it isn't lost, then report it.
        info!(message = %text, env = channel.env_var(), "Slack webhook unset; skipping post");
        return SlackPostOutcome::NotConfigured;
    };

    let client = match reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build Slack HTTP client; skipping post");
            return SlackPostOutcome::Failed;
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
            SlackPostOutcome::Sent
        }
        Ok(resp) => {
            warn!(status = %resp.status(), "Slack post returned non-success; continuing");
            SlackPostOutcome::Failed
        }
        Err(e) => {
            warn!(error = %e, "failed to post to Slack; continuing");
            SlackPostOutcome::Failed
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

/// Ping an external dead-man's-switch monitor (healthchecks.io / Cronitor /
/// Better Uptime "heartbeat" URL). Unlike the Slack pings — which only fire
/// *when a run runs* — this lets an external service alert on the run that
/// **never happened** (cron service dead, schedule silently stopped): the
/// monitor expects a ping each morning and pages when one is missing.
///
/// Call convention (see `SeasonIngester::nightly`): `success=true` on any run
/// that **completes** its served-critical chain — including a *degraded* run,
/// since best-effort feed failures are surfaced in `#cron-job-alerts` and must
/// not page the dead-man's-switch. `success=false` appends `/fail` (the
/// healthchecks.io convention) and is reserved for a **hard abort**, so the
/// monitor pages immediately instead of waiting out its grace period. Net: the
/// monitor pages on exactly a missing ping (never ran) or a `/fail` (aborted).
///
/// No-op when `HEARTBEAT_URL` is unset. Fail-soft — a monitor we can't reach must
/// never affect the ingest it observes.
pub async fn ping_heartbeat(success: bool) {
    let Some(base) = non_empty_env("HEARTBEAT_URL") else {
        info!("HEARTBEAT_URL unset; skipping dead-man's-switch ping");
        return;
    };
    let url = if success {
        base
    } else {
        format!("{}/fail", base.trim_end_matches('/'))
    };

    let client = match reqwest::Client::builder().timeout(NOTIFY_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to build heartbeat HTTP client; skipping ping");
            return;
        }
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => info!(success, "pinged heartbeat monitor"),
        Ok(resp) => {
            warn!(status = %resp.status(), "heartbeat ping returned non-success; continuing")
        }
        Err(e) => warn!(error = %e, "failed to ping heartbeat monitor; continuing"),
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
        assert_eq!(
            SlackChannel::ErrorsApi.env_var(),
            "SLACK_WEBHOOK_ERRORS_API"
        );
        assert_eq!(
            SlackChannel::ErrorsWeb.env_var(),
            "SLACK_WEBHOOK_ERRORS_WEB"
        );
    }
}
