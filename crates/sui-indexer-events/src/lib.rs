use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sui_indexer_config::EventFilter;
use sui_json_rpc_types::{SuiEvent, SuiTransactionBlockResponse};
use sui_types::base_types::{ObjectID, TransactionDigest};
use uuid::Uuid;

pub mod batch;
pub mod filter;
pub mod processor;
pub mod transformer;

pub use batch::*;
pub use filter::*;
pub use processor::*;
pub use transformer::*;

/// Processed event with additional metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedEvent {
    /// Unique identifier for this processed event
    pub id: Uuid,
    /// Original Sui event
    pub event: SuiEvent,
    /// Transaction digest that emitted this event
    pub transaction_digest: TransactionDigest,
    /// Checkpoint sequence number
    pub checkpoint_sequence: u64,
    /// Event timestamp
    pub timestamp: DateTime<Utc>,
    /// Package ID that emitted the event
    pub package_id: ObjectID,
    /// Module name that emitted the event
    pub module_name: String,
    /// Event type name
    pub event_type: String,
    /// Sender address
    pub sender: String,
    /// Event fields as JSON
    pub fields: serde_json::Value,
    /// Processing metadata
    pub metadata: EventMetadata,
}

/// Event processing metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMetadata {
    /// When the event was processed
    pub processed_at: DateTime<Utc>,
    /// Processing duration in milliseconds
    pub processing_duration_ms: u64,
    /// Event index within the transaction
    pub event_index: usize,
    /// Whether this event matched any filters
    pub matched_filters: Vec<String>,
    /// Additional tags for categorization
    pub tags: Vec<String>,
}

/// Transaction processing result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedTransaction {
    /// Unique identifier for this processed transaction
    pub id: Uuid,
    /// Original Sui transaction
    pub transaction: SuiTransactionBlockResponse,
    /// Checkpoint sequence number
    pub checkpoint_sequence: u64,
    /// Transaction timestamp
    pub timestamp: DateTime<Utc>,
    /// Processed events from this transaction
    pub events: Vec<ProcessedEvent>,
    /// Processing metadata
    pub metadata: TransactionMetadata,
}

/// Transaction processing metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionMetadata {
    /// When the transaction was processed
    pub processed_at: DateTime<Utc>,
    /// Processing duration in milliseconds
    pub processing_duration_ms: u64,
    /// Number of events extracted
    pub event_count: usize,
    /// Gas used by the transaction
    pub gas_used: Option<u64>,
    /// Transaction success status
    pub success: bool,
}

/// Event processing statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingStats {
    /// Total events processed
    pub total_events: u64,
    /// Total transactions processed
    pub total_transactions: u64,
    /// Events processed per second
    pub events_per_second: f64,
    /// Transactions processed per second
    pub transactions_per_second: f64,
    /// Average processing time per event (ms)
    pub avg_event_processing_ms: f64,
    /// Average processing time per transaction (ms)
    pub avg_transaction_processing_ms: f64,
    /// Processing start time
    pub start_time: DateTime<Utc>,
    /// Last update time
    pub last_update: DateTime<Utc>,
    /// Current checkpoint being processed
    pub current_checkpoint: Option<u64>,
    /// Events by type distribution
    pub events_by_type: HashMap<String, u64>,
    /// Errors encountered
    pub error_count: u64,
}

impl ProcessingStats {
    /// Create new processing statistics
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            total_events: 0,
            total_transactions: 0,
            events_per_second: 0.0,
            transactions_per_second: 0.0,
            avg_event_processing_ms: 0.0,
            avg_transaction_processing_ms: 0.0,
            start_time: now,
            last_update: now,
            current_checkpoint: None,
            events_by_type: HashMap::new(),
            error_count: 0,
        }
    }

    /// Update statistics with new processing data
    pub fn update(
        &mut self,
        events_processed: u64,
        transactions_processed: u64,
        event_processing_time_ms: u64,
        transaction_processing_time_ms: u64,
        current_checkpoint: Option<u64>,
        events_by_type: HashMap<String, u64>,
    ) {
        let now = Utc::now();
        let elapsed = (now - self.start_time).num_seconds() as f64;

        self.total_events += events_processed;
        self.total_transactions += transactions_processed;
        self.last_update = now;
        self.current_checkpoint = current_checkpoint;

        // Update rates
        if elapsed > 0.0 {
            self.events_per_second = self.total_events as f64 / elapsed;
            self.transactions_per_second = self.total_transactions as f64 / elapsed;
        }

        // Update average processing times
        if self.total_events > 0 {
            self.avg_event_processing_ms = (self.avg_event_processing_ms
                * (self.total_events - events_processed) as f64
                + event_processing_time_ms as f64)
                / self.total_events as f64;
        }

        if self.total_transactions > 0 {
            self.avg_transaction_processing_ms = (self.avg_transaction_processing_ms
                * (self.total_transactions - transactions_processed) as f64
                + transaction_processing_time_ms as f64)
                / self.total_transactions as f64;
        }

        // Update events by type
        for (event_type, count) in events_by_type {
            *self.events_by_type.entry(event_type).or_insert(0) += count;
        }
    }

    /// Increment error count
    pub fn increment_errors(&mut self, count: u64) {
        self.error_count += count;
    }

    /// Get processing uptime
    pub fn uptime(&self) -> chrono::Duration {
        Utc::now() - self.start_time
    }

    /// Get time since last update
    pub fn time_since_last_update(&self) -> chrono::Duration {
        Utc::now() - self.last_update
    }
}

