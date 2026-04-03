use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Shared database connection pool.
#[derive(Debug, Clone)]
pub struct Database {
    pub pool: PgPool,
}

impl Database {
    /// Connect to PostgreSQL with the given URL.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
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
