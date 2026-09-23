//! Batch write discipline: chunk, retry, dedup.
//!
//! The generic [`BatchSink`] owns chunking and retry so both the PG staging
//! path and the ClickHouse path share one discipline. Backends implement the
//! single-chunk [`InsertBatch`] trait.

use async_trait::async_trait;
use tracing::warn;

/// One chunk insert. Implementations must be idempotent for the same `token`.
#[async_trait]
pub trait InsertBatch: Send + Sync {
    /// Insert one chunk; `token` is the deduplication token for retries.
    async fn insert_chunk(
        &self,
        token: &str,
        rows: &[serde_json::Value],
    ) -> Result<u64, eyre::Report>;
}

/// Chunked, retried batch sink.
pub struct BatchSink<B> {
    backend: B,
    chunk_rows: usize,
    retries: u32,
}

impl<B: InsertBatch> BatchSink<B> {
    /// Build with chunk size and retry budget.
    #[must_use]
    pub fn new(backend: B, chunk_rows: usize, retries: u32) -> Self {
        Self {
            backend,
            chunk_rows: chunk_rows.max(1),
            retries: retries.max(1),
        }
    }

    /// Write all rows; returns total rows accepted.
    pub async fn write(&self, rows: &[serde_json::Value]) -> Result<u64, eyre::Report> {
        let mut total = 0u64;
        for chunk in rows.chunks(self.chunk_rows) {
            // Same token across retries of one chunk: idempotent replay.
            let token = format!("chunk-{}-{}", total, chunk.len());
            let mut last_error = None;
            for attempt in 1..=self.retries {
                match self.backend.insert_chunk(&token, chunk).await {
                    Ok(wrote) => {
                        total += wrote;
                        last_error = None;
                        break;
                    }
                    Err(e) => {
                        warn!(attempt, "batch chunk failed: {e:#}");
                        last_error = Some(e);
                    }
                }
            }
            if let Some(e) = last_error {
                return Err(e);
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Flaky {
        fail_first: Mutex<bool>,
        chunks: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl InsertBatch for Flaky {
        async fn insert_chunk(
            &self,
            _token: &str,
            rows: &[serde_json::Value],
        ) -> Result<u64, eyre::Report> {
            self.chunks.lock().expect("mutex").push(rows.len());
            let mut fail = self.fail_first.lock().expect("mutex");
            if *fail {
                *fail = false;
                return Err(eyre::eyre!("boom"));
            }
            Ok(rows.len() as u64)
        }
    }

    #[tokio::test]
    async fn sink_chunks_and_retries() {
        let backend = Flaky {
            fail_first: Mutex::new(true),
            chunks: Mutex::new(Vec::new()),
        };
        let sink = BatchSink::new(backend, 10_000, 3);
        let rows: Vec<serde_json::Value> =
            (0..25_000).map(|i| serde_json::json!({ "i": i })).collect();
        let wrote = sink.write(&rows).await.expect("write");
        assert_eq!(wrote, 25_000);
        // 3 chunks (10k/10k/5k) plus one retry of the first chunk.
        assert_eq!(sink.backend.chunks.lock().expect("mutex").len(), 4);
    }

    struct AlwaysFail;

    #[async_trait]
    impl InsertBatch for AlwaysFail {
        async fn insert_chunk(
            &self,
            _token: &str,
            _rows: &[serde_json::Value],
        ) -> Result<u64, eyre::Report> {
            Err(eyre::eyre!("down"))
        }
    }

    #[tokio::test]
    async fn sink_gives_up_after_retries() {
        let sink = BatchSink::new(AlwaysFail, 10, 2);
        let rows = vec![serde_json::json!({})];
        assert!(sink.write(&rows).await.is_err());
    }
}
