use std::time::Duration;

use eyre::Result;
use sui_json_rpc_types::{SuiEvent, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::{
    filter::EventFilterProcessor, EventTransformer, ProcessedEvent, ProcessedTransaction,
    TransactionMetadata,
};

/// Batch processor for handling multiple events efficiently
pub struct BatchProcessor {
    transformer: EventTransformer,
    filter_processor: EventFilterProcessor,
    batch_size: usize,
    batch_timeout: Duration,
}

impl BatchProcessor {
    /// Create a new batch processor with configuration
    pub fn new(batch_size: usize) -> Self {
        Self {
            transformer: EventTransformer::default(),
            filter_processor: EventFilterProcessor::default(),
            batch_size,
            batch_timeout: Duration::from_secs(5),
        }
    }

    /// Create a batch processor with custom components
    pub fn with_components(
        transformer: EventTransformer,
        filter_processor: EventFilterProcessor,
        batch_size: usize,
        batch_timeout: Duration,
    ) -> Self {
        Self {
            transformer,
            filter_processor,
            batch_size,
            batch_timeout,
        }
    }

    /// Get the configured batch size
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Get the configured batch timeout
    pub fn batch_timeout(&self) -> Duration {
        self.batch_timeout
    }

    /// Process a batch of events with filtering and transformation
    pub async fn process_event_batch(&self, events: Vec<SuiEvent>) -> Result<Vec<ProcessedEvent>> {
        let start_time = Instant::now();

        debug!(
            event_count = events.len(),
            batch_size = self.batch_size,
            "Processing event batch"
        );

        // Filter events based on configured filters
        let filtered_events: Vec<SuiEvent> = events
            .into_iter()
            .filter(|event| self.filter_processor.should_process_event(event))
            .collect();

        debug!(
            filtered_count = filtered_events.len(),
            "Events filtered for processing"
        );

        // Transform filtered events
        let processed_events = self.transformer.transform_events(filtered_events).await?;

        let processing_time = start_time.elapsed();
        info!(
            processed_count = processed_events.len(),
            processing_time_ms = processing_time.as_millis(),
            "Event batch processing completed"
        );

        Ok(processed_events)
    }

    /// Process a batch of transactions
    pub async fn process_transaction_batch(
        &self,
        transactions: Vec<SuiTransactionBlockResponse>,
    ) -> Result<Vec<ProcessedTransaction>> {
        let start_time = Instant::now();

        debug!(
            transaction_count = transactions.len(),
            batch_size = self.batch_size,
            "Processing transaction batch"
        );

        let mut processed_transactions = Vec::with_capacity(transactions.len());

        for transaction in transactions {
            match self.process_single_transaction(transaction).await {
                Ok(processed) => processed_transactions.push(processed),
                Err(err) => {
                    warn!(error = %err, "Failed to process transaction, skipping");
                    // Continue processing other transactions
                }
            }
        }

        let processing_time = start_time.elapsed();
        info!(
            processed_count = processed_transactions.len(),
            processing_time_ms = processing_time.as_millis(),
            "Transaction batch processing completed"
        );

        Ok(processed_transactions)
    }

    /// Process a single transaction
    async fn process_single_transaction(
        &self,
        transaction: SuiTransactionBlockResponse,
    ) -> Result<ProcessedTransaction> {
        use chrono::Utc;
        use uuid::Uuid;

        // Extract basic transaction information
        let transaction_digest = transaction.digest;
        let checkpoint_sequence = transaction.checkpoint.unwrap_or(0);
        let timestamp = transaction
            .timestamp_ms
            .map(|ts| chrono::DateTime::from_timestamp_millis(ts as i64).unwrap_or(Utc::now()))
            .unwrap_or(Utc::now());

        // Extract transaction status
        let success = transaction
            .effects
            .as_ref()
            .map(|effects| effects.status().is_ok())
            .unwrap_or(false);

        // Extract gas information
        let gas_used = transaction.effects.as_ref().map(|effects| {
            let summary = effects.gas_cost_summary();
            summary.computation_cost + summary.storage_cost
        });

        // Count events in the transaction
        let event_count = transaction
            .events
            .as_ref()
            .map(|events| events.data.len())
            .unwrap_or(0);

        // Create processed transaction with correct structure
        let processed_transaction = ProcessedTransaction {
            id: Uuid::new_v4(),
            transaction: transaction.clone(),
            checkpoint_sequence,
            timestamp,
            events: vec![], // Will be populated with processed events
            metadata: TransactionMetadata {
                processed_at: Utc::now(),
                processing_duration_ms: 0, // Will be updated later
                event_count,
                gas_used,
                success,
            },
        };

        debug!(
            transaction_digest = %transaction_digest,
            success = success,
            gas_used = gas_used,
            event_count = event_count,
            "Transaction processed"
        );

        Ok(processed_transaction)
    }

    /// Process events in optimally sized batches
    pub async fn process_events_in_batches(
        &self,
        events: Vec<SuiEvent>,
    ) -> Result<Vec<ProcessedEvent>> {
        let mut all_processed = Vec::new();

        for chunk in events.chunks(self.batch_size) {
            let batch_result = self.process_event_batch(chunk.to_vec()).await?;
            all_processed.extend(batch_result);
        }

        Ok(all_processed)
    }

    /// Process transactions in optimally sized batches
    pub async fn process_transactions_in_batches(
        &self,
        transactions: Vec<SuiTransactionBlockResponse>,
    ) -> Result<Vec<ProcessedTransaction>> {
        let mut all_processed = Vec::new();

        for chunk in transactions.chunks(self.batch_size) {
            let batch_result = self.process_transaction_batch(chunk.to_vec()).await?;
            all_processed.extend(batch_result);
        }

        Ok(all_processed)
    }
}

impl Default for BatchProcessor {
    fn default() -> Self {
        Self::new(100)
    }
}