impl Default for ProcessingStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Event processing result
#[derive(Debug)]
pub enum ProcessingResult {
    /// Event was successfully processed
    Success(Box<ProcessedEvent>),
    /// Event was filtered out
    Filtered { reason: String, event_type: String },
    /// Error occurred during processing
    Error {
        error: eyre::Error,
        event_type: Option<String>,
    },
}

/// Transaction processing result
#[derive(Debug)]
pub enum TransactionProcessingResult {
    /// Transaction was successfully processed
    Success(Box<ProcessedTransaction>),
    /// Transaction was skipped
    Skipped {
        reason: String,
        transaction_digest: TransactionDigest,
    },
    /// Error occurred during processing
    Error {
        error: eyre::Error,
        transaction_digest: Option<TransactionDigest>,
    },
}

/// Event processing configuration
#[derive(Debug, Clone)]
pub struct EventProcessingConfig {
    /// Maximum number of events to process in a batch
    pub batch_size: usize,
    /// Maximum concurrent processors
    pub max_concurrent: usize,
    /// Event filters to apply
    pub filters: Vec<EventFilter>,
    /// Whether to include transaction data
    pub include_transaction_data: bool,
    /// Whether to extract and parse event fields
    pub extract_fields: bool,
    /// Whether to add processing metadata
    pub add_metadata: bool,
}

impl Default for EventProcessingConfig {
    fn default() -> Self {
        Self {
            batch_size: 100,
            max_concurrent: 10,
            filters: vec![],
            include_transaction_data: true,
            extract_fields: true,
            add_metadata: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_stats_creation() {
        let stats = ProcessingStats::new();
        assert_eq!(stats.total_events, 0);
        assert_eq!(stats.total_transactions, 0);
        assert!(stats.events_by_type.is_empty());
    }

    #[test]
    fn test_processing_stats_update() {
        let mut stats = ProcessingStats::new();
        let mut events_by_type = HashMap::new();
        events_by_type.insert("test_event".to_string(), 5);

        stats.update(5, 1, 100, 500, Some(123), events_by_type);

        assert_eq!(stats.total_events, 5);
        assert_eq!(stats.total_transactions, 1);
        assert_eq!(stats.current_checkpoint, Some(123));
        assert_eq!(stats.events_by_type.get("test_event"), Some(&5));
    }

    #[test]
    fn test_processing_stats_error_increment() {
        let mut stats = ProcessingStats::new();
        stats.increment_errors(3);
        assert_eq!(stats.error_count, 3);
    }

    #[test]
    fn test_event_processing_config_default() {
        let config = EventProcessingConfig::default();
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.max_concurrent, 10);
        assert!(config.include_transaction_data);
        assert!(config.extract_fields);
        assert!(config.add_metadata);
    }

    #[test]
    fn test_processed_event_uuid() {
        use sui_types::base_types::ObjectID;

        let event = ProcessedEvent {
            id: Uuid::new_v4(),
            event: serde_json::from_str(r#"{"id":{"eventSeq":"1","txDigest":"test"},"packageId":"0x2","transactionModule":"test","sender":"0x123","type":"test::Event","parsedJson":{},"bcs":"","timestampMs":"1000"}"#).unwrap(),
            transaction_digest: TransactionDigest::default(),
            checkpoint_sequence: 123,
            timestamp: Utc::now(),
            package_id: ObjectID::ZERO,
            module_name: "test".to_string(),
            event_type: "test::Event".to_string(),
            sender: "0x123".to_string(),
            fields: serde_json::json!({}),
            metadata: EventMetadata {
                processed_at: Utc::now(),
                processing_duration_ms: 100,
                event_index: 0,
                matched_filters: vec![],
                tags: vec![],
            },
        };

        // UUID should be valid
        assert_ne!(event.id, Uuid::nil());
    }
}
