use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eyre::Result;
use futures::stream::{self, StreamExt};
use sui_indexer_config::{IndexerConfig, IngestionMode};
use sui_indexer_events::{
    BatchProcessor, DefaultEventProcessor, EventFilterProcessor, EventProcessor, EventTransformer,
};
use sui_indexer_storage::StorageManager;
use sui_types::effects::TransactionEffectsAPI;
use sui_types::execution_status::ExecutionStatus;
use sui_types::transaction::TransactionDataAPI;
use tracing::{debug, error, info, warn};

// Local Sui client module
pub mod api;
pub mod sinks;
pub mod sui;
pub use sui::SuiClient;

/// Runtime counters for indexer observability.
#[derive(Debug, Default)]
pub struct IndexerMetrics {
    /// Checkpoints fully processed.
    pub checkpoints_processed: AtomicU64,
    /// Events written to storage.
    pub events_processed: AtomicU64,
    /// Transactions written to storage.
    pub transactions_processed: AtomicU64,
    /// Processing errors encountered.
    pub errors: AtomicU64,
    /// Latest checkpoint observed on the network.
    pub latest_checkpoint: AtomicU64,
    /// Latest checkpoint committed to storage.
    pub committed_checkpoint: AtomicU64,
}

impl IndexerMetrics {
    /// Render a snapshot for status output.
    pub fn snapshot(&self) -> IndexerMetricsSnapshot {
        IndexerMetricsSnapshot {
            checkpoints_processed: self.checkpoints_processed.load(Ordering::Relaxed),
            events_processed: self.events_processed.load(Ordering::Relaxed),
            transactions_processed: self.transactions_processed.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            latest_checkpoint: self.latest_checkpoint.load(Ordering::Relaxed),
            committed_checkpoint: self.committed_checkpoint.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time copy of [`IndexerMetrics`].
#[derive(Debug, Clone, Copy)]
pub struct IndexerMetricsSnapshot {
    /// Checkpoints fully processed.
    pub checkpoints_processed: u64,
    /// Events written to storage.
    pub events_processed: u64,
    /// Transactions written to storage.
    pub transactions_processed: u64,
    /// Processing errors encountered.
    pub errors: u64,
    /// Latest checkpoint observed on the network.
    pub latest_checkpoint: u64,
    /// Latest checkpoint committed to storage.
    pub committed_checkpoint: u64,
}

/// Core indexer service
#[derive(Clone)]
pub struct IndexerCore {
    config: IndexerConfig,
    sui_client: SuiClient,
    storage: StorageManager,
    event_processor: Arc<dyn EventProcessor>,
    metrics: Arc<IndexerMetrics>,
}

impl IndexerCore {
    /// Create a new indexer core instance
    pub async fn new(config: IndexerConfig) -> Result<Self> {
        info!("Initializing Sui Indexer Core");

        let sui_client = SuiClient::new_grpc_only(config.network.clone()).await?;
        let storage = StorageManager::new_postgres(config.database.clone()).await?;
        let event_processor = Arc::new(DefaultEventProcessor::new());

        Ok(Self {
            config,
            sui_client,
            storage,
            event_processor,
            metrics: Arc::new(IndexerMetrics::default()),
        })
    }

    /// Create indexer with custom event processor
    pub async fn with_event_processor(
        config: IndexerConfig,
        event_processor: Arc<dyn EventProcessor>,
    ) -> Result<Self> {
        info!("Initializing Sui Indexer Core with custom event processor");

        let sui_client = SuiClient::new_grpc_only(config.network.clone()).await?;
        let storage = StorageManager::new_postgres(config.database.clone()).await?;

        Ok(Self {
            config,
            sui_client,
            storage,
            event_processor,
            metrics: Arc::new(IndexerMetrics::default()),
        })
    }

    /// Access the runtime metrics snapshot.
    pub fn metrics_snapshot(&self) -> IndexerMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Access the storage manager.
    pub fn storage(&self) -> &StorageManager {
        &self.storage
    }

    /// Access the indexer configuration.
    pub fn config(&self) -> &IndexerConfig {
        &self.config
    }

    /// Initialize the indexer (run migrations, etc.)
    pub async fn initialize(&self) -> Result<()> {
        info!("Initializing storage backend");

        // Try to initialize with timeout
        let init_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.storage.initialize(),
        )
        .await;

        match init_result {
            Ok(Ok(())) => {
                info!("Storage backend initialized successfully");
                Ok(())
            }
            Ok(Err(e)) => {
                error!("Failed to initialize storage backend: {}", e);
                Err(e)
            }
            Err(_) => {
                error!("Storage initialization timed out after 30 seconds");
                Err(eyre::eyre!("Storage initialization timeout"))
            }
        }
    }

    /// Start the indexer service
    pub async fn start(&mut self) -> Result<()> {
        info!("Sui Indexer started successfully!");
        info!("Network: {} (using gRPC)", self.config.network.network);
        info!("gRPC URL: {}", self.config.network.grpc_url);
        info!("Database: PostgreSQL (connected and migrated)");
        info!("Event batch size: {}", self.config.events.batch_size);
        info!(
            "Max concurrent batches: {}",
            self.config.events.max_concurrent_batches
        );

        info!(
            "Configured {} event filter(s):",
            self.config.events.filters.len()
        );
        for (i, filter) in self.config.events.filters.iter().enumerate() {
            info!(
                "   {}. Package: {}, Module: {}, Event: {}",
                i + 1,
                filter.package.as_deref().unwrap_or("*"),
                filter.module.as_deref().unwrap_or("*"),
                filter.event_type.as_deref().unwrap_or("*")
            );
        }

        let resume_from = self.resolve_resume_checkpoint().await?;
        info!("Resuming from checkpoint {resume_from}");

        match self.config.events.ingestion_mode {
            IngestionMode::Stream => self.run_stream(resume_from).await,
            IngestionMode::Poll => self.run_poll(resume_from).await,
        }
    }

    /// Resolve the checkpoint to resume from: explicit start, stored
    /// watermark, or genesis.
    pub async fn resolve_resume_checkpoint(&self) -> Result<u64> {
        if let Some(start) = self.config.events.start_checkpoint {
            return Ok(start);
        }
        Ok(self
            .storage
            .get_last_processed_checkpoint()
            .await?
            .saturating_add(1))
    }

    /// Stream mode: backfill missing checkpoints, then follow the tip with a
    /// bounded worker pool and retry with exponential backoff.
    async fn run_stream(&mut self, resume_from: u64) -> Result<()> {
        let mut shutdown_signal = Box::pin(tokio::signal::ctrl_c());
        let mut tip_interval = tokio::time::interval(std::time::Duration::from_secs(2));
        let mut next = resume_from;

        info!("Starting stream ingestion from checkpoint {next}");

        loop {
            tokio::select! {
                _ = &mut shutdown_signal => {
                    info!("Received shutdown signal (Ctrl+C)");
                    info!("Stopping Sui indexer...");
                    break;
                }
                _ = tip_interval.tick() => {
                    let latest = match self.latest_checkpoint_with_retry().await {
                        Ok(latest) => latest,
                        Err(e) => {
                            error!("Failed to get latest checkpoint: {e}");
                            self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    self.metrics.latest_checkpoint.store(latest, Ordering::Relaxed);

                    if next > latest {
                        debug!("Caught up to tip at {latest}, waiting for new checkpoints");
                        continue;
                    }

                    let end = self
                        .config
                        .events
                        .last_checkpoint
                        .map(|last| last.min(latest))
                        .unwrap_or(latest);
                    if next > end {
                        info!("Reached configured last checkpoint {end}");
                        break;
                    }

                    match self.process_checkpoint_range(next, end).await {
                        Ok(processed) => {
                            next = processed.saturating_add(1);
                            if self.config.events.last_checkpoint.is_some_and(|last| next > last) {
                                info!("Backfill complete through checkpoint {processed}");
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Error processing checkpoint range: {e}");
                            self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }

        info!("Indexer shutdown complete. Goodbye!");
        Ok(())
    }

    /// Poll mode: periodically scan the latest window for matching events.
    async fn run_poll(&mut self, resume_from: u64) -> Result<()> {
        let mut shutdown_signal = Box::pin(tokio::signal::ctrl_c());
        let mut event_monitor_interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.events.poll_interval_secs.max(1),
        ));
        let mut cursor = resume_from.to_string();

        info!(
            "Starting poll ingestion every {}s",
            self.config.events.poll_interval_secs.max(1)
        );

        loop {
            tokio::select! {
                _ = &mut shutdown_signal => {
                    info!("Received shutdown signal (Ctrl+C)");
                    info!("Stopping Sui indexer...");
                    break;
                }
                _ = event_monitor_interval.tick() => {
                    if let Err(e) = self.poll_and_process_events(&mut cursor).await {
                        error!("Error during event polling: {e}");
                        self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        info!("Indexer shutdown complete. Goodbye!");
        Ok(())
    }

    /// Fetch the latest checkpoint with exponential backoff from config.
    async fn latest_checkpoint_with_retry(&mut self) -> Result<u64> {
        let retry = &self.config.network.retry;
        let mut interval = std::time::Duration::from_millis(retry.initial_delay.max(1));
        let max_interval = std::time::Duration::from_millis(retry.max_delay.max(1));
        let attempts = retry.max_attempts.max(1);

        let mut last_error = None;
        for _ in 0..attempts {
            match self.sui_client.get_latest_checkpoint().await {
                Ok(latest) => return Ok(latest),
                Err(e) => {
                    last_error = Some(e);
                    tokio::time::sleep(interval).await;
                    interval = std::time::Duration::from_secs_f64(
                        (interval.as_secs_f64() * retry.backoff_multiplier)
                            .min(max_interval.as_secs_f64()),
                    );
                }
            }
        }
        Err(eyre::eyre!(
            "latest checkpoint retry exhausted: {}",
            last_error.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    /// Process a checkpoint range with bounded concurrency, then commit the
    /// watermark. Returns the last successfully committed checkpoint.
    pub async fn process_checkpoint_range(&mut self, start: u64, end: u64) -> Result<u64> {
        let batch_size = self.config.events.batch_size.clamp(1, 200) as u64;
        let concurrency = self.config.events.max_concurrent_batches.clamp(1, 32);
        let filter = Arc::new(EventFilterProcessor::new(
            self.config.events.filters.clone(),
        ));
        let transformer = Arc::new(EventTransformer::default());
        let processor = Arc::clone(&self.event_processor);

        let mut ranges = Vec::new();
        let mut current = start;
        while current <= end {
            let range_end = (current.saturating_add(batch_size).saturating_sub(1)).min(end);
            ranges.push((current, range_end));
            current = range_end.saturating_add(1);
        }

        let client = self.sui_client.clone();
        let storage = self.storage.clone();
        let metrics = Arc::clone(&self.metrics);
        let index_transactions = self.config.events.index_transactions;
        let index_objects = self.config.events.index_objects;

        let results = stream::iter(ranges)
            .map(|(range_start, range_end)| {
                let mut client = client.clone();
                let storage = storage.clone();
                let metrics = Arc::clone(&metrics);
                let filter = Arc::clone(&filter);
                let transformer = Arc::clone(&transformer);
                let processor = Arc::clone(&processor);
                async move {
                    let mut last_committed = None;
                    for sequence in range_start..=range_end {
                        let options = CheckpointProcessOptions {
                            index_transactions,
                            index_objects,
                            sequence,
                        };
                        match process_single_checkpoint(
                            &mut client,
                            &storage,
                            &filter,
                            &transformer,
                            &processor,
                            options,
                        )
                        .await
                        {
                            Ok(outcome) => {
                                metrics
                                    .checkpoints_processed
                                    .fetch_add(1, Ordering::Relaxed);
                                metrics
                                    .events_processed
                                    .fetch_add(outcome.events, Ordering::Relaxed);
                                metrics
                                    .transactions_processed
                                    .fetch_add(outcome.transactions, Ordering::Relaxed);
                                metrics
                                    .committed_checkpoint
                                    .store(sequence, Ordering::Relaxed);
                                last_committed = Some(sequence);
                            }
                            Err(e) => {
                                metrics.errors.fetch_add(1, Ordering::Relaxed);
                                warn!("Skipping checkpoint {sequence}: {e}");
                            }
                        }
                    }
                    Ok::<Option<u64>, eyre::Error>(last_committed)
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<Result<Option<u64>>>>()
            .await;

        let mut committed: Option<u64> = None;
        for result in results {
            if let Some(sequence) = result? {
                committed = Some(committed.map_or(sequence, |current: u64| current.max(sequence)));
            }
        }

        if let Some(sequence) = committed {
            self.storage.update_checkpoint_progress(sequence).await?;
            if let Some(retention) = self.config.events.retention {
                self.storage.prune_checkpoints(sequence, retention).await?;
            }
            Ok(sequence)
        } else {
            Err(eyre::eyre!(
                "no checkpoints committed in range {start}..={end}"
            ))
        }
    }

    /// Poll for new events and process them
    async fn poll_and_process_events(&mut self, cursor: &mut String) -> Result<()> {
        let latest_checkpoint = self.latest_checkpoint_with_retry().await?;
        self.metrics
            .latest_checkpoint
            .store(latest_checkpoint, Ordering::Relaxed);

        let filter_processor = EventFilterProcessor::new(self.config.events.filters.clone());
        let batcher = BatchProcessor::new(self.config.events.batch_size.max(1));

        let mut matched = 0_usize;
        for filter in self.config.events.filters.iter() {
            let result = self
                .sui_client
                .query_events(
                    None,
                    filter.sender.clone(),
                    filter.package.clone(),
                    Some(cursor.clone()),
                    Some(self.config.events.batch_size.clamp(1, 500)),
                    false,
                )
                .await?;

            let mut candidates: Vec<sui_json_rpc_types::SuiEvent> = Vec::new();
            for event in &result.data {
                if let Some(sui_event) =
                    grpc_event_to_sui_event(event, &filter_processor, latest_checkpoint)
                {
                    candidates.push(sui_event);
                }
            }

            if !candidates.is_empty() {
                let processed = batcher.process_events_in_batches(candidates).await?;
                let custom = self.process_with_custom_processor(&processed).await?;
                if !custom.is_empty() {
                    self.storage.store_events(custom).await?;
                    matched = matched.saturating_add(processed.len());
                }
            }

            if let Some(next) = result.next_cursor {
                *cursor = next;
            }
        }

        if matched > 0 {
            self.metrics
                .events_processed
                .fetch_add(matched as u64, Ordering::Relaxed);
            self.storage
                .update_checkpoint_progress(latest_checkpoint)
                .await?;
            self.metrics
                .committed_checkpoint
                .store(latest_checkpoint, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Run custom processor enrichment over transformer output without
    /// dropping the checkpoint-anchored metadata.
    async fn process_with_custom_processor(
        &self,
        events: &[sui_indexer_events::ProcessedEvent],
    ) -> Result<Vec<sui_indexer_events::ProcessedEvent>> {
        let mut enriched = Vec::with_capacity(events.len());
        for event in events {
            enriched.push(
                self.event_processor
                    .process_event(event.event.clone())
                    .await
                    .map(|custom| sui_indexer_events::ProcessedEvent {
                        checkpoint_sequence: event.checkpoint_sequence,
                        timestamp: event.timestamp,
                        metadata: event.metadata.clone(),
                        ..custom
                    })?,
            );
        }
        Ok(enriched)
    }

    /// Health check
    pub async fn health_check(&mut self) -> Result<bool> {
        let sui_healthy = self.sui_client.health_check().await?.healthy;
        let storage_healthy = self.storage.health_check().await?;

        Ok(sui_healthy && storage_healthy)
    }
}

/// Outcome of processing one checkpoint.
#[derive(Debug, Clone, Copy, Default)]
struct CheckpointOutcome {
    /// Events written.
    events: u64,
    /// Transactions written.
    transactions: u64,
}

/// Options controlling single-checkpoint processing.
#[derive(Debug, Clone)]
struct CheckpointProcessOptions {
    index_transactions: bool,
    index_objects: bool,
    sequence: u64,
}

/// Process one full checkpoint: extract events, filter, transform, enrich
/// with the custom processor, and write events/transactions/objects with
/// real senders, gas, and checkpoint context.
async fn process_single_checkpoint(
    client: &mut SuiClient,
    storage: &StorageManager,
    filter: &EventFilterProcessor,
    transformer: &EventTransformer,
    processor: &Arc<dyn EventProcessor>,
    options: CheckpointProcessOptions,
) -> Result<CheckpointOutcome> {
    let CheckpointProcessOptions {
        index_transactions,
        index_objects,
        sequence,
    } = options;
    let checkpoint = client.get_full_checkpoint(sequence).await?;
    let summary = checkpoint.summary.data();
    let timestamp_ms = summary.timestamp_ms;

    let mut outcome = CheckpointOutcome::default();
    let mut transaction_models = Vec::new();
    let mut object_models = Vec::new();

    for executed in &checkpoint.transactions {
        let sender = executed.transaction.sender().to_string();
        let digest = executed.effects.transaction_digest();
        let success = matches!(executed.effects.status(), ExecutionStatus::Success);
        let gas = executed.effects.gas_cost_summary();
        let gas_used = gas
            .computation_cost
            .saturating_add(gas.storage_cost)
            .saturating_sub(gas.storage_rebate);

        if index_transactions {
            transaction_models.push(sui_indexer_storage::TransactionModel::new(
                sui_indexer_storage::TransactionModelConfig {
                    digest: digest.to_string(),
                    checkpoint_sequence: sequence as i64,
                    timestamp_ms: timestamp_ms as i64,
                    sender: sender.clone(),
                    gas_used: Some(gas_used as i64),
                    gas_price: Some(executed.transaction.gas_price() as i64),
                    success,
                    error_message: (!success).then(|| format!("{:?}", executed.effects.status())),
                },
            ));
        }

        if index_objects {
            for (object_ref, _owner, _kind) in executed.effects.all_changed_objects() {
                object_models.push(sui_indexer_storage::ObjectModel::new(
                    sui_indexer_storage::ObjectModelConfig {
                        object_id: object_ref.0.to_string(),
                        version: object_ref.1.value() as i64,
                        digest: object_ref.2.to_string(),
                        checkpoint_sequence: sequence as i64,
                        transaction_digest: digest.to_string(),
                        sender: sender.clone(),
                    },
                ));
            }
        }

        if let Some(transaction_events) = &executed.events {
            for (index, event) in transaction_events.data.iter().enumerate() {
                let sui_event = checkpoint_native_event_to_sui_event(
                    event,
                    *digest,
                    index as u64,
                    sequence,
                    timestamp_ms,
                );
                if !filter.should_process_event(&sui_event) {
                    continue;
                }
                let mut processed = transformer.transform_event(sui_event.clone()).await?;
                processed.checkpoint_sequence = sequence;
                let enriched = processor.process_event(sui_event).await?;
                processed
                    .metadata
                    .tags
                    .extend(enriched.metadata.tags.clone());
                processed.metadata.matched_filters = enriched.metadata.matched_filters.clone();
                storage.store_event(&processed).await?;
                outcome.events = outcome.events.saturating_add(1);
            }
        }
    }

    if index_transactions && !transaction_models.is_empty() {
        let count = transaction_models.len() as u64;
        storage.store_transaction_models(transaction_models).await?;
        outcome.transactions = outcome.transactions.saturating_add(count);
    }
    if index_objects && !object_models.is_empty() {
        storage.store_object_models(object_models).await?;
    }

    storage
        .store_checkpoint_model(sui_indexer_storage::CheckpointModel::new(
            sui_indexer_storage::CheckpointModelConfig {
                sequence_number: sequence as i64,
                digest: checkpoint.summary.digest().to_string(),
                epoch: summary.epoch as i64,
                timestamp_ms: timestamp_ms as i64,
                transaction_count: checkpoint.transactions.len() as i64,
                network_total_transactions: summary.network_total_transactions as i64,
            },
        ))
        .await?;

    debug!(
        "Checkpoint {sequence}: {} events, {} transactions",
        outcome.events, outcome.transactions
    );

    Ok(outcome)
}

/// Convert a native checkpoint event into the JSON-RPC `SuiEvent` shape so
/// the existing filter/transformer pipeline can consume real data.
fn checkpoint_native_event_to_sui_event(
    event: &sui_types::event::Event,
    digest: sui_types::base_types::TransactionDigest,
    event_seq: u64,
    checkpoint: u64,
    timestamp_ms: u64,
) -> sui_json_rpc_types::SuiEvent {
    use sui_json_rpc_types::BcsEvent;

    sui_json_rpc_types::SuiEvent {
        id: sui_types::event::EventID {
            tx_digest: digest,
            event_seq,
        },
        package_id: event.package_id,
        transaction_module: event.transaction_module.clone(),
        sender: event.sender,
        type_: event.type_.clone(),
        parsed_json: serde_json::json!({
            "checkpoint": checkpoint,
            "bcs_bytes": event.contents.len(),
        }),
        bcs: BcsEvent::new(event.contents.clone()),
        timestamp_ms: Some(timestamp_ms),
    }
}

/// Convert a gRPC-native event row into a `SuiEvent` for the poll path.
fn grpc_event_to_sui_event(
    event: &sui::Event,
    filter: &EventFilterProcessor,
    _checkpoint: u64,
) -> Option<sui_json_rpc_types::SuiEvent> {
    use sui_json_rpc_types::BcsEvent;

    let package_id: sui_types::base_types::ObjectID = event.package_id.as_deref()?.parse().ok()?;
    let parsed: sui_types::TypeTag = event.type_.as_deref()?.parse().ok()?;
    let type_ = match parsed {
        sui_types::TypeTag::Struct(tag) => *tag,
        _ => return None,
    };
    let sender: sui_types::base_types::SuiAddress = event.sender.as_deref()?.parse().ok()?;
    let contents = event.contents.clone().unwrap_or(serde_json::Value::Null);
    let digest = contents
        .get("transaction_digest")
        .and_then(serde_json::Value::as_str)
        .and_then(|digest| digest.parse().ok())
        .unwrap_or_default();
    let event_seq = contents
        .get("event_index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    let sui_event = sui_json_rpc_types::SuiEvent {
        id: sui_types::event::EventID {
            tx_digest: digest,
            event_seq,
        },
        package_id,
        transaction_module: event
            .transaction_module
            .as_deref()
            .unwrap_or("")
            .parse()
            .unwrap_or_else(|_| "unknown".parse().expect("static identifier")),
        sender,
        type_,
        parsed_json: contents,
        bcs: BcsEvent::new(event.bcs.clone().unwrap_or_default()),
        timestamp_ms: None,
    };

    if !filter.should_process_event(&sui_event) {
        return None;
    }

    Some(sui_event)
}

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
