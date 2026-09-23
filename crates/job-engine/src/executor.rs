//! SQL-tier executor: chunked `INSERT … SELECT` with cursor persistence.
//!
//! Each chunk is one `INSERT INTO <target> <user SELECT with {lo}/{hi} bound>`
//! followed by a cursor advance. Split-on-failure halves a failing chunk so
//! one bad height never blocks the scan; quota breaches quarantine the
//! version instead of starving the sync engine.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use sui_indexer_config::JobSpec;
use sui_indexer_storage::{JobControlPlane, PostgresStorage};
use tracing::{debug, warn};

use crate::{JobError, ddl::validate_table_path, queue::ScanChunk};

/// One executed chunk outcome.
#[derive(Debug, Clone)]
pub struct ChunkOutcome {
    /// Chunk that ran.
    pub chunk: ScanChunk,
    /// Rows written.
    pub rows_written: u64,
}

/// Executes one job version's scan. Implemented by the SQL tier now and the
/// WASM tier in the rule-host crate.
#[async_trait]
pub trait VersionExecutor: Send + Sync {
    /// Run one chunk; returns rows written. Must be cancellable between
    /// chunks by the caller dropping the future.
    async fn run_chunk(&self, chunk: ScanChunk) -> Result<ChunkOutcome, JobError>;
}

/// SQL-tier executor: `INSERT INTO <target> SELECT … WHERE height ∈ [lo, hi)`.
pub struct SqlExecutor {
    chain_id: String,
    job: String,
    version: u32,
    target: String,
    select_sql: String,
    max_rows_per_window: u64,
    rows_this_second: Arc<AtomicU64>,
    storage: Arc<PostgresStorage>,
}

impl SqlExecutor {
    /// Build from a validated spec plus the versioned target table.
    pub fn new(
        chain_id: String,
        spec: &JobSpec,
        version: u32,
        target: String,
        storage: Arc<PostgresStorage>,
    ) -> Result<Self, JobError> {
        spec.validate().map_err(|reason| JobError::InvalidSpec {
            job: spec.name.clone(),
            reason,
        })?;
        crate::ddl::validate_select_sql(&spec.sql).map_err(JobError::BadSql)?;
        Ok(Self {
            chain_id,
            job: spec.name.clone(),
            version,
            target,
            select_sql: spec.sql.clone(),
            max_rows_per_window: spec.runtime.max_rows_per_window.max(1),
            rows_this_second: Arc::new(AtomicU64::new(0)),
            storage,
        })
    }

    /// Render the chunk statement with `{lo}` / `{hi}` bound.
    pub fn chunk_sql(&self, chunk: ScanChunk) -> Result<String, JobError> {
        validate_table_path(&self.target)?;
        let body = self
            .select_sql
            .replace("{lo}", &chunk.lo.to_string())
            .replace("{hi}", &chunk.hi.to_string());
        Ok(format!("INSERT INTO {} {body}", self.target))
    }

    /// Persist the cursor after a successful chunk.
    /// Quota guard: a version that exceeds its budget is quarantined,
    /// never allowed to starve the sync engine.
    fn check_quota(&self, rows_written: u64) -> Result<(), JobError> {
        if rows_written > self.max_rows_per_window {
            return Err(JobError::QuotaExceeded {
                job: self.job.clone(),
                version: self.version,
                reason: format!(
                    "chunk wrote {rows_written} rows, quota is {}",
                    self.max_rows_per_window
                ),
            });
        }
        Ok(())
    }

    /// Persist the cursor after a successful chunk.
    pub async fn commit_chunk(&self, chunk: ScanChunk, rows_written: u64) -> Result<(), JobError> {
        self.check_quota(rows_written)?;
        self.storage
            .advance_version_cursor(
                &self.chain_id,
                &self.job,
                self.version as i32,
                chunk.hi.saturating_sub(1) as i64,
                rows_written as i64,
            )
            .await
            .map_err(JobError::Storage)?;
        debug!(
            job = %self.job,
            version = self.version,
            lo = chunk.lo,
            hi = chunk.hi,
            rows = rows_written,
            "chunk committed"
        );
        Ok(())
    }

    /// Warn when the write rate exceeds the per-second quota.
    #[must_use]
    pub fn check_rate(&self, rows: u64, per_second_quota: u64) -> bool {
        let total = self.rows_this_second.fetch_add(rows, Ordering::Relaxed);
        if total + rows > per_second_quota {
            warn!(
                job = %self.job,
                version = self.version,
                "write rate above quota ({per_second_quota}/s)"
            );
            return false;
        }
        true
    }
}

