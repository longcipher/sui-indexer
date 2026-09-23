use std::sync::Arc;

use eyre::Result;
use sui_indexer_config::DatabaseConfig;
use sui_indexer_events::{ProcessedEvent, ProcessedTransaction};

pub mod control;
pub mod migrations;
pub mod models;
pub mod postgres;
pub mod skeleton;

pub use control::*;
pub use models::*;
pub use postgres::PostgresStorage;
pub use skeleton::{SkeletonCounts, prune_above, prune_sql, store_decoded_block};

/// Filter bundle for event queries.
#[derive(Debug, Clone, Default)]
pub struct EventQueryFilter<'a> {
    /// Package ID filter.
    pub package: Option<&'a str>,
    /// Module filter.
    pub module: Option<&'a str>,
    /// Event type filter.
    pub event_type: Option<&'a str>,
    /// Sender filter.
    pub sender: Option<&'a str>,
    /// First checkpoint (inclusive).
    pub from_checkpoint: Option<u64>,
    /// Last checkpoint (inclusive).
    pub to_checkpoint: Option<u64>,
    /// Max rows.
    pub limit: u64,
}

/// Filter bundle for transaction queries.
#[derive(Debug, Clone, Default)]
pub struct TransactionQueryFilter<'a> {
    /// Sender filter.
    pub sender: Option<&'a str>,
    /// First checkpoint (inclusive).
    pub from_checkpoint: Option<u64>,
    /// Last checkpoint (inclusive).
    pub to_checkpoint: Option<u64>,
    /// Max rows.
    pub limit: u64,
}

/// Row counts per canonical table for status output.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableCounts {
    /// Stored checkpoints.
    pub checkpoints: u64,
    /// Stored canonical transactions.
    pub transactions: u64,
    /// Stored canonical events.
    pub events: u64,
    /// Stored object changes.
    pub objects: u64,
}

/// Read-only SQL gateway result shared with the core query layer.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GatewayResult {
    /// Column names.
    pub columns: Vec<String>,
    /// Rows as JSON values.
    pub rows: Vec<serde_json::Value>,
    /// Rows returned.
    pub row_count: usize,
    /// Whether output was truncated.
    pub truncated: bool,
}

