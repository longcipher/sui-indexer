use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use sui_indexer_config::{EventFilter, IndexerConfig};
use sui_indexer_core::IndexerCore;
use sui_indexer_events::{EventProcessor, ProcessedEvent};
use sui_json_rpc_types::SuiEvent;
use tracing::info;

/// Simple event processor that logs all events
pub struct SimpleEventProcessor;

impl SimpleEventProcessor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EventProcessor for SimpleEventProcessor {
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        // Log the event
        info!(
            "📝 Event: {} from package {} (tx: {})",
            event.type_.name, event.package_id, event.id.tx_digest
        );

        // Create processed event
        let processed_event = ProcessedEvent {
            id: uuid::Uuid::new_v4(),
            event: event.clone(),
            transaction_digest: event.id.tx_digest,
            checkpoint_sequence: 0,
            timestamp: chrono::Utc::now(),
            package_id: event.package_id,
            module_name: event.type_.module.to_string(),
            event_type: event.type_.name.to_string(),
            sender: event.sender.to_string(),
            fields: event.parsed_json.clone(),
            metadata: sui_indexer_events::EventMetadata {
                processed_at: chrono::Utc::now(),
                processing_duration_ms: 1,
                event_index: 0,
                matched_filters: vec![],
                tags: vec!["simple".to_string()],
            },
        };

        Ok(processed_event)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    info!("🚀 Starting Simple Event Indexer");

    // Create basic configuration
    let mut config = IndexerConfig::default();
    config.network.grpc_url = "https://fullnode.testnet.sui.io:443"
        .parse()
        .expect("Valid URL");
    config.network.network = "testnet".to_string();

    // Add a simple filter to monitor coin events
    config.events.filters = vec![EventFilter {
        package: Some("0x2".to_string()),
        module: Some("coin".to_string()),
        event_type: None,
        sender: None,
    }];

    // Create simple processor
    let processor = Arc::new(SimpleEventProcessor::new());

    // Create and start indexer
    let indexer = IndexerCore::with_event_processor(config, processor).await?;
    indexer.initialize().await?;
    indexer.start().await?;

    Ok(())
}
