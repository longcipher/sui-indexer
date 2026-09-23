//! Durable height-scoped KV on PostgreSQL (`rule_kv`).
//!
//! Same layout as [`crate::MemoryKv`]: every write is tagged with a height,
//! so rollback is a range delete. Pool reserves are the canonical use case.

/// Durable KV scoped to one (chain, rule).
#[derive(Debug, Clone)]
pub struct PgKv {
    pool: sqlx::PgPool,
    chain_id: String,
    rule: String,
}

impl PgKv {
    /// Bind to an existing pool.
    #[must_use]
    pub fn new(pool: sqlx::PgPool, chain_id: String, rule: String) -> Self {
        Self {
            pool,
            chain_id,
            rule,
        }
    }

    /// Read the newest value for `key` at or below `height`.
    pub async fn get(&self, key: &str, height: u64) -> Result<Option<Vec<u8>>, crate::RuleError> {
        let value: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM rule_kv
             WHERE chain_id = $1 AND rule = $2 AND key = $3 AND height <= $4
             ORDER BY height DESC LIMIT 1",
        )
        .bind(&self.chain_id)
        .bind(&self.rule)
        .bind(key)
        .bind(height as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| crate::RuleError::Kv(e.to_string()))?;
        Ok(value)
    }

    /// Write `key` at `height`.
    pub async fn put(&self, key: &str, height: u64, value: &[u8]) -> Result<(), crate::RuleError> {
        sqlx::query(
            "INSERT INTO rule_kv (chain_id, rule, key, height, value, updated_at)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT (chain_id, rule, key, height) DO UPDATE SET
               value = EXCLUDED.value, updated_at = NOW()",
        )
        .bind(&self.chain_id)
        .bind(&self.rule)
        .bind(key)
        .bind(height as i64)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| crate::RuleError::Kv(e.to_string()))?;
        Ok(())
    }

    /// Delete every write above `height` (reorg rollback).
    pub async fn rollback_above(&self, height: u64) -> Result<u64, crate::RuleError> {
        let result =
            sqlx::query("DELETE FROM rule_kv WHERE chain_id = $1 AND rule = $2 AND height > $3")
                .bind(&self.chain_id)
                .bind(&self.rule)
                .bind(height as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| crate::RuleError::Kv(e.to_string()))?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live-DB round trip. Runs when `DATABASE_URL` points at a scratch
    /// database (vacuous pass otherwise so ordinary `cargo test` needs no DB).
    #[tokio::test]
    async fn pg_kv_round_trip() {
        let Some(url) = std::env::var("DATABASE_URL").ok() else {
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rule_kv (
               chain_id TEXT NOT NULL, rule TEXT NOT NULL, key TEXT NOT NULL,
               height BIGINT NOT NULL, value BYTEA NOT NULL,
               updated_at TIMESTAMPTZ DEFAULT NOW(),
               PRIMARY KEY (chain_id, rule, key, height))",
        )
        .execute(&pool)
        .await
        .expect("ddl");
        let kv = PgKv::new(
            pool,
            "test".to_owned(),
            // Unique per test process: mutant runs share one database, so
            // rows from an aborted run must never satisfy another run.
            format!("test-rule-{}", std::process::id()),
        );
        kv.put("pool", 10, b"r10").await.expect("put");
        kv.put("pool", 20, b"r20").await.expect("put");
        assert_eq!(
            kv.get("pool", 15).await.expect("get"),
            Some(b"r10".to_vec())
        );
        assert_eq!(
            kv.get("pool", 20).await.expect("get"),
            Some(b"r20".to_vec())
        );
        kv.rollback_above(15).await.expect("rollback");
        assert_eq!(
            kv.get("pool", 20).await.expect("get"),
            Some(b"r10".to_vec())
        );
    }
}
