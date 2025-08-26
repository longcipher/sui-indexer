use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use sui_json_rpc_types::SuiEvent;
use tracing::{debug, info};
use uuid::Uuid;

use crate::{EventMetadata, ProcessedEvent};

/// Trait for processing events
#[async_trait]
pub trait EventProcessor: Send + Sync {
    /// Process a single event
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent>;

    /// Process multiple events in batch
    async fn process_events(&self, events: Vec<SuiEvent>) -> Result<Vec<ProcessedEvent>> {
        let mut results = Vec::new();
        for event in events {
            results.push(self.process_event(event).await?);
        }
        Ok(results)
    }
}

/// Default event processor implementation
pub struct DefaultEventProcessor;

impl DefaultEventProcessor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EventProcessor for DefaultEventProcessor {
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        let start_time = std::time::Instant::now();

        // Check if this is a Navi Protocol event
        let package_id_str = event.package_id.to_string();
        let is_navi_protocol = package_id_str
            .contains("81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f");

        if is_navi_protocol {
            info!(
                "🚀 NAVI PROTOCOL EVENT DETECTED: {} from module {} (tx: {})",
                event.type_.name, event.type_.module, event.id.tx_digest
            );

            // Log detailed event information for Navi Protocol
            info!(
                "📊 Navi Event Data: {}",
                serde_json::to_string_pretty(&event.parsed_json).unwrap_or_default()
            );

            // Special handling for different Navi event types
            match event.type_.name.as_str() {
                name if name.contains("DepositEvent") => {
                    info!(
                        "💰 NAVI DEPOSIT EVENT: User {} made a deposit",
                        event.sender
                    );
                    if let Some(amount) = event.parsed_json.get("amount") {
                        info!("💵 Deposit Amount: {}", amount);
                    }
                    if let Some(coin_type) = event.parsed_json.get("coin_type") {
                        info!("🪙 Coin Type: {}", coin_type);
                    }
                }
                name if name.contains("BorrowEvent") => {
                    info!("🏦 NAVI BORROW EVENT: User {} borrowed funds", event.sender);
                    if let Some(amount) = event.parsed_json.get("amount") {
                        info!("💸 Borrow Amount: {}", amount);
                    }
                    if let Some(coin_type) = event.parsed_json.get("coin_type") {
                        info!("🪙 Coin Type: {}", coin_type);
                    }
                }
                name if name.contains("WithdrawEvent") => {
                    info!(
                        "🏧 NAVI WITHDRAW EVENT: User {} withdrew funds",
                        event.sender
                    );
                }
                name if name.contains("RepayEvent") => {
                    info!("💳 NAVI REPAY EVENT: User {} repaid loan", event.sender);
                }
                _ => {
                    info!(
                        "📋 NAVI OTHER EVENT: {} by {}",
                        event.type_.name, event.sender
                    );
                }
            }
        } else {
            debug!(
                "📝 Processing event: {} from package {} (tx: {})",
                event.type_.name, package_id_str, event.id.tx_digest
            );
        }

        // Extract event fields - simplify for now
        let fields = serde_json::json!({
            "type": event.type_.name.to_string(),
            "parsed_json": event.parsed_json
        });

        let processing_duration = start_time.elapsed().as_millis() as u64;

        let processed_event = ProcessedEvent {
            id: Uuid::new_v4(),
            event: event.clone(),
            transaction_digest: event.id.tx_digest,
            checkpoint_sequence: 0, // Would need to be provided from context
            timestamp: Utc::now(),
            package_id: event.package_id,
            module_name: event.type_.module.to_string(),
            event_type: event.type_.name.to_string(),
            sender: event.sender.to_string(),
            fields,
            metadata: EventMetadata {
                processed_at: Utc::now(),
                processing_duration_ms: processing_duration,
                event_index: 0, // Would need to be provided from context
                matched_filters: if is_navi_protocol {
                    vec!["navi_protocol".to_string()]
                } else {
                    vec![]
                },
                tags: if is_navi_protocol {
                    vec!["navi".to_string(), "defi".to_string()]
                } else {
                    vec![]
                },
            },
        };

        if is_navi_protocol {
            info!(
                "✅ NAVI EVENT PROCESSED: {} (processing time: {}ms)",
                event.type_.name, processing_duration
            );
        }

        Ok(processed_event)
    }

    async fn process_events(&self, events: Vec<SuiEvent>) -> Result<Vec<ProcessedEvent>> {
        let mut processed_events = Vec::new();

        for event in events {
            let processed = self.process_event(event).await?;
            processed_events.push(processed);
        }

        Ok(processed_events)
    }
}

impl Default for DefaultEventProcessor {
    fn default() -> Self {
        Self::new()
    }
}