/// Storage trait for different backend implementations
#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    /// Initialize the storage backend
    async fn initialize(&self) -> Result<()>;

    /// Store a single event
    async fn store_event(&self, event: &ProcessedEvent) -> Result<()> {
        self.store_events(vec![event.clone()]).await
    }

    /// Store a batch of events
    async fn store_events(&self, events: Vec<ProcessedEvent>) -> Result<()>;

    /// Store a single transaction
    async fn store_transaction(&self, transaction: &ProcessedTransaction) -> Result<()> {
        self.store_transactions(vec![transaction.clone()]).await
    }

    /// Store a batch of transactions
    async fn store_transactions(&self, transactions: Vec<ProcessedTransaction>) -> Result<()>;

    /// Get events by checkpoint range
    async fn get_events_by_checkpoint_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<ProcessedEvent>>;

    /// Get the latest processed checkpoint
    async fn get_latest_checkpoint(&self) -> Result<Option<u64>>;

    /// Get the last processed checkpoint (alias for get_latest_checkpoint)
    async fn get_last_processed_checkpoint(&self) -> Result<u64> {
        Ok(self.get_latest_checkpoint().await?.unwrap_or(0))
    }

    /// Update checkpoint progress
    async fn update_checkpoint_progress(&self, checkpoint: u64) -> Result<()>;

    /// Update the last processed checkpoint (alias for update_checkpoint_progress)
    async fn update_last_processed_checkpoint(&self, checkpoint: u64) -> Result<()> {
        self.update_checkpoint_progress(checkpoint).await
    }

    /// Store canonical transaction rows with real senders and gas.
    async fn store_transaction_models(&self, transactions: Vec<TransactionModel>) -> Result<()>;

    /// Store canonical object change rows.
    async fn store_object_models(&self, objects: Vec<ObjectModel>) -> Result<()>;

    /// Store a canonical checkpoint row.
    async fn store_checkpoint_model(&self, checkpoint: CheckpointModel) -> Result<()>;

    /// Store canonical decoded event rows with BCS bytes as BYTEA.
    async fn store_canonical_events(&self, events: Vec<CanonicalEventModel>) -> Result<()>;

    /// Store balance insight flow rows derived from coin object changes.
    async fn store_coin_flows(&self, flows: Vec<CoinFlowModel>) -> Result<()>;

    /// Claim due repair queue entries with SKIP LOCKED.
    async fn claim_repair_entries(&self, limit: usize) -> Result<Vec<RepairQueueEntry>>;

    /// Enqueue a checkpoint for background repair.
    async fn enqueue_repair(&self, checkpoint: u64, error: &str) -> Result<()>;

    /// Mark a repair attempt complete (delete) or reschedule with backoff.
    async fn complete_repair(
        &self,
        checkpoint: u64,
        success: bool,
        error: Option<&str>,
        max_attempts: i32,
        backoff_secs: u64,
    ) -> Result<()>;

    /// Read the converged indexer progress row.
    async fn get_progress(&self, pipeline: &str) -> Result<Option<IndexerProgressModel>>;

    /// Advance the continuous checkpoint (contiguous-commit only).
    async fn advance_continuous(
        &self,
        pipeline: &str,
        continuous: u64,
        floor: u64,
        digest: Option<&str>,
    ) -> Result<()>;

    /// Record the archive interval and hot boundary.
    async fn record_archive_window(
        &self,
        pipeline: &str,
        archive_lo: Option<u64>,
        archive_hi: Option<u64>,
        hot_boundary: Option<u64>,
    ) -> Result<()>;

    /// Detect missing checkpoint sequences in [floor, tip].
    async fn detect_gaps(&self, floor: u64, tip: u64) -> Result<Vec<(u64, u64)>>;

    /// Fetch stored checkpoint digests in a range for verification.
    async fn checkpoint_digests(&self, start: u64, end: u64) -> Result<Vec<(u64, String)>>;

    /// Row counts per canonical table (no full scans beyond COUNT).
    async fn table_counts(&self) -> Result<TableCounts>;

    /// Read a pipeline watermark.
    async fn get_watermark(&self, pipeline: &str) -> Result<Option<WatermarkModel>>;

    /// Advance a pipeline watermark (never regresses).
    async fn set_watermark(&self, watermark: WatermarkModel) -> Result<bool>;

    /// Prune canonical tables below the retention window. Returns pruned rows.
    async fn prune_checkpoints(&self, latest: u64, retention: u64) -> Result<u64>;

    /// Rewind a pipeline watermark for replay.
    async fn rewind_watermark(&self, pipeline: &str, checkpoint: u64) -> Result<()>;

    /// Rewind the converged progress row for replay. Unlike
    /// [`Storage::advance_continuous`] (which never regresses), this sets the
    /// continuous checkpoint unconditionally and lowers the floor to match.
    async fn rewind_continuous(&self, pipeline: &str, checkpoint: u64) -> Result<()>;

    /// Query events with optional filters for the HTTP API.
    async fn query_events(&self, filter: EventQueryFilter<'_>) -> Result<Vec<ProcessedEvent>>;

    /// Query transactions with optional filters for the HTTP API.
    async fn query_transactions(
        &self,
        filter: TransactionQueryFilter<'_>,
    ) -> Result<Vec<ProcessedTransaction>>;

    /// Execute a validated read-only SQL statement with row/byte caps.
    async fn sql_query(&self, sql: &str, limit: u64, max_bytes: usize) -> Result<GatewayResult>;

    /// Refresh derived balance snapshots and coin metadata from flows.
    async fn repair_derived_insights(&self) -> Result<()>;

    /// Health check for storage backend
    async fn health_check(&self) -> Result<bool>;
}

