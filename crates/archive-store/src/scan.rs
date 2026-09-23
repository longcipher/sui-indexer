//! ClickHouse scan executor: the same chunked `INSERT … SELECT` inside the
//! archive (path A, zero RPC).
//!
//! The user SQL runs verbatim against the archive database, which mirrors
//! the skeleton table names. The PG executor owns the cursor and the row
//! counts; this executor converges the analytical archive behind it.

use async_trait::async_trait;

use crate::{ClickHouseClient, ranged_backfill_sql};

/// Archive-side executor for one job version.
pub struct ClickHouseExecutor {
    client: ClickHouseClient,
    database: String,
    target: String,
    select_sql: String,
}

impl ClickHouseExecutor {
    /// Build for `(database, target)` with the validated user select.
    #[must_use]
    pub fn new(
        client: ClickHouseClient,
        database: String,
        target: String,
        select_sql: String,
    ) -> Self {
        Self {
            client,
            database,
            target,
            select_sql,
        }
    }

    /// Render the chunk statement with `{lo}` / `{hi}` bound.
    #[must_use]
    pub fn chunk_sql(&self, chunk: job_engine::ScanChunk) -> String {
        ranged_backfill_sql(
            &format!("{}.{}", self.database, self.target),
            &self.select_sql,
        )
        .replace("{lo}", &chunk.lo.to_string())
        .replace("{hi}", &chunk.hi.to_string())
    }

    /// Write narrow output rows (`chain`, `_height`, `_rule_version`,
    /// `_commitment`, `data`) to the versioned archive table.
    pub async fn write_rows(
        &self,
        rows: &[serde_json::Value],
    ) -> Result<u64, job_engine::JobError> {
        self.client
            .insert_json(&format!("{}.{}", self.database, self.target), rows)
            .await
            .map_err(|e| job_engine::JobError::Storage(eyre::eyre!("{e}")))
    }
}

#[async_trait]
impl job_engine::VersionExecutor for ClickHouseExecutor {
    async fn run_chunk(
        &self,
        chunk: job_engine::ScanChunk,
    ) -> Result<job_engine::ChunkOutcome, job_engine::JobError> {
        let sql = self.chunk_sql(chunk);
        self.client
            .query(&sql)
            .await
            .map_err(|e| job_engine::JobError::Storage(eyre::eyre!("{e}")))?;
        // ClickHouse reports no affected-row count on this path; the PG
        // executor owns the cursor and the metrics.
        Ok(job_engine::ChunkOutcome {
            chunk,
            rows_written: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executor() -> ClickHouseExecutor {
        let client = ClickHouseClient::new(
            "http://localhost:8123".to_owned(),
            "analytics".to_owned(),
            100,
            1,
        )
        .expect("client builds");
        ClickHouseExecutor::new(
            client,
            "analytics".to_owned(),
            "job_sandwich__v3".to_owned(),
            "SELECT * FROM events WHERE height >= {lo} AND height < {hi}".to_owned(),
        )
    }

    #[test]
    fn chunk_sql_targets_the_archive_table_with_bounds() {
        let sql = executor().chunk_sql(job_engine::ScanChunk { lo: 100, hi: 200 });
        assert!(sql.starts_with("INSERT INTO analytics.job_sandwich__v3 "));
        assert!(sql.contains("height >= 100"));
        assert!(sql.contains("height < 200"));
        assert!(!sql.contains("{lo}"));
    }
}
