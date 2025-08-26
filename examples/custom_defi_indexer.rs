use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use serde_json::Value;
use sui_indexer_config::{EventFilter, IndexerConfig};
use sui_indexer_core::IndexerCore;
use sui_indexer_events::{EventProcessor, ProcessedEvent};
use sui_json_rpc_types::SuiEvent;
use tracing::{info, warn};

/// Custom DeFi event processor
/// This example shows how to create a custom processor for monitoring DeFi protocols
/// using Navi Protocol as an example
pub struct DeFiEventProcessor {
    // Protocol-specific configurations
    navi_package_id: String,
    // Add other protocols as needed
    // compound_package_id: String,
    // aave_package_id: String,
}

impl DeFiEventProcessor {
    pub fn new() -> Self {
        Self {
            // Navi Protocol package ID for demonstration
            navi_package_id: "0x81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f"
                .to_string(),
        }
    }

    /// Handle Navi Protocol specific events
    async fn handle_navi_event(&self, event: &SuiEvent) -> Result<()> {
        let event_type = &event.type_.name;

        match event_type.as_str() {
            name if name.contains("DepositEvent") => {
                self.handle_deposit_event(event).await?;
            }
            name if name.contains("BorrowEvent") => {
                self.handle_borrow_event(event).await?;
            }
            name if name.contains("WithdrawEvent") => {
                self.handle_withdraw_event(event).await?;
            }
            name if name.contains("RepayEvent") => {
                self.handle_repay_event(event).await?;
            }
            name if name.contains("LiquidationEvent") => {
                self.handle_liquidation_event(event).await?;
            }
            _ => {
                info!("📋 Other Navi event: {} from {}", event_type, event.sender);
            }
        }

        Ok(())
    }

    /// Handle deposit events with detailed analysis
    async fn handle_deposit_event(&self, event: &SuiEvent) -> Result<()> {
        info!("💰 Deposit Event Detected");
        info!("  👤 User: {}", event.sender);
        info!("  📦 Transaction: {}", event.id.tx_digest);

        // Extract event data
        if let Some(amount) = self.extract_field(&event.parsed_json, "amount") {
            info!("  💵 Amount: {}", amount);
        }

        if let Some(asset_type) = self.extract_field(&event.parsed_json, "coin_type") {
            info!("  🪙 Asset: {}", asset_type);
        }

        if let Some(pool_id) = self.extract_field(&event.parsed_json, "pool_id") {
            info!("  🏊 Pool: {}", pool_id);
        }

        // Custom business logic
        self.update_user_portfolio(&event.sender.to_string(), "deposit", &event.parsed_json)
            .await?;

        self.calculate_tvl_impact(&event.parsed_json).await?;

        Ok(())
    }

    /// Handle borrow events
    async fn handle_borrow_event(&self, event: &SuiEvent) -> Result<()> {
        info!("🏦 Borrow Event Detected");
        info!("  👤 Borrower: {}", event.sender);

        if let Some(amount) = self.extract_field(&event.parsed_json, "amount") {
            info!("  💸 Borrowed: {}", amount);
        }

        if let Some(asset_type) = self.extract_field(&event.parsed_json, "coin_type") {
            info!("  🪙 Asset: {}", asset_type);
        }

        // Custom logic for borrow tracking
        self.update_debt_position(&event.sender.to_string(), &event.parsed_json)
            .await?;
        self.calculate_utilization_rate(&event.parsed_json).await?;

        Ok(())
    }

    /// Handle withdraw events
    async fn handle_withdraw_event(&self, event: &SuiEvent) -> Result<()> {
        info!("🏧 Withdraw Event Detected");
        info!("  👤 User: {}", event.sender);

        if let Some(amount) = self.extract_field(&event.parsed_json, "amount") {
            info!("  💸 Withdrawn: {}", amount);
        }

        // Update user positions
        self.update_user_portfolio(&event.sender.to_string(), "withdraw", &event.parsed_json)
            .await?;

        Ok(())
    }

    /// Handle repay events
    async fn handle_repay_event(&self, event: &SuiEvent) -> Result<()> {
        info!("💳 Repay Event Detected");
        info!("  👤 User: {}", event.sender);

        if let Some(amount) = self.extract_field(&event.parsed_json, "amount") {
            info!("  💰 Repaid: {}", amount);
        }

        // Update debt tracking
        self.update_debt_position(&event.sender.to_string(), &event.parsed_json)
            .await?;

        Ok(())
    }