/// Storage manager for handling different storage backends
#[derive(Clone)]
pub struct StorageManager {
    backend: Arc<dyn Storage>,
    postgres: Arc<PostgresStorage>,
}

impl StorageManager {
    /// Create a new storage manager with PostgreSQL backend
    pub async fn new_postgres(config: DatabaseConfig) -> Result<Self> {
        let backend = PostgresStorage::new(config).await?;
        let postgres = Arc::new(backend);
        Ok(Self {
            backend: postgres.clone(),
            postgres,
        })
    }

    /// Shared PostgreSQL handle: sync and jobs share one pool, including
    /// the job-engine control plane ([`JobControlPlane`]).
    #[must_use]
    pub fn postgres(&self) -> &Arc<PostgresStorage> {
        &self.postgres
    }

    /// Initialize the storage backend
    pub async fn initialize(&self) -> Result<()> {
        self.backend.initialize().await
    }

    /// Store a single event
    pub async fn store_event(&self, event: &ProcessedEvent) -> Result<()> {
        self.backend.store_event(event).await
    }

    /// Store events
    pub async fn store_events(&self, events: Vec<ProcessedEvent>) -> Result<()> {
        self.backend.store_events(events).await
    }

    /// Store a single transaction
    pub async fn store_transaction(&self, transaction: &ProcessedTransaction) -> Result<()> {
        self.backend.store_transaction(transaction).await
    }

    /// Store transactions
    pub async fn store_transactions(&self, transactions: Vec<ProcessedTransaction>) -> Result<()> {
        self.backend.store_transactions(transactions).await
    }

    /// Get events by checkpoint range
    pub async fn get_events_by_checkpoint_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<ProcessedEvent>> {
        self.backend
            .get_events_by_checkpoint_range(start, end)
            .await
    }

    /// Get the latest processed checkpoint
    pub async fn get_latest_checkpoint(&self) -> Result<Option<u64>> {
        self.backend.get_latest_checkpoint().await
    }

    /// Get the last processed checkpoint
    pub async fn get_last_processed_checkpoint(&self) -> Result<u64> {
        self.backend.get_last_processed_checkpoint().await
    }

    /// Update checkpoint progress
    pub async fn update_checkpoint_progress(&self, checkpoint: u64) -> Result<()> {
        self.backend.update_checkpoint_progress(checkpoint).await
    }

    /// Update the last processed checkpoint
    pub async fn update_last_processed_checkpoint(&self, checkpoint: u64) -> Result<()> {
        self.backend
            .update_last_processed_checkpoint(checkpoint)
            .await
    }

    /// Store canonical transaction rows with real senders and gas.
    pub async fn store_transaction_models(
        &self,
        transactions: Vec<TransactionModel>,
    ) -> Result<()> {
        self.backend.store_transaction_models(transactions).await
    }

    /// Store canonical object change rows.
    pub async fn store_object_models(&self, objects: Vec<ObjectModel>) -> Result<()> {
        self.backend.store_object_models(objects).await
    }

    /// Store a canonical checkpoint row.
    pub async fn store_checkpoint_model(&self, checkpoint: CheckpointModel) -> Result<()> {
        self.backend.store_checkpoint_model(checkpoint).await
    }

    /// Store canonical decoded event rows.
    pub async fn store_canonical_events(&self, events: Vec<CanonicalEventModel>) -> Result<()> {
        self.backend.store_canonical_events(events).await
    }

    /// Store balance insight flow rows.
    pub async fn store_coin_flows(&self, flows: Vec<CoinFlowModel>) -> Result<()> {
        self.backend.store_coin_flows(flows).await
    }

    /// Claim due repair queue entries.
    pub async fn claim_repair_entries(&self, limit: usize) -> Result<Vec<RepairQueueEntry>> {
        self.backend.claim_repair_entries(limit).await
    }

