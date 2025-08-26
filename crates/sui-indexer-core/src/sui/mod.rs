use eyre::Result;
use sui_indexer_config::NetworkConfig;
use tokio::time::Duration;

/// gRPC Event type (pure gRPC)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub event_type: Option<String>,
    pub package_id: Option<String>,
    pub transaction_module: Option<String>,
    pub sender: Option<String>,
    pub type_: Option<String>,
    pub contents: Option<serde_json::Value>,
    pub bcs: Option<Vec<u8>>,
}

pub mod checkpoint;
pub mod grpc_client;

// Re-export the main types from checkpoint module
pub use checkpoint::{CheckpointData, CheckpointProcessor, CheckpointRange, CheckpointStats};
pub use grpc_client::{CheckpointSubscription, SuiGrpcClient};

/// Event query result using pure gRPC types
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EventQueryResult {
    pub data: Vec<Event>,
    pub next_cursor: Option<String>,
    pub has_next_page: bool,
}

/// Health status for monitoring (pure gRPC only)
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthStatus {
    pub healthy: bool,
    pub latest_checkpoint: Option<u64>,
    pub latency: Option<Duration>,
    pub error: Option<String>,
}

/// Sui client wrapper for pure gRPC communication
#[derive(Debug, Clone)]
pub struct SuiClient {
    grpc_client: SuiGrpcClient,
    config: NetworkConfig,
}

impl SuiClient {
    /// Create a new Sui client with gRPC-only mode
    pub async fn new_grpc_only(config: NetworkConfig) -> Result<Self> {
        let grpc_client = SuiGrpcClient::new(config.grpc_url.as_str()).await?;

        Ok(SuiClient {
            grpc_client,
            config,
        })
    }

    /// Create a new Sui client (alias for new_grpc_only)
    pub async fn new(config: NetworkConfig) -> Result<Self> {
        Self::new_grpc_only(config).await
    }

    /// Get the latest checkpoint sequence number
    pub async fn get_latest_checkpoint(&self) -> Result<u64> {
        self.grpc_client.get_latest_checkpoint().await
    }

    /// Get checkpoint data by sequence number
    pub async fn get_checkpoint(&self, sequence_number: u64) -> Result<CheckpointData> {
        self.grpc_client.get_checkpoint(sequence_number).await
    }

    /// Subscribe to checkpoint updates (pure gRPC streaming)
    pub async fn subscribe_checkpoints(
        &self,
        start_sequence: Option<u64>,
    ) -> Result<CheckpointSubscription> {
        self.grpc_client.subscribe_checkpoints(start_sequence).await
    }

    /// Query events with filter using pure gRPC
    pub async fn query_events(
        &self,
        transaction_digest: Option<String>,
        sender: Option<String>,
        package_id: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
        descending_order: bool,
    ) -> Result<EventQueryResult> {
        self.grpc_client
            .query_events(
                transaction_digest,
                sender,
                package_id,
                cursor,
                limit,
                descending_order,
            )
            .await
    }

    /// Get the network configuration
    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Get health status
    pub async fn health_check(&self) -> Result<HealthStatus> {
        let start = std::time::Instant::now();

        match self.grpc_client.health_check().await {
            Ok(_) => {
                if let Ok(checkpoint) = self.grpc_client.get_latest_checkpoint().await {
                    Ok(HealthStatus {
                        healthy: true,
                        latest_checkpoint: Some(checkpoint),
                        latency: Some(start.elapsed()),
                        error: None,
                    })
                } else {
                    Ok(HealthStatus {
                        healthy: true,
                        latest_checkpoint: None,
                        latency: Some(start.elapsed()),
                        error: None,
                    })
                }
            }
            Err(e) => Ok(HealthStatus {
                healthy: false,
                latest_checkpoint: None,
                latency: Some(start.elapsed()),
                error: Some(e.to_string()),
            }),
        }
    }
}