    /// Handle liquidation events
    async fn handle_liquidation_event(&self, event: &SuiEvent) -> Result<()> {
        warn!("🚨 Liquidation Event Detected");
        info!("  👤 Liquidated User: {}", event.sender);

        if let Some(liquidator) = self.extract_field(&event.parsed_json, "liquidator") {
            info!("  🔨 Liquidator: {}", liquidator);
        }

        if let Some(amount) = self.extract_field(&event.parsed_json, "liquidated_amount") {
            info!("  💥 Liquidated Amount: {}", amount);
        }

        // Alert systems or risk management
        self.handle_liquidation_alert(&event.parsed_json).await?;

        Ok(())
    }

    /// Extract field from parsed JSON
    fn extract_field(&self, json: &Value, field: &str) -> Option<String> {
        json.get(field).and_then(|v| {
            // Try string first
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if let Some(n) = v.as_number() {
                Some(n.to_string())
            } else {
                None
            }
        })
    }

    /// Update user portfolio (placeholder for custom business logic)
    async fn update_user_portfolio(
        &self,
        user: &str,
        action: &str,
        _event_data: &Value,
    ) -> Result<()> {
        // Implement your custom portfolio tracking logic here
        info!(
            "📊 Updating portfolio for user {} (action: {})",
            user, action
        );

        // Example: Store in your custom database, update cache, etc.
        // let portfolio = self.portfolio_service.get_or_create(user).await?;
        // portfolio.update_from_event(action, event_data).await?;

        Ok(())
    }

    /// Calculate TVL impact
    async fn calculate_tvl_impact(&self, _event_data: &Value) -> Result<()> {
        // Implement TVL calculation logic
        info!("📈 Calculating TVL impact from event");

        // Example implementation:
        // if let Some(amount) = self.extract_field(event_data, "amount") {
        //     let amount_num: f64 = amount.parse().unwrap_or(0.0);
        //     self.tvl_tracker.add_deposit(amount_num).await?;
        // }

        Ok(())
    }

    /// Update debt position tracking
    async fn update_debt_position(&self, user: &str, _event_data: &Value) -> Result<()> {
        info!("📊 Updating debt position for user {}", user);

        // Implement debt tracking logic
        // let debt_tracker = self.debt_service.get_user_debt(user).await?;
        // debt_tracker.update_from_event(event_data).await?;

        Ok(())
    }

    /// Calculate utilization rate
    async fn calculate_utilization_rate(&self, _event_data: &Value) -> Result<()> {
        info!("📊 Calculating pool utilization rate");

        // Implement utilization calculation
        // if let Some(pool_id) = self.extract_field(event_data, "pool_id") {
        //     let pool = self.pool_service.get_pool(&pool_id).await?;
        //     let utilization = pool.calculate_utilization().await?;
        //     info!("📈 Pool {} utilization: {:.2}%", pool_id, utilization * 100.0);
        // }

        Ok(())
    }

    /// Handle liquidation alerts
    async fn handle_liquidation_alert(&self, _event_data: &Value) -> Result<()> {
        warn!("🚨 Liquidation alert triggered");

        // Implement alerting logic
        // self.alert_service.send_liquidation_alert(event_data).await?;
        // self.risk_monitor.update_risk_metrics(event_data).await?;

        Ok(())
    }
}

#[async_trait]
impl EventProcessor for DeFiEventProcessor {
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        let start_time = std::time::Instant::now();
        let package_id_str = event.package_id.to_string();

        // Check if this is a protocol we're monitoring
        if package_id_str.contains(&self.navi_package_id) {
            info!("🎯 Navi Protocol Event: {}", event.type_.name);

            // Handle Navi-specific events
            if let Err(e) = self.handle_navi_event(&event).await {
                warn!("⚠️ Error processing Navi event: {}", e);
            }
        } else {
            // Handle other blockchain events
            info!(
                "📝 Generic event: {} from {}",
                event.type_.name, package_id_str
            );
        }

        let processing_duration = start_time.elapsed().as_millis() as u64;

