/// Database models for storage layer - Complete implementation
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Complete Event model matching the database schema
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EventModel {
    pub id: uuid::Uuid,
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub event_sequence: i64,
    pub event_type: String,
    pub package_id: String,
    pub module_name: String,
    pub sender: String,
    pub timestamp_ms: i64,
    pub bcs: Option<Vec<u8>>,
    pub fields: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Complete Transaction model matching the database schema
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TransactionModel {
    pub id: uuid::Uuid,
    pub digest: String,
    pub checkpoint_sequence: i64,
    pub timestamp_ms: i64,
    pub sender: String,
    pub gas_used: Option<i64>,
    pub gas_price: Option<i64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Indexer state tracking for checkpoint synchronization
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IndexerStateModel {
    pub id: i32,
    pub last_processed_checkpoint: i64,
    pub last_processed_timestamp: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Unified pipeline watermark matching the official indexer-alt-framework
/// semantics: committer high-water mark, reader lower bound, and pruner
/// progress for a named pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WatermarkModel {
    pub pipeline: String,
    pub epoch_hi_inclusive: i64,
    pub checkpoint_hi_inclusive: i64,
    pub tx_hi: i64,
    pub timestamp_ms_hi_inclusive: i64,
    pub reader_lo: i64,
    pub pruner_hi: i64,
    pub pruner_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Checkpoint metadata row for the canonical checkpoints table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CheckpointModel {
    pub sequence_number: i64,
    pub digest: String,
    pub prev_digest: Option<String>,
    pub epoch: i64,
    pub timestamp_ms: i64,
    pub transaction_count: i64,
    pub network_total_transactions: i64,
    pub validator_signature: String,
    pub end_of_epoch_data: Option<serde_json::Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Object change row for the canonical objects table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ObjectModel {
    pub id: uuid::Uuid,
    pub object_id: String,
    pub version: i64,
    pub digest: String,
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub sender: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Canonical decoded event row (BCS bytes stored as BYTEA).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CanonicalEventModel {
    pub id: uuid::Uuid,
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub event_index: i64,
    pub package_id: String,
    pub module_name: String,
    pub event_type: String,
    pub sender: String,
    pub timestamp_ms: i64,
    pub bcs: Option<Vec<u8>>,
    pub fields: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Converged indexer progress: continuous/floor watermarks plus archive
/// interval and hot boundary.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IndexerProgressModel {
    pub pipeline: String,
    pub continuous_checkpoint: i64,
    pub floor_checkpoint: i64,
    pub archive_lo: Option<i64>,
    pub archive_hi: Option<i64>,
    pub hot_boundary: Option<i64>,
    pub hot_boundary_ts: Option<chrono::DateTime<chrono::Utc>>,
    pub digest: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Durable repair queue entry for a checkpoint that failed ingest.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RepairQueueEntry {
    pub checkpoint_sequence: i64,
    pub attempts: i32,
    pub next_retry_at: chrono::DateTime<chrono::Utc>,
    pub last_error: Option<String>,
    pub parked: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Balance insight flow row derived from coin object changes.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CoinFlowModel {
    pub checkpoint_sequence: i64,
    pub timestamp_ms: i64,
    pub transaction_digest: String,
    pub coin_type: String,
    pub holder: String,
    pub object_id: String,
    pub version: i64,
    pub balance: serde_json::Value,
}

/// Processed events tracking to avoid reprocessing
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProcessedEventModel {
    pub id: uuid::Uuid,
    pub transaction_digest: String,
    pub event_sequence: i64,
    pub checkpoint_sequence: i64,
    pub processed_at: chrono::DateTime<chrono::Utc>,
    pub processing_duration_ms: i64,
    pub status: String,
    pub error_message: Option<String>,
}

/// Processed transactions tracking
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProcessedTransactionModel {
    pub id: uuid::Uuid,
    pub transaction_digest: String,
    pub checkpoint_sequence: i64,
    pub processed_at: chrono::DateTime<chrono::Utc>,
    pub processing_duration_ms: i64,
    pub status: String,
    pub error_message: Option<String>,
}

/// Event statistics for monitoring and analytics
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EventStatsModel {
    pub date: chrono::NaiveDate,
    pub package_id: String,
    pub module_name: String,
    pub event_type: String,
    pub event_count: i64,
    pub unique_senders: i64,
    pub total_gas_used: i64,
}

/// Configuration for creating a new EventModel
#[derive(Debug)]
pub struct EventModelConfig {
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub event_sequence: i64,
    pub event_type: String,
    pub package_id: String,
    pub module_name: String,
    pub sender: String,
    pub timestamp_ms: i64,
    pub bcs: Option<Vec<u8>>,
    pub fields: serde_json::Value,
}

impl EventModel {
    /// Create a new event model from configuration
    pub fn new(config: EventModelConfig) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            checkpoint_sequence: config.checkpoint_sequence,
            transaction_digest: config.transaction_digest,
            event_sequence: config.event_sequence,
            event_type: config.event_type,
            package_id: config.package_id,
            module_name: config.module_name,
            sender: config.sender,
            timestamp_ms: config.timestamp_ms,
            bcs: config.bcs,
            fields: config.fields,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Configuration for creating a new TransactionModel
#[derive(Debug)]
pub struct TransactionModelConfig {
    pub digest: String,
    pub checkpoint_sequence: i64,
    pub timestamp_ms: i64,
    pub sender: String,
    pub gas_used: Option<i64>,
    pub gas_price: Option<i64>,
    pub success: bool,
    pub error_message: Option<String>,
}

impl TransactionModel {
    /// Create a new transaction model from configuration
    pub fn new(config: TransactionModelConfig) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            digest: config.digest,
            checkpoint_sequence: config.checkpoint_sequence,
            timestamp_ms: config.timestamp_ms,
            sender: config.sender,
            gas_used: config.gas_used,
            gas_price: config.gas_price,
            success: config.success,
            error_message: config.error_message,
            created_at: chrono::Utc::now(),
        }
    }
}

impl IndexerStateModel {
    /// Create a new indexer state
    pub fn new(last_processed_checkpoint: i64, status: String) -> Self {
        Self {
            id: 1, // Always use ID 1 for singleton state
            last_processed_checkpoint,
            last_processed_timestamp: chrono::Utc::now(),
            status,
            updated_at: chrono::Utc::now(),
        }
    }
}

impl ProcessedEventModel {
    /// Create a new processed event record
    pub fn new(
        transaction_digest: String,
        event_sequence: i64,
        checkpoint_sequence: i64,
        processing_duration_ms: i64,
        status: String,
        error_message: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            transaction_digest,
            event_sequence,
            checkpoint_sequence,
            processed_at: chrono::Utc::now(),
            processing_duration_ms,
            status,
            error_message,
        }
    }
}

impl ProcessedTransactionModel {
    /// Create a new processed transaction record
    pub fn new(
        transaction_digest: String,
        checkpoint_sequence: i64,
        processing_duration_ms: i64,
        status: String,
        error_message: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            transaction_digest,
            checkpoint_sequence,
            processed_at: chrono::Utc::now(),
            processing_duration_ms,
            status,
            error_message,
        }
    }
}

/// Configuration for creating a new WatermarkModel
#[derive(Debug)]
pub struct WatermarkModelConfig {
    pub pipeline: String,
    pub epoch_hi_inclusive: i64,
    pub checkpoint_hi_inclusive: i64,
    pub tx_hi: i64,
    pub timestamp_ms_hi_inclusive: i64,
    pub reader_lo: i64,
    pub pruner_hi: i64,
}

/// Configuration for creating a new CheckpointModel
#[derive(Debug)]
pub struct CheckpointModelConfig {
    pub sequence_number: i64,
    pub digest: String,
    pub prev_digest: Option<String>,
    pub epoch: i64,
    pub timestamp_ms: i64,
    pub transaction_count: i64,
    pub network_total_transactions: i64,
    pub validator_signature: String,
    pub end_of_epoch_data: Option<serde_json::Value>,
}

/// Configuration for creating a new ObjectModel
#[derive(Debug)]
pub struct ObjectModelConfig {
    pub object_id: String,
    pub version: i64,
    pub digest: String,
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub sender: String,
}

impl WatermarkModel {
    /// Create a new pipeline watermark
    pub fn new(config: WatermarkModelConfig) -> Self {
        Self {
            pipeline: config.pipeline,
            epoch_hi_inclusive: config.epoch_hi_inclusive,
            checkpoint_hi_inclusive: config.checkpoint_hi_inclusive,
            tx_hi: config.tx_hi,
            timestamp_ms_hi_inclusive: config.timestamp_ms_hi_inclusive,
            reader_lo: config.reader_lo,
            pruner_hi: config.pruner_hi,
            pruner_timestamp: None,
            updated_at: chrono::Utc::now(),
        }
    }
}

impl CheckpointModel {
    /// Create a new checkpoint row
    pub fn new(config: CheckpointModelConfig) -> Self {
        Self {
            sequence_number: config.sequence_number,
            digest: config.digest,
            prev_digest: config.prev_digest,
            epoch: config.epoch,
            timestamp_ms: config.timestamp_ms,
            transaction_count: config.transaction_count,
            network_total_transactions: config.network_total_transactions,
            validator_signature: config.validator_signature,
            end_of_epoch_data: config.end_of_epoch_data,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Configuration for creating a canonical decoded event row.
#[derive(Debug)]
pub struct CanonicalEventModelConfig {
    pub checkpoint_sequence: i64,
    pub transaction_digest: String,
    pub event_index: i64,
    pub package_id: String,
    pub module_name: String,
    pub event_type: String,
    pub sender: String,
    pub timestamp_ms: i64,
    pub bcs: Option<Vec<u8>>,
    pub fields: serde_json::Value,
}

impl CanonicalEventModel {
    /// Create a new canonical event row.
    pub fn new(config: CanonicalEventModelConfig) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            checkpoint_sequence: config.checkpoint_sequence,
            transaction_digest: config.transaction_digest,
            event_index: config.event_index,
            package_id: config.package_id,
            module_name: config.module_name,
            event_type: config.event_type,
            sender: config.sender,
            timestamp_ms: config.timestamp_ms,
            bcs: config.bcs,
            fields: config.fields,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Configuration for creating a coin flow row.
#[derive(Debug)]
pub struct CoinFlowModelConfig {
    pub checkpoint_sequence: i64,
    pub timestamp_ms: i64,
    pub transaction_digest: String,
    pub coin_type: String,
    pub holder: String,
    pub object_id: String,
    pub version: i64,
    pub balance: serde_json::Value,
}

impl CoinFlowModel {
    /// Create a new coin flow row.
    pub fn new(config: CoinFlowModelConfig) -> Self {
        Self {
            checkpoint_sequence: config.checkpoint_sequence,
            timestamp_ms: config.timestamp_ms,
            transaction_digest: config.transaction_digest,
            coin_type: config.coin_type,
            holder: config.holder,
            object_id: config.object_id,
            version: config.version,
            balance: config.balance,
        }
    }
}

impl ObjectModel {
    /// Create a new object change row
    pub fn new(config: ObjectModelConfig) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            object_id: config.object_id,
            version: config.version,
            digest: config.digest,
            checkpoint_sequence: config.checkpoint_sequence,
            transaction_digest: config.transaction_digest,
            sender: config.sender,
            created_at: chrono::Utc::now(),
        }
    }
}
