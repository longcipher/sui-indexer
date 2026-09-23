//! Solana (`svm`) adapter: slots, skipped markers, inner instructions.
//!
//! Height is the slot. Batch fetch is `getBlock(slot,
//! {transactionDetails:"full", rewards:false, maxSupportedTransactionVersion:0})`.
//! `getBlock` returns `null` for skipped slots — normal, not an error: the
//! gap detector records a marker and does not retry. Detection runs at
//! `confirmed` with block-scoped rollback; latency-insensitive jobs can wait
//! for `final`.

use std::ops::Range;
use std::time::Duration;

use async_trait::async_trait;
use chain_core::{
    ChainAdapter, ChainError, ChainKind, ChainSchema, ColumnDescriptor, Commitment,
    CommitmentModel, DecodedBlock,
};
use tracing::debug;

pub mod decode;
pub mod factory;
pub mod rpc;

pub use decode::{decode_block_value, discriminator_hex};
pub use factory::SolanaAdapterFactory;
pub use rpc::{BlockValue, CommitmentLevel, RpcClient};

/// Default `maxSupportedTransactionVersion` for `getBlock`.
pub const MAX_SUPPORTED_TX_VERSION: u8 = 0;

/// Solana adapter: JSON-RPC `getBlock` fetch + decode to rows.
pub struct SolanaAdapter {
    endpoint: String,
    schema: ChainSchema,
    rpc: RpcClient,
    commitment: CommitmentLevel,
    max_depth: u64,
}

impl SolanaAdapter {
    /// Build against `endpoint` at `commitment` (default `confirmed`).
    #[must_use]
    pub fn new(endpoint: String, commitment: CommitmentLevel) -> Self {
        Self {
            rpc: RpcClient::new(endpoint.clone()),
            endpoint,
            schema: svm_schema(),
            commitment,
            max_depth: 128,
        }
    }

    /// Current commitment level.
    #[must_use]
    pub fn commitment_level(&self) -> CommitmentLevel {
        self.commitment
    }

    /// Endpoint this adapter talks to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Native tables kept for SVM fidelity: account deltas, program logs,
/// rewards. Inner instructions are first-class in `events`.
#[must_use]
pub fn svm_schema() -> ChainSchema {
    let text = |name: &str| ColumnDescriptor {
        name: name.to_owned(),
        sql_type: "TEXT".to_owned(),
        nullable: true,
    };
    let bigint = |name: &str, nullable: bool| ColumnDescriptor {
        name: name.to_owned(),
        sql_type: "BIGINT".to_owned(),
        nullable,
    };
    ChainSchema {
        kind: ChainKind::Svm,
        native_tables: vec![
            (
                "account_deltas".to_owned(),
                vec![
                    bigint("slot", false),
                    text("pubkey"),
                    bigint("pre_balance", true),
                    bigint("post_balance", true),
                ],
            ),
            (
                "program_logs".to_owned(),
                vec![bigint("slot", false), text("program_id"), text("message")],
            ),
            (
                "rewards".to_owned(),
                vec![
                    bigint("slot", false),
                    text("pubkey"),
                    bigint("lamports", true),
                ],
            ),
        ],
    }
}

#[async_trait]
impl ChainAdapter for SolanaAdapter {
    fn kind(&self) -> ChainKind {
        ChainKind::Svm
    }

    fn commitment(&self) -> CommitmentModel {
        match self.commitment {
            CommitmentLevel::Finalized => CommitmentModel::Final,
            CommitmentLevel::Confirmed => CommitmentModel::Reorgable {
                max_depth: self.max_depth,
            },
        }
    }

    fn schema(&self) -> &ChainSchema {
        &self.schema
    }

    async fn head(&self) -> Result<u64, ChainError> {
        self.rpc.get_slot(self.commitment).await
    }

    async fn fetch(&self, range: Range<u64>) -> Result<Vec<DecodedBlock>, ChainError> {
        if range.start >= range.end {
            return Err(ChainError::InvalidRange(range.start, range.end));
        }
        debug!("svm fetch [{}, {})", range.start, range.end);
        let slots: Vec<u64> = range.collect();
        let mut out = Vec::with_capacity(slots.len());
        // Sequential by default; the sync engine parallelises across ranges.
        // ponytail: sequential loop, JoinSet batching if RPC latency dominates.
        for slot in slots {
            match self.rpc.get_block(slot, self.commitment).await? {
                None => out.push(DecodedBlock::skipped(slot)),
                Some(value) => out.push(decode_block_value(
                    slot,
                    &value,
                    commitment_of(self.commitment),
                )),
            }
            // Be polite to public endpoints between slots.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(out)
    }
}

fn commitment_of(level: CommitmentLevel) -> Commitment {
    match level {
        CommitmentLevel::Confirmed => Commitment::Confirmed,
        CommitmentLevel::Finalized => Commitment::Final,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    #[test]
    fn schema_covers_svm_native_tables() {
        let schema = svm_schema();
        assert_eq!(schema.kind, ChainKind::Svm);
        let names: Vec<&str> = schema
            .native_tables
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["account_deltas", "program_logs", "rewards"]);
    }

    #[test]
    fn commitment_model_follows_level() {
        let confirmed = SolanaAdapter::new("http://x".to_owned(), CommitmentLevel::Confirmed);
        assert_eq!(
            confirmed.commitment(),
            CommitmentModel::Reorgable { max_depth: 128 }
        );
        assert_eq!(confirmed.commitment_level(), CommitmentLevel::Confirmed);
        assert_eq!(confirmed.endpoint(), "http://x");
        assert_eq!(confirmed.kind(), ChainKind::Svm);
        assert_eq!(confirmed.schema().native_tables.len(), 3);
        let finalized = SolanaAdapter::new("http://x".to_owned(), CommitmentLevel::Finalized);
        assert_eq!(finalized.commitment(), CommitmentModel::Final);
        assert_eq!(finalized.commitment_level(), CommitmentLevel::Finalized);
    }

    #[tokio::test]
    async fn fetch_rejects_empty_and_inverted_ranges() {
        let adapter =
            SolanaAdapter::new("http://127.0.0.1:1".to_owned(), CommitmentLevel::Confirmed);
        assert!(adapter.fetch(5..5).await.is_err());
        assert!(adapter.fetch(Range { start: 9, end: 5 }).await.is_err());
    }
}
