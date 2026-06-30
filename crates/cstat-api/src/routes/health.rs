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
/// gating health on it would false-alarm. Kept in sync with the steps recorded
/// by `SeasonIngester::nightly`.
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

#[derive(Serialize)]
struct StepHealth {
    step: String,
    /// Most recent `status` recorded for this step (`ok` / `failed` / `skipped`).
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
                let stale = is_critical
                    && hours_since_ok
                        .map(|h| h > STALE_AFTER_HOURS)
                        .unwrap_or(true);
                StepHealth {
                    step,
                    last_status,
                    last_ok_at,
                    hours_since_ok,
                    last_run_at,
                    stale,
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