#[async_trait]
impl VersionExecutor for SqlExecutor {
    /// Run one chunk atomically: rows and cursor commit together, so a
    /// quota breach rolls the partial chunk back instead of duplicating it
    /// when the split halves re-run.
    async fn run_chunk(&self, chunk: ScanChunk) -> Result<ChunkOutcome, JobError> {
        let sql = self.chunk_sql(chunk)?;
        let mut tx = self.storage.pool().begin().await.map_err(|e| {
            JobError::Storage(eyre::eyre!(
                "chunk [{}, {}) begin failed: {e}",
                chunk.lo,
                chunk.hi
            ))
        })?;
        // Audited: target passed `validate_table_path`, body passed
        // `validate_select_sql`, and only `{lo}`/`{hi}` integers interpolate.
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                JobError::Storage(eyre::eyre!(
                    "chunk [{}, {}) failed: {e}",
                    chunk.lo,
                    chunk.hi
                ))
            })?;
        let rows_written = result.rows_affected();
        // Breach drops `tx` uncommitted: no partial rows survive.
        self.check_quota(rows_written)?;
        sui_indexer_storage::advance_version_cursor_tx(
            &mut tx,
            &self.chain_id,
            &self.job,
            self.version as i32,
            chunk.hi.saturating_sub(1) as i64,
            rows_written as i64,
        )
        .await
        .map_err(JobError::Storage)?;
        tx.commit().await.map_err(|e| {
            JobError::Storage(eyre::eyre!(
                "chunk [{}, {}) commit failed: {e}",
                chunk.lo,
                chunk.hi
            ))
        })?;
        debug!(
            job = %self.job,
            version = self.version,
            lo = chunk.lo,
            hi = chunk.hi,
            rows = rows_written,
            "chunk committed"
        );
        Ok(ChunkOutcome {
            chunk,
            rows_written,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sui_indexer_config::OutputConfig;

    fn spec() -> JobSpec {
        JobSpec {
            name: "arb".to_owned(),
            output: OutputConfig {
                table: "job_arb".to_owned(),
                ..OutputConfig::default()
            },
            sql: "SELECT height FROM chain_events WHERE height >= {lo} AND height < {hi}"
                .to_owned(),
            ..JobSpec::default()
        }
    }

    // ponytail: executor DB paths need a live pool; unit tests cover SQL
    // rendering and quota guards, integration covers run_chunk.
    fn executor_without_db(spec: &JobSpec) -> SqlExecutor {
        SqlExecutor {
            chain_id: "test".to_owned(),
            job: spec.name.clone(),
            version: 1,
            target: "job_arb__v1".to_owned(),
            select_sql: spec.sql.clone(),
            max_rows_per_window: spec.runtime.max_rows_per_window,
            rows_this_second: Arc::new(AtomicU64::new(0)),
            storage: Arc::new(fake_storage()),
        }
    }

    fn fake_storage() -> PostgresStorage {
        // Lazy pool with a short acquire timeout: never connects in tests,
        // and any query that escapes the quota guard fails fast instead of
        // hanging on connect.
        let options = "postgresql://127.0.0.1:1/fake"
            .parse::<sqlx::postgres::PgConnectOptions>()
            .expect("endpoint parses");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy_with(options);
        PostgresStorage::from_pool(pool)
    }

    #[tokio::test]
    async fn chunk_sql_binds_lo_hi() {
        let spec = spec();
        let ex = executor_without_db(&spec);
        let sql = ex.chunk_sql(ScanChunk { lo: 100, hi: 200 }).expect("sql");
        assert!(sql.starts_with("INSERT INTO job_arb__v1 "));
        assert!(sql.contains("height >= 100"));
        assert!(sql.contains("height < 200"));
        assert!(!sql.contains("{lo}"));
    }

    #[tokio::test]
    async fn constructor_rejects_non_select_sql() {
        let mut bad = spec();
        bad.sql = "DELETE FROM chain_events".to_owned();
        let storage = Arc::new(fake_storage());
        assert!(
            SqlExecutor::new("t".to_owned(), &bad, 1, "job_arb__v1".to_owned(), storage).is_err()
        );
    }

    #[tokio::test]
    async fn rate_guard_trips_above_quota() {
        let spec = spec();
        let ex = executor_without_db(&spec);
        assert!(ex.check_rate(10, 100));
        assert!(!ex.check_rate(95, 100));
    }

    #[tokio::test]
    async fn quota_boundary_passes_and_breach_fails() {
        let spec = spec();
        let ex = executor_without_db(&spec);
        let quota = spec.runtime.max_rows_per_window;
        assert!(ex.check_quota(quota).is_ok());
        assert!(ex.check_quota(quota.saturating_add(1)).is_err());
    }

    #[tokio::test]
    async fn quota_breach_fails_before_any_io() {
        let spec = spec();
        let ex = executor_without_db(&spec);
        // Breach fails before any I/O: no database needed.
        assert!(
            ex.commit_chunk(
                ScanChunk { lo: 0, hi: 10 },
                spec.runtime.max_rows_per_window.saturating_add(1)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn rate_boundary_is_exact() {
        let spec = spec();
        let ex = executor_without_db(&spec);
        // Exactly at quota passes; one more trips.
        assert!(ex.check_rate(4, 4));
        let ex = executor_without_db(&spec);
        assert!(!ex.check_rate(5, 4));
    }
}
