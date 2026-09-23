//! ClickHouse HTTP client: queries, chunked inserts, retries.
//!
//! Wire format is `JSONEachRow` over HTTP: chunked at 10k rows, 3 retries,
//! `query_id` deduplication token, 30s query / 120s insert timeouts.
//! (RowBinary + LZ4 is the upgrade path if profiling shows serialisation
//! pressure — the chunking, retry and dedup discipline is identical.)

use std::time::Duration;

use tracing::{debug, warn};

use crate::ArchiveError;

/// ClickHouse HTTP client.
#[derive(Debug, Clone)]
pub struct ClickHouseClient {
    url: String,
    database: String,
    http: reqwest::Client,
    insert_chunk_rows: usize,
    insert_retries: u32,
}

impl ClickHouseClient {
    /// Build from endpoint parts. No I/O happens here; use [`Self::ping`].
    pub fn new(
        url: String,
        database: String,
        insert_chunk_rows: usize,
        insert_retries: u32,
    ) -> Result<Self, ArchiveError> {
        if url.trim().is_empty() {
            return Err(ArchiveError::BadConfig("empty clickhouse url".to_owned()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ArchiveError::BadConfig(format!("http client: {e}")))?;
        Ok(Self {
            url,
            database,
            http,
            insert_chunk_rows: insert_chunk_rows.max(1),
            insert_retries: insert_retries.max(1),
        })
    }

    /// Build from the shared [`sui_indexer_config::ClickHouseConfig`].
    pub fn from_config(
        config: &sui_indexer_config::ClickHouseConfig,
    ) -> Result<Self, ArchiveError> {
        Self::new(
            config.url.clone(),
            config.database.clone(),
            config.insert_chunk_rows,
            config.insert_retries,
        )
    }

    /// Database this client writes to.
    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Chunk size for inserts.
    #[must_use]
    pub fn chunk_rows(&self) -> usize {
        self.insert_chunk_rows
    }

    /// `SELECT 1` liveness probe.
    pub async fn ping(&self) -> Result<(), ArchiveError> {
        self.query("SELECT 1").await.map(|_| ())
    }

    /// Run a query and return the raw response body.
    pub async fn query(&self, sql: &str) -> Result<String, ArchiveError> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut last_error = String::new();
        for attempt in 1..=self.insert_retries {
            match self
                .http
                .post(&self.url)
                .query(&[
                    ("database", self.database.as_str()),
                    ("query_id", id.as_str()),
                ])
                .body(sql.to_owned())
                .timeout(Duration::from_secs(30))
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    if status.is_success() {
                        return Ok(body);
                    }
                    last_error = format!("status {status}: {body}");
                }
                Err(e) => last_error = e.to_string(),
            }
            warn!(attempt, "clickhouse query failed: {last_error}");
        }
        Err(ArchiveError::RequestFailed {
            attempts: self.insert_retries,
            reason: last_error,
        })
    }

    /// Insert JSON rows into `table` in [`Self::chunk_rows`] chunks.
    pub async fn insert_json(
        &self,
        table: &str,
        rows: &[serde_json::Value],
    ) -> Result<u64, ArchiveError> {
        let mut written = 0u64;
        for chunk in rows.chunks(self.insert_chunk_rows) {
            let body = chunk
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            let id = uuid::Uuid::new_v4().to_string();
            let mut last_error = String::new();
            let mut ok = false;
            for attempt in 1..=self.insert_retries {
                match self
                    .http
                    .post(&self.url)
                    .query(&[
                        ("database", self.database.as_str()),
                        (
                            "query",
                            format!("INSERT INTO {table} FORMAT JSONEachRow").as_str(),
                        ),
                        ("query_id", id.as_str()),
                    ])
                    .body(body.clone())
                    .timeout(Duration::from_secs(120))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        ok = true;
                        break;
                    }
                    Ok(resp) => {
                        last_error = format!("status {}", resp.status());
                        // Same query_id retried: the insert is idempotent.
                        debug!(attempt, table, "clickhouse insert retry: {last_error}");
                    }
                    Err(e) => last_error = e.to_string(),
                }
            }
            if !ok {
                return Err(ArchiveError::RequestFailed {
                    attempts: self.insert_retries,
                    reason: last_error,
                });
            }
            written += chunk.len() as u64;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_rejects_empty_url() {
        assert!(ClickHouseClient::new(String::new(), "db".to_owned(), 10_000, 3).is_err());
    }

    #[test]
    fn client_keeps_chunk_and_retry_settings() {
        let client =
            ClickHouseClient::new("http://localhost:8123".to_owned(), "db".to_owned(), 0, 0)
                .expect("client builds");
        assert_eq!(client.chunk_rows(), 1);
        assert_eq!(client.database(), "db");
    }

    #[test]
    fn client_builds_from_shared_config() {
        let config = sui_indexer_config::ClickHouseConfig::default();
        let client = ClickHouseClient::from_config(&config).expect("client builds");
        assert_eq!(client.database(), "indexer");
    }
}
