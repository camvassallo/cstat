use serde_json::Value;
use sqlx::PgPool;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

/// PostgreSQL-backed API response cache.
#[derive(Debug, Clone)]
pub struct ApiCache {
    pool: PgPool,
}

impl ApiCache {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Look up a cached response. Returns None if not found or expired.
    pub async fn get(&self, endpoint: &str, params: &str) -> Result<Option<Value>, sqlx::Error> {
        let params_hash = Self::hash_params(params);
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT response_body FROM api_cache
             WHERE endpoint = $1 AND params_hash = $2 AND expires_at > now()",
        )
        .bind(endpoint)
        .bind(&params_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(body,)| body))
    }

    /// Store a response in the cache with a TTL in seconds.
    pub async fn set(
        &self,
        endpoint: &str,
        params: &str,
        response: &Value,
        ttl_seconds: i64,
    ) -> Result<(), sqlx::Error> {
        let params_hash = Self::hash_params(params);
        sqlx::query(
            "INSERT INTO api_cache (id, endpoint, params_hash, response_body, fetched_at, expires_at)
             VALUES ($1, $2, $3, $4, now(), now() + make_interval(secs => $5))
             ON CONFLICT (endpoint, params_hash) DO UPDATE
             SET response_body = EXCLUDED.response_body,
                 fetched_at = EXCLUDED.fetched_at,
                 expires_at = EXCLUDED.expires_at"
        )
        .bind(Uuid::new_v4())
        .bind(endpoint)
        .bind(&params_hash)
        .bind(response)
        .bind(ttl_seconds as f64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Remove all expired entries.
    pub async fn cleanup_expired(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM api_cache WHERE expires_at < now()")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    fn hash_params(params: &str) -> String {
        let mut hasher = DefaultHasher::new();
        params.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}