        // Create processed event with metadata
        let processed_event = ProcessedEvent {
            id: uuid::Uuid::new_v4(),
            event: event.clone(),
            transaction_digest: event.id.tx_digest,
            checkpoint_sequence: 0, // Would be provided from context
            timestamp: chrono::Utc::now(),
            package_id: event.package_id,
            module_name: event.type_.module.to_string(),
            event_type: event.type_.name.to_string(),
            sender: event.sender.to_string(),
            fields: event.parsed_json.clone(),
            metadata: sui_indexer_events::EventMetadata {
                processed_at: chrono::Utc::now(),
                processing_duration_ms: processing_duration,
                event_index: 0,
                matched_filters: if package_id_str.contains(&self.navi_package_id) {
                    vec!["navi_protocol".to_string()]
                } else {
                    vec![]
                },
                tags: if package_id_str.contains(&self.navi_package_id) {
                    vec![
                        "navi".to_string(),
                        "defi".to_string(),
                        "lending".to_string(),
                    ]
                } else {
                    vec!["blockchain".to_string()]
                },
            },
        };

        info!("✅ Event processed in {}ms", processing_duration);
        Ok(processed_event)
    }
}

/// Create custom configuration for DeFi monitoring
fn create_defi_config() -> IndexerConfig {
    let mut config = IndexerConfig::default();

    // Configure for mainnet
    config.network.grpc_url = "https://fullnode.mainnet.sui.io:443"
        .parse()
        .expect("Valid URL");
    config.network.network = "mainnet".to_string();

    // Database configuration
    config.database.url =
        "postgresql://postgres:password@localhost:5433/sui_defi_indexer".to_string();
    config.database.max_connections = 20;

    // Event configuration
    config.events.batch_size = 50;
    config.events.max_concurrent_batches = 4;

    // Add filters for Navi Protocol events
    config.events.filters = vec![
        // Navi Deposit Events
        EventFilter {
            package: Some("0x81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f".to_string()),
            module: Some("lending".to_string()),
            event_type: Some("0xd899cf7d2b5db716bd2cf55599fb0d5ee38a3061e7b6bb6eebf73fa5bc4c81ca::lending::DepositEvent".to_string()),
            sender: None,
        },
        // Navi Borrow Events
        EventFilter {
            package: Some("0x81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f".to_string()),
            module: Some("lending".to_string()),
            event_type: Some("0xd899cf7d2b5db716bd2cf55599fb0d5ee38a3061e7b6bb6eebf73fa5bc4c81ca::lending::BorrowEvent".to_string()),
            sender: None,
        },
        // Add more event types as needed
        // EventFilter {
        //     package: Some("0x81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f".to_string()),
        //     module: Some("lending".to_string()),
        //     event_type: Some("0xd899cf7d2b5db716bd2cf55599fb0d5ee38a3061e7b6bb6eebf73fa5bc4c81ca::lending::WithdrawEvent".to_string()),
        //     sender: None,
        // },
    ];

    config
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    info!("🚀 Starting Custom DeFi Indexer");
    info!("📊 Monitoring Navi Protocol and other DeFi events");

    // Create configuration
    let config = create_defi_config();

    // Create custom event processor
    let processor = Arc::new(DeFiEventProcessor::new());

    // Create indexer with custom processor
    let indexer = IndexerCore::with_event_processor(config, processor).await?;

    // Initialize database and run migrations
    info!("🔧 Initializing indexer...");
    indexer.initialize().await?;

    // Start indexing with custom processing
    info!("▶️ Starting custom DeFi event indexing...");
    indexer.start().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_extract_field() {
        let processor = DeFiEventProcessor::new();
        let json = json!({
            "amount": "1000000000",
            "coin_type": "0x2::sui::SUI",
            "user": "0x123..."
        });

        assert_eq!(
            processor.extract_field(&json, "amount"),
            Some("1000000000".to_string())
        );
        assert_eq!(
            processor.extract_field(&json, "coin_type"),
            Some("0x2::sui::SUI".to_string())
        );
        assert_eq!(processor.extract_field(&json, "nonexistent"), None);
    }

    #[test]
    fn test_defi_config_creation() {
        let config = create_defi_config();
        assert_eq!(config.network.network, "mainnet");
        assert!(!config.events.filters.is_empty());
        assert_eq!(config.events.batch_size, 50);
    }
}
