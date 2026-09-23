use std::time::Duration;

use eyre::Result;
use futures::stream::{self, StreamExt};
use sui_json_rpc_types::{SuiEvent, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::{
    EventTransformer, ProcessedEvent, ProcessedTransaction, TransactionMetadata,
    filter::EventFilterProcessor,
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

    /// Process events in optimally sized batches with bounded concurrency.
    pub async fn process_events_in_batches(
        &self,
        events: Vec<SuiEvent>,
    ) -> Result<Vec<ProcessedEvent>> {
        let chunks: Vec<Vec<SuiEvent>> = events
            .chunks(self.batch_size)
            .map(<[SuiEvent]>::to_vec)
            .collect();
        let concurrency = self.batch_concurrency().max(1);

        stream::iter(chunks)
            .map(|chunk| self.process_event_batch(chunk))
            .buffer_unordered(concurrency)
            .collect::<Vec<Result<Vec<ProcessedEvent>>>>()
            .await
            .into_iter()
            .collect::<Result<Vec<Vec<ProcessedEvent>>>>()
            .map(|batches| batches.into_iter().flatten().collect())
    }

    /// Process transactions in optimally sized batches with bounded concurrency.
    pub async fn process_transactions_in_batches(
        &self,
        transactions: Vec<SuiTransactionBlockResponse>,
    ) -> Result<Vec<ProcessedTransaction>> {
        let chunks: Vec<Vec<SuiTransactionBlockResponse>> = transactions
            .chunks(self.batch_size)
            .map(<[SuiTransactionBlockResponse]>::to_vec)
            .collect();
        let concurrency = self.batch_concurrency().max(1);

        stream::iter(chunks)
            .map(|chunk| self.process_transaction_batch(chunk))
            .buffer_unordered(concurrency)
            .collect::<Vec<Result<Vec<ProcessedTransaction>>>>()
            .await
            .into_iter()
            .collect::<Result<Vec<Vec<ProcessedTransaction>>>>()
            .map(|batches| batches.into_iter().flatten().collect())
    }

    fn batch_concurrency(&self) -> usize {
        4
    }
}

impl Default for BatchProcessor {
    fn default() -> Self {
        Self::new(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_event;

    fn allow_all() -> BatchProcessor {
        BatchProcessor::new(10)
    }

    #[test]
    fn components_and_getters() {
        let processor = BatchProcessor::with_components(
            EventTransformer::default(),
            EventFilterProcessor::default(),
            7,
            Duration::from_secs(9),
        );
        assert_eq!(processor.batch_size(), 7);
        assert_eq!(processor.batch_timeout(), Duration::from_secs(9));
        assert_eq!(BatchProcessor::default().batch_size(), 100);
    }

    #[tokio::test]
    async fn event_batch_filters_then_transforms() {
        let processor = BatchProcessor::with_components(
            EventTransformer::default(),
            EventFilterProcessor::new(vec![sui_indexer_config::EventFilter {
                package: Some("0x2".to_string()),
                module: None,
                event_type: None,
                sender: None,
            }]),
            10,
            Duration::from_secs(5),
        );
        let events = vec![
            sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({})),
            sample_event(2, "0x3", "sui_system", "Other", serde_json::json!({})),
        ];
        let processed = processor.process_event_batch(events).await.expect("batch");
        assert_eq!(processed.len(), 1);
        assert_eq!(processed[0].event_type, "Transfer");
    }

    #[tokio::test]
    async fn transaction_batch_extracts_status_gas_and_counts() {
        let processor = allow_all();
        let tx = sample_transaction(4, Some(1000), true);
        let processed = processor
            .process_transaction_batch(vec![tx])
            .await
            .expect("batch");
        assert_eq!(processed.len(), 1);
        assert_eq!(processed[0].checkpoint_sequence, 4);
        assert!(processed[0].metadata.success);
        assert_eq!(processed[0].metadata.gas_used, Some(15));
        assert_eq!(processed[0].metadata.event_count, 2);
    }

    #[tokio::test]
    async fn transaction_without_effects_is_unsuccessful() {
        let processor = allow_all();
        let mut tx = sui_json_rpc_types::SuiTransactionBlockResponse::default();
        tx.checkpoint = Some(9);
        let processed = processor
            .process_transaction_batch(vec![tx])
            .await
            .expect("batch");
        assert_eq!(processed.len(), 1);
        assert!(!processed[0].metadata.success);
        assert_eq!(processed[0].metadata.gas_used, None);
        assert_eq!(processed[0].metadata.event_count, 0);
    }

    #[tokio::test]
    async fn chunked_paths_cover_every_item() {
        let processor = BatchProcessor::new(2);
        let events: Vec<SuiEvent> = (0..5)
            .map(|i| sample_event(i, "0x2", "coin", "Transfer", serde_json::json!({})))
            .collect();
        let mut processed = processor
            .process_events_in_batches(events)
            .await
            .expect("batches");
        processed.sort_by_key(|e| e.metadata.event_index);
        assert_eq!(processed.len(), 5);

        let txs = vec![
            sui_json_rpc_types::SuiTransactionBlockResponse::default(),
            sui_json_rpc_types::SuiTransactionBlockResponse::default(),
            sui_json_rpc_types::SuiTransactionBlockResponse::default(),
        ];
        let done = processor
            .process_transactions_in_batches(txs)
            .await
            .expect("batches");
        assert_eq!(done.len(), 3);
    }

    fn sample_transaction(
        checkpoint: u64,
        timestamp_ms: Option<u64>,
        success: bool,
    ) -> sui_json_rpc_types::SuiTransactionBlockResponse {
        use sui_json_rpc_types::{
            OwnedObjectRef, SuiExecutionStatus, SuiObjectRef, SuiTransactionBlockEffects,
            SuiTransactionBlockEffectsV1, SuiTransactionBlockEvents,
        };
        let addr = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let status = if success {
            SuiExecutionStatus::Success
        } else {
            SuiExecutionStatus::Failure {
                error: "abort".to_string(),
            }
        };
        let gas_object = OwnedObjectRef {
            owner: sui_types::object::Owner::AddressOwner(addr.parse().expect("addr")),
            reference: SuiObjectRef {
                object_id: addr.parse().expect("id"),
                version: 1.into(),
                digest: sui_types::digests::ObjectDigest::new([1; 32]),
            },
        };
        let mut response = sui_json_rpc_types::SuiTransactionBlockResponse::default();
        response.checkpoint = Some(checkpoint);
        response.timestamp_ms = timestamp_ms;
        response.effects = Some(SuiTransactionBlockEffects::V1(
            SuiTransactionBlockEffectsV1 {
                status,
                executed_epoch: 1,
                gas_used: sui_types::gas::GasCostSummary {
                    computation_cost: 10,
                    storage_cost: 5,
                    storage_rebate: 0,
                    non_refundable_storage_fee: 0,
                },
                modified_at_versions: vec![],
                shared_objects: vec![],
                transaction_digest: sui_types::base_types::TransactionDigest::new([9; 32]),
                created: vec![],
                mutated: vec![],
                unwrapped: vec![],
                deleted: vec![],
                unwrapped_then_deleted: vec![],
                wrapped: vec![],
                accumulator_events: vec![],
                gas_object,
                events_digest: None,
                dependencies: vec![],
                abort_error: None,
            },
        ));
        response.events = Some(SuiTransactionBlockEvents {
            data: vec![
                sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({})),
                sample_event(2, "0x2", "coin", "Transfer", serde_json::json!({})),
            ],
        });
        response
    }
}
