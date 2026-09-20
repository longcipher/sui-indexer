use eyre::Result;
use sui_rpc_api::Client as SuiRpcApiClient;
use sui_types::effects::TransactionEffectsAPI;
use sui_types::messages_checkpoint::CheckpointSequenceNumber;
use sui_types::transaction::TransactionDataAPI;
use tracing::{debug, error, info};

use super::{CheckpointData, Event, EventQueryResult};

/// Placeholder for checkpoint subscription
#[derive(Debug, Clone)]
pub struct CheckpointSubscription {
    pub start_sequence: Option<CheckpointSequenceNumber>,
}

/// SuiGrpcClient provides gRPC-based access to Sui blockchain data using the official sui-rpc-api
#[derive(Clone)]
pub struct SuiGrpcClient {
    client: SuiRpcApiClient,
    endpoint: String,
}

impl SuiGrpcClient {
    /// Create a new gRPC client using the official sui-rpc-api
    pub async fn new(endpoint: &str) -> Result<Self> {
        info!("Connecting to Sui gRPC endpoint: {}", endpoint);

        let mut client = SuiRpcApiClient::new(endpoint)
            .map_err(|e| eyre::eyre!("Failed to create gRPC client: {}", e))?;

        // Test the connection
        if let Err(e) = client.get_latest_checkpoint().await {
            error!("Failed to connect to Sui gRPC endpoint {}: {}", endpoint, e);
            return Err(eyre::eyre!("Failed to connect to gRPC endpoint: {}", e));
        }

        info!("Successfully connected to Sui gRPC endpoint: {}", endpoint);

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
        })
    }

    /// Get the latest checkpoint number
    pub async fn get_latest_checkpoint(&mut self) -> Result<CheckpointSequenceNumber> {
        debug!("Fetching latest checkpoint from gRPC");

        let checkpoint_summary = self
            .client
            .get_latest_checkpoint()
            .await
            .map_err(|e| eyre::eyre!("Failed to get latest checkpoint: {}", e))?;
        let sequence_number = checkpoint_summary.sequence_number;

        debug!("Latest checkpoint: {}", sequence_number);
        Ok(sequence_number)
    }

    /// Get full checkpoint data by sequence number.
    ///
    /// Uses the gRPC `LedgerService::getCheckpoint` API with a read mask that
    /// covers summary, contents, transactions (transaction, effects, events)
    /// and objects, then converts the proto response into
    /// `sui_types::full_checkpoint_content::Checkpoint`.
    pub async fn get_full_checkpoint(
        &mut self,
        sequence_number: CheckpointSequenceNumber,
    ) -> Result<sui_types::full_checkpoint_content::Checkpoint> {
        debug!("Fetching full checkpoint {} from gRPC", sequence_number);

        self.client
            .get_full_checkpoint(sequence_number)
            .await
            .map_err(|e| eyre::eyre!("Failed to get full checkpoint {sequence_number}: {e}"))
    }

    /// Get checkpoint data by sequence number
    pub async fn get_checkpoint(
        &self,
        sequence_number: CheckpointSequenceNumber,
    ) -> Result<CheckpointData> {
        debug!("Fetching checkpoint {} from gRPC", sequence_number);

        let mut client = self.client.clone();
        let checkpoint = client
            .get_full_checkpoint(sequence_number)
            .await
            .map_err(|e| eyre::eyre!("Failed to get full checkpoint {sequence_number}: {e}"))?;

        Ok(CheckpointData::from_full_checkpoint(&checkpoint))
    }

    /// Subscribe to checkpoint stream (placeholder for future streaming implementation)
    pub async fn subscribe_checkpoints(
        &self,
        start_sequence: Option<CheckpointSequenceNumber>,
    ) -> Result<CheckpointSubscription> {
        // Note: This is a placeholder. The actual implementation would use
        // the subscription service from sui-rpc-api when available
        info!("Checkpoint subscription via gRPC not yet implemented in sui-rpc-api");
        Ok(CheckpointSubscription { start_sequence })
    }

    /// Query events by filter.
    ///
    /// Checkpoint-anchored scan: for the requested checkpoint window the
    /// client pulls full checkpoints in pages and extracts events locally,
    /// applying package / sender filters. This avoids the simulated-event
    /// path and returns real on-chain events.
    pub async fn query_events(
        &mut self,
        _transaction_digest: Option<String>,
        sender: Option<String>,
        package_id: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
        descending_order: bool,
    ) -> Result<EventQueryResult> {
        self.query_events_in_checkpoints(sender, package_id, cursor, limit, descending_order)
            .await
    }

    /// Scan a checkpoint window for events and apply local filters.
    ///
    /// `cursor` encodes the start checkpoint sequence as a decimal string.
    /// The window covers at most `scan_window` checkpoints ending at the
    /// latest checkpoint (ascending) or starting at it (descending).
    pub async fn query_events_in_checkpoints(
        &mut self,
        sender: Option<String>,
        package_id: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
        descending_order: bool,
    ) -> Result<EventQueryResult> {
        const SCAN_WINDOW: u64 = 20;
        let limit = limit.unwrap_or(50).min(500);

        let latest = self.get_latest_checkpoint().await?;
        let start: u64 = cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or_else(|| latest.saturating_sub(SCAN_WINDOW.saturating_sub(1)));

        let mut sequences: Vec<u64> =
            (start..=latest.min(start.saturating_add(SCAN_WINDOW))).collect();
        if descending_order {
            sequences.reverse();
        }

        let mut events = Vec::new();
        let mut scanned = start;
        for seq in sequences {
            scanned = seq;
            let checkpoint = match self.get_full_checkpoint(seq).await {
                Ok(checkpoint) => checkpoint,
                Err(e) => {
                    debug!("Skipping checkpoint {seq}: {e}");
                    continue;
                }
            };
            for event in checkpoint_events(&checkpoint, seq) {
                if let Some(filter) = &package_id
                    && event.package_id.as_deref().is_none_or(|id| {
                        id != filter && !id.ends_with(filter.trim_start_matches("0x"))
                    })
                {
                    continue;
                }
                if let Some(filter) = &sender
                    && event.sender.as_deref() != Some(filter.as_str())
                {
                    continue;
                }
                events.push(event);
                if events.len() >= limit {
                    break;
                }
            }
            if events.len() >= limit {
                break;
            }
        }

        let has_next_page = scanned < latest && events.len() >= limit;
        let next_cursor = has_next_page.then(|| (scanned.saturating_add(1)).to_string());

        info!(
            "Scanned checkpoints {start}..={scanned} (latest {latest}), found {} events",
            events.len()
        );

        Ok(EventQueryResult {
            data: events,
            next_cursor,
            has_next_page,
        })
    }

    /// Health check for the gRPC connection
    pub async fn health_check(&mut self) -> Result<()> {
        debug!("Performing gRPC health check");

        match self.client.get_latest_checkpoint().await {
            Ok(_) => {
                debug!("gRPC health check passed");
                Ok(())
            }
            Err(e) => {
                error!("gRPC health check failed: {}", e);
                Err(eyre::eyre!("Health check failed: {}", e))
            }
        }
    }

    /// Get the endpoint URL
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Get configuration info (placeholder)
    pub fn config(&self) -> String {
        format!("gRPC endpoint: {}", self.endpoint)
    }
}

impl std::fmt::Debug for SuiGrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuiGrpcClient")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// Extract gRPC-native events from a full checkpoint with checkpoint context.
pub fn checkpoint_events(
    checkpoint: &sui_types::full_checkpoint_content::Checkpoint,
    sequence_number: u64,
) -> Vec<Event> {
    let mut events = Vec::new();
    for transaction in &checkpoint.transactions {
        let sender = transaction.transaction.sender().to_string();
        let digest = transaction.effects.transaction_digest().to_string();
        if let Some(transaction_events) = &transaction.events {
            for (index, event) in transaction_events.data.iter().enumerate() {
                let type_ = event.type_.to_canonical_string(true);
                events.push(Event {
                    event_type: Some(event.type_.name.to_string()),
                    package_id: Some(event.package_id.to_string()),
                    transaction_module: Some(event.transaction_module.to_string()),
                    sender: Some(sender.clone()),
                    type_: Some(type_),
                    contents: Some(serde_json::json!({
                        "checkpoint": sequence_number,
                        "transaction_digest": digest,
                        "event_index": index,
                        "sender": sender,
                        "package_id": event.package_id.to_string(),
                    })),
                    bcs: Some(event.contents.clone()),
                });
            }
        }
    }
    events
}
