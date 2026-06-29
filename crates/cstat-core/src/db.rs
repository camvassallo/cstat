use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};

/// Shared database connection pool.
#[derive(Debug, Clone)]
pub struct Database {
    pub pool: PgPool,
}

impl Database {
    /// Pool size from `DATABASE_MAX_CONNECTIONS` (default 25). The API's
    /// heavier endpoints fan out multiple queries per request — `team_detail`
    /// projects every schedule game concurrently, each building a
    /// point-in-time CamPom aggregate — so the old hardcoded 10 throttled
    /// that concurrency before the (shared) pool ever became the bottleneck.
    /// 25 stays well under Postgres's default server-side `max_connections`
    /// of 100, leaving headroom for the ingest binary and ad-hoc `psql`;
    /// override down via the env var if a managed plan caps connections lower.
    fn max_connections() -> u32 {
        std::env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(25)
    }

    /// Connect to PostgreSQL with the given URL. Plain pool used by the
    /// ingest/compute CLI — no statement timeout, because its large aggregate
    /// writes legitimately run long. For the public API use `connect_api`.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(Self::max_connections())
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    /// Connect with public-serving guardrails on top of `connect`:
    ///
    /// - a bounded `acquire_timeout` (10s) so a request fails fast instead of
    ///   hanging indefinitely when every pooled connection is busy, and
    /// - a per-connection `statement_timeout` (15s) so a single runaway query
    ///   can't pin a connection open forever and starve the pool.
    ///
    /// Deliberately NOT used by ingest/compute (`connect`), whose batch writes
    /// exceed the serving cap by design. 15s is generous for any read the API
    /// serves — the heaviest is `team_detail`'s full-season point-in-time
    /// aggregate, well under a second in practice.
    pub async fn connect_api(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(Self::max_connections())
            .acquire_timeout(Duration::from_secs(10))
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("SET statement_timeout = '15s'").await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    /// Run all pending migrations.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("../../migrations").run(&self.pool).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_requires_valid_url() {
        let result = Database::connect("postgres://invalid:5432/nonexistent").await;
        assert!(result.is_err());
    }
}
