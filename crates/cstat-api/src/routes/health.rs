//! `GET /api/health/ingest` — freshness/observability surface over the
//! `ingest_runs` ledger written by the `cstat-ingest nightly` orchestrator.
//!
//! Reports, per pipeline step, the last successful run timestamp and the most
//! recent status, plus an overall `healthy`/`stale` verdict. Drives a status
//! badge on the site and is the poll target for an external uptime monitor —
//! that monitor (not the API) is what catches a *missed* night (last-success
//! older than the threshold) during the season, since the nightly process only
//! self-alerts when it actually runs.
//!
//! Mounted **un-guarded** in `main.rs` (alongside `/api/health`) so a saturated
//! server or a stale pipeline still answers the monitor rather than getting
//! load-shed.

use axum::{extract::State, http::StatusCode, response::Json};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

/// Steps whose staleness makes the served product wrong (predictions/rankings
/// recompute from these). `forecasts` is intentionally excluded — its payload
/// is legitimately empty off-season and before NatStat's nightly tabulation, so
/// gating health on it would false-alarm. `playbyplay`/`lineups` are likewise
/// excluded: they only feed display surfaces (duos/trios, on-off, RAPM) and 3
/// gracefully-degrading trajectory features, and are legitimately empty on any
/// night with no games in the window — gating on them would false-alarm every
/// off-season night. Kept in sync with the steps recorded by
/// `SeasonIngester::nightly`.
///
/// `torvik`/`torvik_games` stay on this list through the November season flip,
/// when barttorvik has not published the new season yet — that case is handled
/// by widening the threshold (see [`SOURCE_NOT_PUBLISHED_GRACE_HOURS`]) rather
/// than by dropping them, so a Torvik outage on a season Bart *is* publishing
/// still turns this endpoint red on schedule.
const SERVED_CRITICAL: &[&str] = &[
    "games",
    "player_perfs",
    "team_perfs",
    "elo",
    "torvik",
    "torvik_games",
    "compute",
];

/// A served-critical step is "stale" once its last success is older than this.
/// One missed nightly (run cadence is ~24h) is still fresh; two in a row is not.
const STALE_AFTER_HOURS: f64 = 36.0;

/// `ingest_runs.status` written by the nightly when a step ran correctly and the
/// **upstream source has no data for that season yet** — see
/// `cstat_ingest::run_ledger::StepStatus::SourceNotPublished`. Compared as a
/// string because the two crates meet at the database, not at a shared type;
/// the ingest side pins this exact spelling in its own test.
const SOURCE_NOT_PUBLISHED: &str = "source_not_published";

/// Extra staleness slack while the source itself is the thing that is behind
/// (#248).
///
/// The problem this solves: `current_natstat_season()` rolls forward on Nov 1,
/// but barttorvik publishes the new season days-to-weeks later. In that gap
/// `torvik` and `torvik_games` — both served-critical — cannot succeed, so at
/// 36h this endpoint returned 503 every night over a condition with no
/// remediation. An endpoint that is red for a fortnight every November is an
/// endpoint whose red stops being read, during the one window when the rest of
/// the tipoff checklist depends on it.
///
/// Why a grace window rather than an exemption. "The source has not published
/// it" is a true statement on night 1 and an outage by night 30 — barttorvik
/// disappearing for a month is a real problem, and a permanent exemption would
/// keep this endpoint green through it. So the excuse is time-boxed, and the
/// box is generous: two weeks past the ordinary threshold comfortably covers
/// the observed publish lag while still failing loudly if the feed never
/// returns.
///
/// Two properties worth keeping if this is ever changed:
///
/// - The grace extends the threshold; it never resets the clock. `last_ok_at`
///   still points at the last real success, so the moment the source starts
///   answering normally again — or fails for any *other* reason — the full
///   accumulated staleness is exposed immediately, without a fresh success.
/// - It keys off the step's most recent status only. One ordinary failure in
///   the middle of the gap drops the step straight back to the 36h rule, which
///   is right: that failure is ours, not the source's.
const SOURCE_NOT_PUBLISHED_GRACE_HOURS: f64 = 24.0 * 14.0;

/// Is this step stale? Pure — unit-tested below.
///
/// `hours_since_ok` is `None` when the step has never succeeded at all, which is
/// stale for a served-critical step no matter what the latest status says: an
/// unpublished source plus no history means the served product genuinely has
/// nothing, and that is worth a 503.
fn is_stale(is_critical: bool, hours_since_ok: Option<f64>, last_status: Option<&str>) -> bool {
    if !is_critical {
        return false;
    }
    let threshold = if last_status == Some(SOURCE_NOT_PUBLISHED) {
        STALE_AFTER_HOURS + SOURCE_NOT_PUBLISHED_GRACE_HOURS
    } else {
        STALE_AFTER_HOURS
    };
    hours_since_ok.map(|h| h > threshold).unwrap_or(true)
}

#[derive(Serialize)]
struct StepHealth {
    step: String,
    /// Most recent `status` recorded for this step (`ok` / `failed` / `skipped`
    /// / `source_not_published`).
    last_status: Option<String>,
    /// Timestamp of the most recent *successful* run of this step.
    last_ok_at: Option<DateTime<Utc>>,
    /// Hours since `last_ok_at` (null if the step has never succeeded).
    hours_since_ok: Option<f64>,
    /// Timestamp of the most recent run of this step regardless of outcome.
    last_run_at: Option<DateTime<Utc>>,
    /// Served-critical and last success older than the staleness threshold
    /// (or never succeeded). Drives the overall verdict.
    stale: bool,
    /// The upstream source has no data for this season yet, so this step is
    /// running under the widened threshold. Surfaced so a reader of a green
    /// response can see *why* a step with an old `last_ok_at` is not stale.
    source_not_published: bool,
}

