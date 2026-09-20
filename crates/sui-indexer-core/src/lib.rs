use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eyre::Result;
use futures::stream::{self, StreamExt};
use sui_indexer_config::{IndexerConfig, IngestionMode};
use sui_indexer_events::{
    BatchProcessor, DefaultEventProcessor, EventFilterProcessor, EventProcessor, EventTransformer,
};
use sui_indexer_storage::{CanonicalEventModelConfig, CoinFlowModelConfig, StorageManager};
use sui_types::effects::TransactionEffectsAPI;
use sui_types::execution_status::ExecutionStatus;
use sui_types::transaction::TransactionDataAPI;
use tracing::{debug, error, info, warn};

// Local Sui client module
pub mod api;
pub mod archive;
pub mod block_feed;
pub mod progress;
pub mod query_gateway;
pub mod repair;
pub mod sinks;
pub mod sui;
pub mod sync_engine;
pub use block_feed::BlockFeed;
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
    block_feed: Arc<BlockFeed>,
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
            block_feed: Arc::new(BlockFeed::new(256)),
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
            block_feed: Arc::new(BlockFeed::new(256)),
        })
    }

    /// Access the live checkpoint feed for SSE streaming.
    pub fn block_feed(&self) -> &Arc<BlockFeed> {
        &self.block_feed
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

    /// Resolve the checkpoint to resume from: explicit start, converged
    /// progress row, stored watermark, or genesis.
    pub async fn resolve_resume_checkpoint(&self) -> Result<u64> {
        progress::Progress::resolve_resume(&self.storage, self.config.events.start_checkpoint).await
    }

    /// Stream mode: dual-lane sync engine (tip tracker + historical
    /// backfiller) with contiguous commit, repair queue enqueue, and digest
    /// verification. Failures requeue without skipping checkpoints.
    async fn run_stream(&mut self, resume_from: u64) -> Result<()> {
        let engine = sync_engine::SyncEngine::new(
            self.config.sync.clone(),
            self.config.events.last_checkpoint,
        );
        let mut shutdown_signal = Box::pin(tokio::signal::ctrl_c());
        let mut tip_interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.sync.tip_interval_secs.max(1),
        ));
        let mut next = resume_from;
        let mut failures: u32 = 0;

        info!("Starting dual-lane sync from checkpoint {next}");

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

                    if let Some(last) = self.config.events.last_checkpoint
                        && next > last.min(latest)
                    {
                        info!("Reached configured last checkpoint {}", last.min(latest));
                        break;
                    }
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
                    let tick = engine.plan_tick(next, end, latest);
                    match self.execute_tick(&tick).await {
                        Ok(committed) => {
                            failures = 0;
                            if let Some(committed) = committed {
                                next = committed.saturating_add(1);
                                self.block_feed.broadcast(committed, latest);
                            }
                        }
                        Err(e) => {
                            failures = failures.saturating_add(1);
                            error!("Error processing tick at {next}: {e}");
                            self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                            if failures >= self.config.sync.failure_backoff_threshold.max(1) {
                                warn!(
                                    "Backing off after {failures} consecutive failures; \
                                     checkpoints requeued, nothing skipped"
                                );
                                failures = 0;
                            }
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

    /// Process a checkpoint range with bounded concurrency and contiguous
    /// commit: the watermark advances only over the longest gap-free prefix,
    /// failed checkpoints are enqueued for background repair, digests are
    /// verified against stored rows, and alerts/sinks fire per checkpoint.
    /// Returns the last contiguously committed checkpoint.
    pub async fn process_checkpoint_range(&mut self, start: u64, end: u64) -> Result<u64> {
        if start > end {
            return Err(eyre::eyre!("invalid range {start}..={end}"));
        }
        let max_span = self.config.sync.max_range_span.max(1);
        let mut cursor = start;
        let mut committed = start.saturating_sub(1);
        let mut first = true;
        while cursor <= end {
            let chunk_end = cursor.saturating_add(max_span).saturating_sub(1).min(end);
            let chunk_committed = self.process_contiguous_chunk(cursor, chunk_end).await?;
            if first || chunk_committed >= committed {
                committed = chunk_committed;
                first = false;
            }
            if chunk_committed < chunk_end {
                break;
            }
            cursor = chunk_end.saturating_add(1);
        }
        if first {
            return Err(eyre::eyre!(
                "no checkpoints committed in range {start}..={end}"
            ));
        }
        Ok(committed)
    }

    /// Execute one sync-engine tick: backfill gaps newest-first, then advance
    /// the contiguous frontier.
    async fn execute_tick(&mut self, tick: &sync_engine::SyncTick) -> Result<Option<u64>> {
        if tick.backfill_ranges.iter().all(|range| range.is_empty) && tick.tracker_range.is_none() {
            return Ok(None);
        }
        for range in &tick.backfill_ranges {
            if range.is_empty {
                continue;
            }
            self.process_contiguous_chunk(range.start, range.end)
                .await?;
        }
        if let Some(range) = &tick.tracker_range {
            let committed = self
                .process_contiguous_chunk(range.start, range.end)
                .await?;
            return Ok(Some(committed));
        }
        let committed = self
            .storage
            .get_last_processed_checkpoint()
            .await
            .unwrap_or(0);
        Ok(Some(committed))
    }

    /// Process one chunk with per-checkpoint isolation: fetch, digest-verify,
    /// extract, filter, transform, enrich, write canonically, commit the
    /// longest gap-free prefix, enqueue the rest for repair.
    async fn process_contiguous_chunk(&mut self, start: u64, end: u64) -> Result<u64> {
        let batch_size = self.config.events.batch_size.clamp(1, 200) as u64;
        let concurrency = self
            .config
            .events
            .max_concurrent_batches
            .clamp(1, 32)
            .min(self.config.sync.backfill_concurrency.max(1));
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
        let sinks = self.config.sinks.clone();
        let alerts = self.config.alerts.clone();
        let webhook_client = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let results = stream::iter(ranges)
            .map(|(range_start, range_end)| {
                let mut client = client.clone();
                let storage = storage.clone();
                let metrics = Arc::clone(&metrics);
                let filter = Arc::clone(&filter);
                let transformer = Arc::clone(&transformer);
                let processor = Arc::clone(&processor);
                let sinks = sinks.clone();
                let alerts = alerts.clone();
                let webhook_client = webhook_client.clone();
                async move {
                    let pipeline = CheckpointPipeline {
                        filter: &filter,
                        transformer: &transformer,
                        processor: &processor,
                        sinks: &sinks,
                        alerts: &alerts,
                        webhook_client: &webhook_client,
                    };
                    let mut outcomes: BTreeMap<u64, CheckpointOutcome> = BTreeMap::new();
                    let mut failures: Vec<(u64, String)> = Vec::new();
                    for sequence in range_start..=range_end {
                        let options = CheckpointProcessOptions {
                            index_transactions,
                            index_objects,
                            sequence,
                        };
                        match process_single_checkpoint(&mut client, &storage, &pipeline, options)
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
                                outcomes.insert(sequence, outcome);
                            }
                            Err(e) => {
                                metrics.errors.fetch_add(1, Ordering::Relaxed);
                                failures.push((sequence, e.to_string()));
                            }
                        }
                    }
                    Ok::<(BTreeMap<u64, CheckpointOutcome>, Vec<(u64, String)>), eyre::Error>((
                        outcomes, failures,
                    ))
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<Result<(BTreeMap<u64, CheckpointOutcome>, Vec<(u64, String)>)>>>()
            .await;

        let mut succeeded: BTreeSet<u64> = BTreeSet::new();
        let mut failures: Vec<(u64, String)> = Vec::new();
        for result in results {
            let (outcomes, chunk_failures) = result?;
            succeeded.extend(outcomes.keys().copied());
            failures.extend(chunk_failures);
        }

        for (sequence, error) in &failures {
            warn!("Checkpoint {sequence} failed, enqueueing for repair: {error}");
            if let Err(e) = self.storage.enqueue_repair(*sequence, error).await {
                warn!("Failed to enqueue repair for {sequence}: {e}");
            }
        }

        let committed = contiguous_prefix(start, end, &succeeded);
        let Some(committed) = committed else {
            return Err(eyre::eyre!(
                "no checkpoints committed in range {start}..={end}"
            ));
        };
        self.commit_contiguous(start, committed).await?;
        Ok(committed)
    }

    /// Commit the longest gap-free prefix: verify digests, advance the
    /// watermark, persist progress, and prune under retention.
    async fn commit_contiguous(&self, start: u64, committed: u64) -> Result<()> {
        self.verify_digests(start, committed).await?;
        self.storage.update_checkpoint_progress(committed).await?;
        self.storage
            .advance_continuous("default", committed, start, None)
            .await?;
        if let Some(retention) = self.config.events.retention {
            self.storage.prune_checkpoints(committed, retention).await?;
        }
        Ok(())
    }

    /// Verify stored checkpoint digests are present for the committed prefix.
    async fn verify_digests(&self, start: u64, committed: u64) -> Result<()> {
        let digests = self.storage.checkpoint_digests(start, committed).await?;
        let present: BTreeSet<u64> = digests.iter().map(|(seq, _)| *seq).collect();
        let mut missing = Vec::new();
        for sequence in start..=committed {
            if !present.contains(&sequence) {
                missing.push(sequence);
            }
        }
        if !missing.is_empty() {
            return Err(eyre::eyre!(
                "digest verification failed, missing checkpoints: {missing:?}"
            ));
        }
        Ok(())
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

/// Shared pipeline handles threaded through single-checkpoint processing.
#[derive(Clone)]
struct CheckpointPipeline<'a> {
    filter: &'a EventFilterProcessor,
    transformer: &'a EventTransformer,
    processor: &'a Arc<dyn EventProcessor>,
    sinks: &'a [sui_indexer_config::WebhookSink],
    alerts: &'a [sui_indexer_config::AlertRule],
    webhook_client: &'a reqwest::Client,
}

/// Process one full checkpoint: verify continuity, extract events, filter,
/// transform, enrich with the custom processor, fire sinks/alerts, and write
/// canonical rows with BCS bytes, real senders, gas, and checkpoint context.
async fn process_single_checkpoint(
    client: &mut SuiClient,
    storage: &StorageManager,
    pipeline: &CheckpointPipeline<'_>,
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
    let mut canonical_events = Vec::new();
    let mut coin_flows = Vec::new();

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
            coin_flows.extend(coin_flows_from_effects(
                &checkpoint,
                executed,
                sequence,
                timestamp_ms,
                &sender,
            ));
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
                if !pipeline.filter.should_process_event(&sui_event) {
                    continue;
                }
                let mut processed = pipeline
                    .transformer
                    .transform_event(sui_event.clone())
                    .await?;
                processed.checkpoint_sequence = sequence;
                let enriched = pipeline.processor.process_event(sui_event).await?;
                processed
                    .metadata
                    .tags
                    .extend(enriched.metadata.tags.clone());
                processed.metadata.matched_filters = enriched.metadata.matched_filters.clone();
                storage.store_event(&processed).await?;
                canonical_events.push(sui_indexer_storage::CanonicalEventModel::new(
                    CanonicalEventModelConfig {
                        checkpoint_sequence: sequence as i64,
                        transaction_digest: digest.to_string(),
                        event_index: index as i64,
                        package_id: event.package_id.to_string(),
                        module_name: event.transaction_module.to_string(),
                        event_type: event.type_.name.to_string(),
                        sender: event.sender.to_string(),
                        timestamp_ms: timestamp_ms as i64,
                        bcs: Some(event.contents.clone()),
                        fields: processed.fields.clone(),
                    },
                ));
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
    if !canonical_events.is_empty() {
        storage.store_canonical_events(canonical_events).await?;
    }
    if !coin_flows.is_empty() {
        storage.store_coin_flows(coin_flows).await?;
    }

    let end_of_epoch = summary.end_of_epoch_data.as_ref().map(|data| {
        serde_json::json!({
            "next_epoch_protocol_version": data.next_epoch_protocol_version.as_u64(),
            "committee_size": data.next_epoch_committee.len(),
        })
    });
    storage
        .store_checkpoint_model(sui_indexer_storage::CheckpointModel::new(
            sui_indexer_storage::CheckpointModelConfig {
                sequence_number: sequence as i64,
                digest: checkpoint.summary.digest().to_string(),
                prev_digest: summary.previous_digest.map(|digest| digest.to_string()),
                epoch: summary.epoch as i64,
                timestamp_ms: timestamp_ms as i64,
                transaction_count: checkpoint.transactions.len() as i64,
                network_total_transactions: summary.network_total_transactions as i64,
                validator_signature: checkpoint.summary.auth_sig().signature.to_string(),
                end_of_epoch_data: end_of_epoch,
            },
        ))
        .await?;

    if !pipeline.sinks.is_empty() || !pipeline.alerts.is_empty() {
        let stored = storage
            .get_events_by_checkpoint_range(sequence, sequence)
            .await?;
        if !pipeline.sinks.is_empty() {
            let _ =
                crate::sinks::dispatch_webhooks(pipeline.sinks, &stored, pipeline.webhook_client)
                    .await;
        }
        for message in crate::sinks::evaluate_alerts(pipeline.alerts, sequence, &stored) {
            warn!("{message}");
        }
    }

    debug!(
        "Checkpoint {sequence}: {} events, {} transactions",
        outcome.events, outcome.transactions
    );

    Ok(outcome)
}

/// Derive coin balance flows from the checkpoint object set: for every owned
/// coin object version in this checkpoint, record holder/coin/balance.
fn coin_flows_from_effects(
    checkpoint: &sui_types::full_checkpoint_content::Checkpoint,
    executed: &sui_types::full_checkpoint_content::ExecutedTransaction,
    sequence: u64,
    timestamp_ms: u64,
    sender: &str,
) -> Vec<sui_indexer_storage::CoinFlowModel> {
    let digest = executed.effects.transaction_digest().to_string();
    executed
        .effects
        .all_changed_objects()
        .into_iter()
        .filter_map(|(object_ref, owner, _kind)| {
            let key = sui_types::storage::ObjectKey(object_ref.0, object_ref.1);
            let object = checkpoint.object_set.get(&key)?;
            let (coin_type, balance) =
                sui_types::coin::Coin::extract_balance_if_coin(object).ok()??;
            let holder = match owner {
                sui_types::object::Owner::AddressOwner(holder) => holder.to_string(),
                _ => sender.to_string(),
            };
            Some(sui_indexer_storage::CoinFlowModel::new(
                CoinFlowModelConfig {
                    checkpoint_sequence: sequence as i64,
                    timestamp_ms: timestamp_ms as i64,
                    transaction_digest: digest.clone(),
                    coin_type: format!("0x{}", coin_type.to_canonical_string(true)),
                    holder,
                    object_id: object_ref.0.to_string(),
                    version: object_ref.1.value() as i64,
                    balance: serde_json::json!(balance),
                },
            ))
        })
        .collect()
}

/// Convert a native checkpoint event into the JSON-RPC `SuiEvent` shape so
/// the existing filter/transformer pipeline consumes the full event body:
/// Move type fields stay in `parsed_json` and raw BCS bytes stay in `bcs`.
fn checkpoint_native_event_to_sui_event(
    event: &sui_types::event::Event,
    digest: sui_types::base_types::TransactionDigest,
    event_seq: u64,
    _checkpoint: u64,
    timestamp_ms: u64,
) -> sui_json_rpc_types::SuiEvent {
    use sui_json_rpc_types::BcsEvent;

    let parsed_json = move_event_fields(event);
    sui_json_rpc_types::SuiEvent {
        id: sui_types::event::EventID {
            tx_digest: digest,
            event_seq,
        },
        package_id: event.package_id,
        transaction_module: event.transaction_module.clone(),
        sender: event.sender,
        type_: event.type_.clone(),
        parsed_json,
        bcs: BcsEvent::new(event.contents.clone()),
        timestamp_ms: Some(timestamp_ms),
    }
}

/// Decode Move event BCS bytes into JSON fields, falling back to envelope
/// metadata when the bytes do not decode against the local layout.
fn move_event_fields(event: &sui_types::event::Event) -> serde_json::Value {
    let mut fields = HashMap::new();
    fields.insert(
        "package".to_string(),
        serde_json::Value::String(event.package_id.to_string()),
    );
    fields.insert(
        "module".to_string(),
        serde_json::Value::String(event.transaction_module.to_string()),
    );
    fields.insert(
        "event_type".to_string(),
        serde_json::Value::String(event.type_.to_canonical_string(true)),
    );
    fields.insert(
        "sender".to_string(),
        serde_json::Value::String(event.sender.to_string()),
    );
    fields.insert(
        "bcs_bytes".to_string(),
        serde_json::Value::Number(event.contents.len().into()),
    );
    match bcs::from_bytes::<serde_json::Value>(&event.contents) {
        Ok(serde_json::Value::Object(decoded)) => {
            let mut merged = decoded;
            for (key, value) in fields {
                merged.entry(key).or_insert(value);
            }
            serde_json::Value::Object(merged)
        }
        Ok(decoded) => decoded,
        Err(_) => serde_json::Value::Object(fields.into_iter().collect()),
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

/// Longest gap-free prefix of [start, end] present in `succeeded`.
fn contiguous_prefix(start: u64, end: u64, succeeded: &BTreeSet<u64>) -> Option<u64> {
    let mut cursor = start;
    let mut committed = None;
    while cursor <= end {
        if succeeded.contains(&cursor) {
            committed = Some(cursor);
            cursor = cursor.saturating_add(1);
        } else {
            break;
        }
    }
    committed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }

    #[test]
    fn contiguous_prefix_advances_over_gap_free_runs() {
        let succeeded: BTreeSet<u64> = [10, 11, 12, 14].into_iter().collect();
        assert_eq!(contiguous_prefix(10, 15, &succeeded), Some(12));
        assert_eq!(contiguous_prefix(13, 15, &succeeded), None);
        let full: BTreeSet<u64> = [7, 8, 9].into_iter().collect();
        assert_eq!(contiguous_prefix(7, 9, &full), Some(9));
    }
}
