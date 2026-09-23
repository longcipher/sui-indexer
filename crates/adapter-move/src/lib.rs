//! Move (Sui) adapter: today's Sui fetch/decode behind [`ChainAdapter`].
//!
//! Height is the checkpoint sequence. Checkpoints are final, so the reorg
//! path is a no-op and continuity is verified through `prev_digest`.

use std::ops::Range;

use async_trait::async_trait;
use chain_core::{
    ChainAdapter, ChainError, ChainKind, ChainSchema, ColumnDescriptor, CommitmentModel,
    DecodedBlock, ParentRef,
};
use tracing::debug;

pub mod decode;
pub mod factory;

pub use decode::{decode_checkpoint_summary, emitter_of, event_topics};
pub use factory::MoveAdapterFactory;

/// Sui adapter: batched `get_full_checkpoint` fetch + decode to rows.
pub struct MoveAdapter {
    endpoint: String,
    schema: ChainSchema,
    client: tokio::sync::Mutex<sui_rpc_api::Client>,
}

impl MoveAdapter {
    /// Connect to `endpoint` and verify with a tip probe.
    pub async fn new(endpoint: String) -> Result<Self, ChainError> {
        let mut client = sui_rpc_api::Client::new(&endpoint)
            .map_err(|e| ChainError::Transport(format!("move connect {endpoint}: {e}")))?;
        client
            .get_latest_checkpoint()
            .await
            .map_err(|e| ChainError::Transport(format!("move tip probe {endpoint}: {e}")))?;
        Ok(Self {
            endpoint,
            schema: move_schema(),
            client: tokio::sync::Mutex::new(client),
        })
    }

    /// Build without a network probe (tests, offline planning).
    #[must_use]
    pub fn offline(endpoint: String) -> Self {
        // ponytail: offline adapter still needs a client value; connecting is
        // lazy and the tip probe is skipped. Reconnect explicitly via
        // `reconnect()` before serving traffic.
        let client = sui_rpc_api::Client::new(&endpoint).unwrap_or_else(|_| {
            // Client::new only fails on an invalid endpoint; fall back to
            // a loopback placeholder so offline construction stays total.
            sui_rpc_api::Client::new("http://127.0.0.1:1").expect("loopback endpoint parses")
        });
        Self {
            endpoint,
            schema: move_schema(),
            client: tokio::sync::Mutex::new(client),
        }
    }

    /// Endpoint this adapter talks to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Native tables kept for Move fidelity: checkpoints, transactions, objects,
/// coin flows. Skeleton tables stay chain-independent.
#[must_use]
pub fn move_schema() -> ChainSchema {
    let text = |name: &str| ColumnDescriptor {
        name: name.to_owned(),
        sql_type: "TEXT".to_owned(),
        nullable: true,
    };
    let bigint = |name: &str| ColumnDescriptor {
        name: name.to_owned(),
        sql_type: "BIGINT".to_owned(),
        nullable: false,
    };
    ChainSchema {
        kind: ChainKind::Move,
        native_tables: vec![
            (
                "checkpoints".to_owned(),
                vec![
                    bigint("sequence_number"),
                    text("digest"),
                    text("prev_digest"),
                    bigint("epoch"),
                    bigint("timestamp_ms"),
                ],
            ),
            (
                "transactions".to_owned(),
                vec![
                    text("digest"),
                    bigint("checkpoint_sequence"),
                    text("sender"),
                    bigint("gas_used"),
                ],
            ),
            (
                "objects".to_owned(),
                vec![text("object_id"), bigint("version"), text("digest")],
            ),
            (
                "coin_flows".to_owned(),
                vec![text("coin_type"), text("holder"), bigint("balance")],
            ),
        ],
    }
}

#[async_trait]
impl ChainAdapter for MoveAdapter {
    fn kind(&self) -> ChainKind {
        ChainKind::Move
    }

    fn commitment(&self) -> CommitmentModel {
        CommitmentModel::Final
    }

    fn schema(&self) -> &ChainSchema {
        &self.schema
    }

    async fn head(&self) -> Result<u64, ChainError> {
        let mut client = self.client.lock().await;
        let summary = client
            .get_latest_checkpoint()
            .await
            .map_err(|e| ChainError::Transport(format!("move head: {e}")))?;
        Ok(summary.sequence_number)
    }

    async fn fetch(&self, range: Range<u64>) -> Result<Vec<DecodedBlock>, ChainError> {
        if range.start >= range.end {
            return Err(ChainError::InvalidRange(range.start, range.end));
        }
        debug!("move fetch [{}, {})", range.start, range.end);
        let mut out = Vec::with_capacity((range.end - range.start) as usize);
        for height in range {
            let checkpoint = {
                let mut client = self.client.lock().await;
                client.get_full_checkpoint(height).await.map_err(|e| {
                    ChainError::Transport(format!("move fetch checkpoint {height}: {e}"))
                })?
            };
            out.push(decode::decode_checkpoint(&checkpoint));
        }
        Ok(out)
    }

    fn parent_ref(&self, block: &DecodedBlock) -> ParentRef {
        block.parent.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    #[test]
    fn schema_covers_move_native_tables() {
        let schema = move_schema();
        assert_eq!(schema.kind, ChainKind::Move);
        let names: Vec<&str> = schema
            .native_tables
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["checkpoints", "transactions", "objects", "coin_flows"]
        );
    }

    #[tokio::test]
    async fn adapter_reports_final_commitment() {
        let adapter = MoveAdapter::offline("http://127.0.0.1:1".to_owned());
        assert_eq!(adapter.kind(), ChainKind::Move);
        assert_eq!(adapter.commitment(), CommitmentModel::Final);
        assert_eq!(adapter.endpoint(), "http://127.0.0.1:1");
        assert_eq!(adapter.schema().native_tables.len(), 4);
        // The override forwards the block's own parent link.
        let block = decode::decode_checkpoint_summary(7, &"ab".repeat(32), "0x0", 0, 0);
        assert_eq!(adapter.parent_ref(&block).hash, block.parent.hash);
    }

    #[tokio::test]
    async fn fetch_rejects_empty_and_inverted_ranges() {
        let adapter = MoveAdapter::offline("http://127.0.0.1:1".to_owned());
        assert!(adapter.fetch(5..5).await.is_err());
        assert!(adapter.fetch(Range { start: 9, end: 5 }).await.is_err());
    }

    #[tokio::test]
    async fn head_and_fetch_fail_without_server() {
        // Nothing listens on port 1: the failure is deterministic and fast.
        // This pins the error paths that mutants would turn into `Ok`.
        let adapter = MoveAdapter::offline("http://127.0.0.1:1".to_owned());
        assert!(adapter.head().await.is_err());
        assert!(adapter.fetch(0..1).await.is_err());
    }
}
