use std::sync::Arc;

use eyre::Result;
use sui_indexer_config::DatabaseConfig;
use sui_indexer_events::{ProcessedEvent, ProcessedTransaction};

pub mod migrations;
pub mod models;
pub mod postgres;

pub use models::*;
pub use postgres::PostgresStorage;

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

    /// Read a pipeline watermark.
    async fn get_watermark(&self, pipeline: &str) -> Result<Option<WatermarkModel>>;

    /// Advance a pipeline watermark (never regresses).
    async fn set_watermark(&self, watermark: WatermarkModel) -> Result<bool>;

    /// Prune canonical tables below the retention window. Returns pruned rows.
    async fn prune_checkpoints(&self, latest: u64, retention: u64) -> Result<u64>;

    /// Rewind a pipeline watermark for replay.
    async fn rewind_watermark(&self, pipeline: &str, checkpoint: u64) -> Result<()>;

    /// Query events with optional filters for the HTTP API.
    async fn query_events(&self, filter: EventQueryFilter<'_>) -> Result<Vec<ProcessedEvent>>;

    /// Query transactions with optional filters for the HTTP API.
    async fn query_transactions(
        &self,
        filter: TransactionQueryFilter<'_>,
    ) -> Result<Vec<ProcessedTransaction>>;

    /// Health check for storage backend
    async fn health_check(&self) -> Result<bool>;
}

/// Storage manager for handling different storage backends
#[derive(Clone)]
pub struct StorageManager {
    backend: Arc<dyn Storage>,
}

impl StorageManager {
    /// Create a new storage manager with PostgreSQL backend
    pub async fn new_postgres(config: DatabaseConfig) -> Result<Self> {
        let backend = PostgresStorage::new(config).await?;
        Ok(Self {
            backend: Arc::new(backend),
        })
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

    /// Health check
    pub async fn health_check(&self) -> Result<bool> {
        self.backend.health_check().await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_storage_manager_creation() {
        // This is a placeholder test - actual tests would require database setup
        // Test passes by default - replace with actual test logic when database is available
    }
}