    /// Enqueue a checkpoint for background repair.
    pub async fn enqueue_repair(&self, checkpoint: u64, error: &str) -> Result<()> {
        self.backend.enqueue_repair(checkpoint, error).await
    }

    /// Mark a repair attempt complete or rescheduled.
    pub async fn complete_repair(
        &self,
        checkpoint: u64,
        success: bool,
        error: Option<&str>,
        max_attempts: i32,
        backoff_secs: u64,
    ) -> Result<()> {
        self.backend
            .complete_repair(checkpoint, success, error, max_attempts, backoff_secs)
            .await
    }

    /// Read the converged indexer progress row.
    pub async fn get_progress(&self, pipeline: &str) -> Result<Option<IndexerProgressModel>> {
        self.backend.get_progress(pipeline).await
    }

    /// Advance the continuous checkpoint.
    pub async fn advance_continuous(
        &self,
        pipeline: &str,
        continuous: u64,
        floor: u64,
        digest: Option<&str>,
    ) -> Result<()> {
        self.backend
            .advance_continuous(pipeline, continuous, floor, digest)
            .await
    }

    /// Record the archive interval and hot boundary.
    pub async fn record_archive_window(
        &self,
        pipeline: &str,
        archive_lo: Option<u64>,
        archive_hi: Option<u64>,
        hot_boundary: Option<u64>,
    ) -> Result<()> {
        self.backend
            .record_archive_window(pipeline, archive_lo, archive_hi, hot_boundary)
            .await
    }

    /// Detect missing checkpoint sequences in [floor, tip].
    pub async fn detect_gaps(&self, floor: u64, tip: u64) -> Result<Vec<(u64, u64)>> {
        self.backend.detect_gaps(floor, tip).await
    }

    /// Fetch stored checkpoint digests in a range.
    pub async fn checkpoint_digests(&self, start: u64, end: u64) -> Result<Vec<(u64, String)>> {
        self.backend.checkpoint_digests(start, end).await
    }

    /// Row counts per canonical table.
    pub async fn table_counts(&self) -> Result<TableCounts> {
        self.backend.table_counts().await
    }

    /// Read a pipeline watermark.
    pub async fn get_watermark(&self, pipeline: &str) -> Result<Option<WatermarkModel>> {
        self.backend.get_watermark(pipeline).await
    }

    /// Advance a pipeline watermark (never regresses).
    pub async fn set_watermark(&self, watermark: WatermarkModel) -> Result<bool> {
        self.backend.set_watermark(watermark).await
    }

    /// Prune canonical tables below the retention window.
    pub async fn prune_checkpoints(&self, latest: u64, retention: u64) -> Result<u64> {
        self.backend.prune_checkpoints(latest, retention).await
    }

    /// Rewind a pipeline watermark for replay.
    pub async fn rewind_watermark(&self, pipeline: &str, checkpoint: u64) -> Result<()> {
        self.backend.rewind_watermark(pipeline, checkpoint).await
    }

    /// Rewind the converged progress row for replay.
    pub async fn rewind_continuous(&self, pipeline: &str, checkpoint: u64) -> Result<()> {
        self.backend.rewind_continuous(pipeline, checkpoint).await
    }

    /// Query events with optional filters for the HTTP API.
    pub async fn query_events(&self, filter: EventQueryFilter<'_>) -> Result<Vec<ProcessedEvent>> {
        self.backend.query_events(filter).await
    }

    /// Query transactions with optional filters for the HTTP API.
    pub async fn query_transactions(
        &self,
        filter: TransactionQueryFilter<'_>,
    ) -> Result<Vec<ProcessedTransaction>> {
        self.backend.query_transactions(filter).await
    }

    /// Execute a validated read-only SQL statement with caps.
    pub async fn sql_query(
        &self,
        sql: &str,
        limit: u64,
        max_bytes: usize,
    ) -> Result<GatewayResult> {
        self.backend.sql_query(sql, limit, max_bytes).await
    }

