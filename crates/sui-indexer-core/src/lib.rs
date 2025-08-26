use std::sync::Arc;

use eyre::Result;
use sui_indexer_config::IndexerConfig;
use sui_indexer_events::{DefaultEventProcessor, EventProcessor};
use sui_indexer_storage::StorageManager;
use tracing::{error, info};

// Local Sui client module
pub mod sui;
pub use sui::SuiClient;

/// Core indexer service
#[derive(Clone)]
pub struct IndexerCore {
    config: IndexerConfig,
    sui_client: SuiClient,
    storage: StorageManager,
    _event_processor: Arc<dyn EventProcessor>, // TODO: Integrate with gRPC event processing
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
            _event_processor: event_processor,
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
            _event_processor: event_processor,
        })
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
    pub async fn start(&self) -> Result<()> {
        info!("✅ Sui Indexer started successfully!");
        info!("🌐 Network: {} (using gRPC)", self.config.network.network);
        info!("🔗 gRPC URL: {}", self.config.network.grpc_url);
        info!("💾 Database: PostgreSQL (connected and migrated)");
        info!("📊 Event batch size: {}", self.config.events.batch_size);
        info!(
            "🔄 Max concurrent batches: {}",
            self.config.events.max_concurrent_batches
        );

        // Display the configured event filters
        info!(
            "📋 Configured {} event filter(s):",
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

        info!("");
        info!("🎉 Sui event indexing is now active!");
        info!("💡 This is a generic Sui blockchain event indexer");
        info!("⚡ Ready to capture events in real-time based on your configuration");
        info!("");

        // Start the event monitoring loop
        let mut shutdown_signal = Box::pin(tokio::signal::ctrl_c());
        let mut event_monitor_interval = tokio::time::interval(std::time::Duration::from_secs(10));

        info!("🔍 Starting event monitoring loop...");
        info!("📡 Polling for events every 10 seconds");

        loop {
            tokio::select! {
                _ = &mut shutdown_signal => {
                    info!("✋ Received shutdown signal (Ctrl+C)");
                    info!("🛑 Stopping Sui indexer...");
                    break;
                }
                _ = event_monitor_interval.tick() => {
                    if let Err(e) = self.poll_and_process_events().await {
                        error!("❌ Error during event polling: {}", e);
                    }
                }
            }
        }

        info!("💤 Indexer shutdown complete. Goodbye!");
        Ok(())
    }

    /// Poll for new events and process them
    async fn poll_and_process_events(&self) -> Result<()> {
        info!("🔍 Polling for new events...");

        // Get latest checkpoint
        match self.sui_client.get_latest_checkpoint().await {
            Ok(latest_checkpoint) => {
                info!("📊 Latest checkpoint: {}", latest_checkpoint);

                // Try to query events for each configured filter
                for (i, filter) in self.config.events.filters.iter().enumerate() {
                    info!(
                        "🔎 Checking filter {}: Package={:?}, Module={:?}, Event={:?}",
                        i + 1,
                        filter.package,
                        filter.module,
                        filter.event_type
                    );

                    match self
                        .sui_client
                        .query_events(
                            None,                   // transaction_digest
                            None,                   // sender
                            filter.package.clone(), // package_id
                            None,                   // cursor
                            Some(50),               // limit
                            false,                  // descending_order
                        )
                        .await
                    {
                        Ok(events) => {
                            if events.data.is_empty() {
                                info!("📭 No events found for filter {}", i + 1);
                            } else {
                                info!("📬 Found {} events for filter {}", events.data.len(), i + 1);

                                // For now, just log the events since we need to convert types
                                // TODO: Convert sui_indexer_sui::Event to SuiEvent for processing
                                for (j, event) in events.data.iter().enumerate() {
                                    info!(
                                        "  📄 Event {}: Type={:?}, Package={:?}",
                                        j + 1,
                                        event.event_type,
                                        event.package_id
                                    );
                                }

                                info!(
                                    "✅ Listed {} events (processing integration pending)",
                                    events.data.len()
                                );
                            }
                        }
                        Err(e) => {
                            error!("❌ Failed to query events for filter {}: {}", i + 1, e);
                        }
                    }
                }
            }
            Err(e) => {
                error!("❌ Failed to get latest checkpoint: {}", e);
            }
        }

        Ok(())
    }

    /// Health check
    pub async fn health_check(&self) -> Result<bool> {
        let sui_healthy = self.sui_client.health_check().await?.healthy;
        let storage_healthy = self.storage.health_check().await?;

        Ok(sui_healthy && storage_healthy)
    }
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
