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
