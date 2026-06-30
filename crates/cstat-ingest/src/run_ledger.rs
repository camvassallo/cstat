//! Per-step ingest run ledger (`ingest_runs` table). The nightly orchestrator
//! opens a [`RunLedger`] for one invocation and records each step's outcome as
//! it finishes, so a crash mid-run still leaves a durable audit trail and the
//! freshness/health route can report the last successful run per step.
//!
//! Ledger writes are intentionally **fail-soft**: a failure to record a step
//! must never abort the ingest it is observing — we log a warning and move on.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

/// Outcome of a single pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Ok,
    Failed,
    Skipped,
}

impl StepStatus {
    /// Stored string form (the `ingest_runs.status` column).
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
        }
    }
}

/// Records each step of one nightly invocation to `ingest_runs`. All of a
/// run's step rows share its [`run_id`](RunLedger::run_id).
pub struct RunLedger<'a> {
    pool: &'a PgPool,
    run_id: Uuid,
    season: i32,
}

impl<'a> RunLedger<'a> {
    /// Open a ledger for a single run, minting the grouping `run_id`.
    pub fn start(pool: &'a PgPool, season: i32) -> Self {
        Self {
            pool,
            run_id: Uuid::new_v4(),
            season,
        }
    }

    /// The grouping id shared by every step row of this run.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Record one finished step. Fail-soft: a ledger write error is logged and
    /// swallowed so it can't abort the pipeline it's observing.
    pub async fn record(
        &self,
        step: &str,
        status: StepStatus,
        rows_touched: Option<i64>,
        started_at: DateTime<Utc>,
        error: Option<&str>,
    ) {
        let ended_at = Utc::now();
        let res = sqlx::query(
            "INSERT INTO ingest_runs \
             (run_id, season, step, status, rows_touched, started_at, ended_at, error) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(self.run_id)
        .bind(self.season)
        .bind(step)
        .bind(status.as_str())
        .bind(rows_touched)
        .bind(started_at)
        .bind(ended_at)
        .bind(error)
        .execute(self.pool)
        .await;

        if let Err(e) = res {
            warn!(step, error = %e, "failed to record ingest_runs step; continuing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_are_stable() {
        // The health route + alerting query on these literals — they must not
        // drift.
        assert_eq!(StepStatus::Ok.as_str(), "ok");
        assert_eq!(StepStatus::Failed.as_str(), "failed");
        assert_eq!(StepStatus::Skipped.as_str(), "skipped");
    }
}