/// Row shape from the per-step aggregate query.
type StepRow = (
    String,                // step
    Option<DateTime<Utc>>, // last_ok_at
    Option<f64>,           // hours_since_ok
    Option<String>,        // last_status
    Option<DateTime<Utc>>, // last_run_at
);

pub async fn ingest_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows: Vec<StepRow> = sqlx::query_as(
        "SELECT \
           step, \
           max(ended_at) FILTER (WHERE status = 'ok')                                      AS last_ok_at, \
           (EXTRACT(EPOCH FROM (now() - max(ended_at) FILTER (WHERE status = 'ok'))) / 3600.0)::float8 AS hours_since_ok, \
           (array_agg(status ORDER BY ended_at DESC))[1]                                   AS last_status, \
           max(ended_at)                                                                   AS last_run_at \
         FROM ingest_runs \
         GROUP BY step \
         ORDER BY step",
    )
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("ingest_runs query failed: {e}") })),
        )
    })?;

    let steps: Vec<StepHealth> = rows
        .into_iter()
        .map(
            |(step, last_ok_at, hours_since_ok, last_status, last_run_at)| {
                let is_critical = SERVED_CRITICAL.contains(&step.as_str());
                let stale = is_stale(is_critical, hours_since_ok, last_status.as_deref());
                let source_not_published = last_status.as_deref() == Some(SOURCE_NOT_PUBLISHED);
                StepHealth {
                    step,
                    last_status,
                    last_ok_at,
                    hours_since_ok,
                    last_run_at,
                    stale,
                    source_not_published,
                }
            },
        )
        .collect();

    // A served-critical step that has never been recorded at all is also stale
    // — it would simply be absent from the rows above. Detect both shapes.
    let recorded: std::collections::HashSet<&str> = steps.iter().map(|s| s.step.as_str()).collect();
    let missing_critical: Vec<&str> = SERVED_CRITICAL
        .iter()
        .copied()
        .filter(|s| !recorded.contains(s))
        .collect();

    let any_stale = steps.iter().any(|s| s.stale) || !missing_critical.is_empty();
    let last_run_at = steps.iter().filter_map(|s| s.last_run_at).max();

    let status = if any_stale { "stale" } else { "ok" };
    let code = if any_stale {
        // 503 so an external uptime monitor flips red without parsing the body.
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    let body = json!({
        "status": status,
        "healthy": !any_stale,
        "stale_after_hours": STALE_AFTER_HOURS,
        "source_not_published_grace_hours": SOURCE_NOT_PUBLISHED_GRACE_HOURS,
        "last_run_at": last_run_at,
        "missing_critical_steps": missing_critical,
        "steps": steps,
    });

    if any_stale {
        Err((code, Json(body)))
    } else {
        Ok(Json(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLACK: f64 = STALE_AFTER_HOURS + SOURCE_NOT_PUBLISHED_GRACE_HOURS;

    #[test]
    fn non_critical_steps_are_never_stale() {
        assert!(!is_stale(false, Some(10_000.0), Some("failed")));
        assert!(!is_stale(false, None, None));
    }

    #[test]
    fn ordinary_steps_use_the_36h_rule() {
        assert!(!is_stale(true, Some(35.9), Some("ok")));
        assert!(is_stale(true, Some(36.1), Some("ok")));
        // A failure is ours to fix, so it gets no slack.
        assert!(is_stale(true, Some(36.1), Some("failed")));
    }

    /// The November flip: Torvik's last success is days old and the source is
    /// the thing that is behind. Red at 36h is the noise #248 was filed about.
    #[test]
    fn an_unpublished_source_widens_the_threshold() {
        assert!(!is_stale(true, Some(200.0), Some(SOURCE_NOT_PUBLISHED)));
        assert!(!is_stale(
            true,
            Some(SLACK - 0.1),
            Some(SOURCE_NOT_PUBLISHED)
        ));
    }

    /// ...but the excuse is time-boxed. A source that never comes back is an
    /// outage, and this endpoint has to say so eventually.
    #[test]
    fn the_grace_window_expires() {
        assert!(is_stale(
            true,
            Some(SLACK + 0.1),
            Some(SOURCE_NOT_PUBLISHED)
        ));
    }

    /// The grace extends the threshold, it does not reset the clock: one
    /// ordinary failure mid-gap drops the step straight back to 36h, exposing
    /// the staleness that accumulated while the source was behind.
    #[test]
    fn a_real_failure_mid_gap_re_exposes_the_backlog() {
        let hours = 200.0;
        assert!(!is_stale(true, Some(hours), Some(SOURCE_NOT_PUBLISHED)));
        assert!(
            is_stale(true, Some(hours), Some("failed")),
            "the same age must be stale the moment the latest status is not the \
             source-behind one — no fresh success required"
        );
    }

    /// A served-critical step that has never succeeded is stale regardless: an
    /// unpublished source plus no history means the product genuinely has
    /// nothing to serve.
    #[test]
    fn never_succeeded_is_stale_even_when_unpublished() {
        assert!(is_stale(true, None, Some(SOURCE_NOT_PUBLISHED)));
        assert!(is_stale(true, None, None));
    }
}