    /// Refresh derived balance insights.
    pub async fn repair_derived_insights(&self) -> Result<()> {
        self.backend.repair_derived_insights().await
    }

    /// Health check
    pub async fn health_check(&self) -> Result<bool> {
        self.backend.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_manager_creation() {
        // This is a placeholder test - actual tests would require database setup
        // Test passes by default - replace with actual test logic when database is available
    }

    #[test]
    fn test_gateway_result_shape() {
        let result = GatewayResult {
            columns: vec!["sequence_number".to_string()],
            rows: vec![serde_json::json!({"sequence_number": 1})],
            row_count: 1,
            truncated: false,
        };
        assert_eq!(result.row_count, 1);
        assert_eq!(result.columns.len(), 1);
    }

    /// In-memory Storage recording progress writes: pins the default-method
    /// delegations (`store_event` → `store_events`,
    /// `update_last_processed_checkpoint` → `update_checkpoint_progress`,
    /// `get_last_processed_checkpoint` → `get_latest_checkpoint`) without a DB.
    struct FakeStorage {
        progress: std::sync::Mutex<u64>,
        events: std::sync::Mutex<u64>,
    }

    #[async_trait::async_trait]
    impl Storage for FakeStorage {
        async fn initialize(&self) -> Result<()> {
            Ok(())
        }

        async fn store_events(&self, events: Vec<ProcessedEvent>) -> Result<()> {
            *self.events.lock().expect("mutex") += events.len() as u64;
            Ok(())
        }

        async fn store_transactions(&self, _transactions: Vec<ProcessedTransaction>) -> Result<()> {
            Ok(())
        }

        async fn get_events_by_checkpoint_range(
            &self,
            _start: u64,
            _end: u64,
        ) -> Result<Vec<ProcessedEvent>> {
            Ok(Vec::new())
        }

        async fn get_latest_checkpoint(&self) -> Result<Option<u64>> {
            Ok(Some(*self.progress.lock().expect("mutex")))
        }

        async fn update_checkpoint_progress(&self, checkpoint: u64) -> Result<()> {
            *self.progress.lock().expect("mutex") = checkpoint;
            Ok(())
        }

        async fn store_transaction_models(
            &self,
            _transactions: Vec<TransactionModel>,
        ) -> Result<()> {
            Ok(())
        }

        async fn store_object_models(&self, _objects: Vec<ObjectModel>) -> Result<()> {
            Ok(())
        }

        async fn store_checkpoint_model(&self, _checkpoint: CheckpointModel) -> Result<()> {
            Ok(())
        }

        async fn store_canonical_events(&self, _events: Vec<CanonicalEventModel>) -> Result<()> {
            Ok(())
        }

        async fn store_coin_flows(&self, _flows: Vec<CoinFlowModel>) -> Result<()> {
            Ok(())
        }

        async fn claim_repair_entries(&self, _limit: usize) -> Result<Vec<RepairQueueEntry>> {
            Ok(Vec::new())
        }

        async fn enqueue_repair(&self, _checkpoint: u64, _error: &str) -> Result<()> {
            Ok(())
        }

        async fn complete_repair(
            &self,
            _checkpoint: u64,
            _success: bool,
            _error: Option<&str>,
            _max_attempts: i32,
            _backoff_secs: u64,
        ) -> Result<()> {
            Ok(())
        }

        async fn get_progress(&self, _pipeline: &str) -> Result<Option<IndexerProgressModel>> {
            Ok(None)
        }

        async fn advance_continuous(
            &self,
            _pipeline: &str,
            _continuous: u64,
            _floor: u64,
            _digest: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }

        async fn record_archive_window(
            &self,
            _pipeline: &str,
            _archive_lo: Option<u64>,
            _archive_hi: Option<u64>,
            _hot_boundary: Option<u64>,
        ) -> Result<()> {
            Ok(())
        }

        async fn detect_gaps(&self, _floor: u64, _tip: u64) -> Result<Vec<(u64, u64)>> {
            Ok(Vec::new())
        }

        async fn checkpoint_digests(&self, _start: u64, _end: u64) -> Result<Vec<(u64, String)>> {
            Ok(Vec::new())
        }

        async fn table_counts(&self) -> Result<TableCounts> {
            Ok(TableCounts::default())
        }

        async fn get_watermark(&self, _pipeline: &str) -> Result<Option<WatermarkModel>> {
            Ok(None)
        }

        async fn set_watermark(&self, _watermark: WatermarkModel) -> Result<bool> {
            Ok(true)
        }

        async fn prune_checkpoints(&self, _latest: u64, _retention: u64) -> Result<u64> {
            Ok(0)
        }

        async fn rewind_watermark(&self, _pipeline: &str, _checkpoint: u64) -> Result<()> {
            Ok(())
        }

        async fn rewind_continuous(&self, _pipeline: &str, _checkpoint: u64) -> Result<()> {
            Ok(())
        }

        async fn query_events(&self, _filter: EventQueryFilter<'_>) -> Result<Vec<ProcessedEvent>> {
            Ok(Vec::new())
        }

        async fn query_transactions(
            &self,
            _filter: TransactionQueryFilter<'_>,
        ) -> Result<Vec<ProcessedTransaction>> {
            Ok(Vec::new())
        }

        async fn sql_query(
            &self,
            _sql: &str,
            _limit: u64,
            _max_bytes: usize,
        ) -> Result<GatewayResult> {
            Ok(GatewayResult::default())
        }

        async fn repair_derived_insights(&self) -> Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn default_methods_delegate() {
        let storage = FakeStorage {
            progress: std::sync::Mutex::new(0),
            events: std::sync::Mutex::new(0),
        };
        // `store_event` delegates to `store_events`.
        storage
            .store_event(&sample_processed_event())
            .await
            .expect("store");
        assert_eq!(*storage.events.lock().expect("mutex"), 1);
        // `update_last_processed_checkpoint` delegates to
        // `update_checkpoint_progress`; the getter reads it back.
        storage
            .update_last_processed_checkpoint(7)
            .await
            .expect("update");
        assert_eq!(
            storage.get_last_processed_checkpoint().await.expect("get"),
            7
        );
    }

    fn sample_processed_event() -> ProcessedEvent {
        ProcessedEvent {
            id: uuid::Uuid::new_v4(),
            event: sample_sui_event(),
            transaction_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
            checkpoint_sequence: 1,
            timestamp: chrono::Utc::now(),
            package_id: "0x0000000000000000000000000000000000000000000000000000000000000002"
                .parse()
                .expect("package"),
            module_name: "coin".to_owned(),
            event_type: "Transfer".to_owned(),
            sender: "0x1".to_owned(),
            fields: serde_json::json!({}),
            metadata: sui_indexer_events::EventMetadata {
                processed_at: chrono::Utc::now(),
                processing_duration_ms: 1,
                event_index: 0,
                matched_filters: vec![],
                tags: vec![],
            },
        }
    }

    fn sample_sui_event() -> sui_json_rpc_types::SuiEvent {
        sui_json_rpc_types::SuiEvent {
            id: sui_types::event::EventID {
                tx_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
                event_seq: 0,
            },
            package_id: "0x0000000000000000000000000000000000000000000000000000000000000002"
                .parse()
                .expect("package"),
            transaction_module: "coin".parse().expect("module"),
            sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .expect("sender"),
            type_:
                "0x0000000000000000000000000000000000000000000000000000000000000002::coin::Transfer"
                    .parse()
                    .expect("type"),
            parsed_json: serde_json::json!({}),
            bcs: sui_json_rpc_types::BcsEvent::new(vec![]),
            timestamp_ms: Some(0),
        }
    }
}
