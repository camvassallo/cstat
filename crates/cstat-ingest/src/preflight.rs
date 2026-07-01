//! Preflight connectivity health check (`cstat-ingest preflight`).
//!
//! Pings every external feed the nightly depends on — Postgres, NatStat, Torvik,
//! and (if a JWT is configured) 247 — and reports each as reachable, skipped, or
//! down. Two consumers:
//!
//! 1. **The nightly orchestrator** runs it first ([`SeasonIngester::nightly`])
//!    and logs a per-feed summary + records a `preflight` ledger row, so a dead
//!    dependency is diagnosed up front instead of surfacing as an opaque mid-run
//!    failure. It does not change control flow — the per-step isolation already
//!    fail-softs best-effort feeds and hard-fails the serving-critical chain.
//! 2. **Operators / an external monitor** run the standalone command for a quick
//!    "is everything reachable right now" readout (exit code gated on severity).
//!
//! Design note: 247 is intentionally *not* serving-critical (its ~6h JWT lapses
//! routinely — see `docs/247_jwt_recapture.md`), so an expired token is reported
//! but never blocks a default preflight.

use crate::client::NatStatClient;
use crate::tfs::{AuthProbe, TfsClient};
use crate::torvik::TorkvikClient;
use sqlx::PgPool;
use tracing::{info, warn};

/// Health of a single feed.
#[derive(Debug, Clone)]
pub enum FeedHealth {
    /// Reachable and (where applicable) auth-valid, with a short detail.
    Ok(String),
    /// Legitimately not exercised — e.g. 247 with no JWT configured. Not a fault.
    Skipped(String),
    /// Unreachable or auth-rejected, with the reason.
    Down(String),
}

impl FeedHealth {
    /// Single-glyph-free status word for logs/CLI output.
    pub fn word(&self) -> &'static str {
        match self {
            FeedHealth::Ok(_) => "ok",
            FeedHealth::Skipped(_) => "skipped",
            FeedHealth::Down(_) => "down",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            FeedHealth::Ok(s) | FeedHealth::Skipped(s) | FeedHealth::Down(s) => s,
        }
    }

    fn is_down(&self) -> bool {
        matches!(self, FeedHealth::Down(_))
    }
}

/// One feed's probe result, tagged with whether it gates the serving nightly.
#[derive(Debug, Clone)]
pub struct FeedReport {
    pub feed: &'static str,
    pub health: FeedHealth,
    /// Serving-critical: its failure means the nightly produces wrong/stale
    /// served output (DB, NatStat, Torvik). 247 is not critical.
    pub critical: bool,
}

/// Full preflight result across all feeds.
#[derive(Debug, Clone)]
pub struct PreflightReport {
    pub feeds: Vec<FeedReport>,
}

impl PreflightReport {
    /// Any serving-critical feed down — the nightly should treat this as a
    /// genuine problem (default preflight exit-code gate).
    pub fn critical_down(&self) -> bool {
        self.feeds.iter().any(|f| f.critical && f.health.is_down())
    }

    /// Any feed at all down (including non-critical 247) — the `--strict` gate.
    pub fn any_down(&self) -> bool {
        self.feeds.iter().any(|f| f.health.is_down())
    }

    /// Names of the down feeds (for alert/log summaries).
    pub fn down_feeds(&self) -> Vec<&'static str> {
        self.feeds
            .iter()
            .filter(|f| f.health.is_down())
            .map(|f| f.feed)
            .collect()
    }

    /// Emit a per-feed summary to the tracing log (info for ok/skipped, warn for
    /// down). Used by the orchestrator so the nightly log shows feed health.
    pub fn log(&self) {
        for f in &self.feeds {
            if f.health.is_down() {
                warn!(
                    feed = f.feed,
                    critical = f.critical,
                    detail = f.health.detail(),
                    "preflight: feed DOWN"
                );
            } else {
                info!(
                    feed = f.feed,
                    status = f.health.word(),
                    detail = f.health.detail(),
                    "preflight"
                );
            }
        }
    }

    /// Human-readable multi-line summary for the CLI.
    pub fn render(&self) -> String {
        let mut out = String::from("Preflight connectivity check\n");
        for f in &self.feeds {
            let crit = if f.critical { "critical" } else { "optional" };
            out.push_str(&format!(
                "  {:<9} [{:<8}] {:<7} — {}\n",
                f.feed,
                crit,
                f.health.word(),
                f.health.detail()
            ));
        }
        out
    }
}

/// Run the full preflight sweep. `year` scopes the NatStat + 247 probes.
///
/// Each probe is independent and fail-soft: a panic-free `Down` is recorded
/// rather than propagated, so one dead feed never aborts the check itself.
pub async fn run(client: &NatStatClient, pool: &PgPool, year: i32) -> PreflightReport {
    let mut feeds = Vec::new();

    // --- Postgres: SELECT 1 ---
    let db = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
    {
        Ok(_) => FeedHealth::Ok("connection alive".into()),
        Err(e) => FeedHealth::Down(format!("query failed: {e}")),
    };
    feeds.push(FeedReport {
        feed: "database",
        health: db,
        critical: true,
    });

    // --- NatStat: a cheap known-season teamcodes fetch (one page, 1h cache).
    // Ok proves reachable + auth-valid (the client turns API/auth errors into
    // Err); a transport error means the host is unreachable. ---
    let natstat = match client
        .get("teamcodes", Some(&year.to_string()), None, Some(3600))
        .await
    {
        Ok(_) => {
            if client.used_v3_fallback() {
                FeedHealth::Ok("reachable via v3 fallback (v4 was failing)".into())
            } else {
                FeedHealth::Ok(format!("reachable + auth-valid ({year})"))
            }
        }
        Err(e) => FeedHealth::Down(e.to_string()),
    };
    feeds.push(FeedReport {
        feed: "natstat",
        health: natstat,
        critical: true,
    });

    // --- Torvik: lightweight coachdict HEAD-ish probe ---
    let torvik = match TorkvikClient::new().probe().await {
        Ok(()) => FeedHealth::Ok("reachable".into()),
        Err(e) => FeedHealth::Down(e.to_string()),
    };
    feeds.push(FeedReport {
        feed: "torvik",
        health: torvik,
        critical: true,
    });

    // --- 247: only if a JWT is configured. Not serving-critical — an expired
    // token is expected and fail-soft (falls back to the last snapshot). ---
    let tfs = match TfsClient::from_env() {
        Ok(tfs) => match tfs.probe_auth(year).await {
            AuthProbe::Valid { count } => FeedHealth::Ok(format!("JWT valid, {count} in portal")),
            AuthProbe::Expired { status } => {
                FeedHealth::Down(format!("JWT rejected (HTTP {status}) — re-capture it"))
            }
            AuthProbe::Unreachable(msg) => FeedHealth::Down(msg),
        },
        Err(_) => FeedHealth::Skipped("no TFS_247_JWT configured".into()),
    };
    feeds.push(FeedReport {
        feed: "247",
        health: tfs,
        critical: false,
    });

    PreflightReport { feeds }
}
