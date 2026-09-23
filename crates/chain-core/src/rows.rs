//! Decoded-row contract: the only data the engine ever sees.
//!
//! Adapters fetch *and* decode. The engine never sees a chain-native block
//! type; it only sees [`DecodedBlock`] rows destined for the skeleton
//! (`blocks`, `txs`, `events`) and native tables.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{ChainKind, Commitment, ParentRef};

/// One fully decoded height: skeleton rows plus native rows.
///
/// `height` is the universal cursor: block number (EVM), slot (Solana),
/// checkpoint sequence (Sui).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedBlock {
    /// Monotone integer cursor.
    pub height: u64,
    /// Canonical block/checkpoint hash (32 bytes where the chain has one).
    pub hash: Vec<u8>,
    /// Parent reference used for fork detection.
    pub parent: ParentRef,
    /// Block timestamp.
    pub ts: DateTime<Utc>,
    /// Finality of this row at fetch time.
    pub commitment: Commitment,
    /// Skeleton `blocks` + `txs` rows.
    pub rows: CoreRows,
    /// Universal event projection: every rule reads this table.
    pub events: Vec<Ev>,
    /// Chain-specific rows (EVM receipts, SVM account deltas, Move objects).
    pub native: Vec<RowSet>,
    /// True for Solana skipped slots: a marker, not missing data.
    pub skipped: bool,
}

impl DecodedBlock {
    /// A skipped-slot marker carries no rows and must not be retried.
    #[must_use]
    pub fn skipped(height: u64) -> Self {
        Self {
            height,
            hash: Vec::new(),
            parent: ParentRef::default(),
            ts: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            commitment: Commitment::Final,
            rows: CoreRows::default(),
            events: Vec::new(),
            native: Vec::new(),
            skipped: true,
        }
    }
}

/// Skeleton `blocks` + `txs` rows for one height.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoreRows {
    /// The single `blocks` row for this height.
    pub block: Option<BlockRow>,
    /// Zero or more `txs` rows for this height.
    pub txs: Vec<TxRow>,
}

/// One skeleton `blocks` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRow {
    /// Block height.
    pub height: u64,
    /// Block hash bytes.
    pub hash: Vec<u8>,
    /// Parent hash bytes.
    pub parent_hash: Vec<u8>,
    /// Chain-specific parent identity (JSON).
    pub parent_ref: serde_json::Value,
    /// Block timestamp.
    pub ts: DateTime<Utc>,
    /// `blocks.commitment` integer encoding.
    pub commitment: i16,
    /// Producer, gas/compute, slot flags, …
    pub chain_meta: serde_json::Value,
}

/// One skeleton `txs` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TxRow {
    /// Block height.
    pub height: u64,
    /// Block timestamp.
    pub block_ts: DateTime<Utc>,
    /// Index of the transaction inside the block.
    pub tx_index: u32,
    /// Transaction hash / signature / digest bytes.
    pub tx_hash: Vec<u8>,
    /// Sender address (chain-native encoding, hex or base58 string).
    pub sender: String,
    /// Success flag.
    pub success: bool,
    /// Fee paid in the chain's native accounting unit.
    pub fee: i128,
    /// Remaining chain-specific fields.
    pub chain_meta: serde_json::Value,
}

/// One universal `events` row.
///
/// Column mapping: EVM log `address` / Solana `program_id` /
/// Move `package::module` all land in `emitter`; topic/discriminator/event-type
/// land in `topics`; raw payload in `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ev {
    /// Block height.
    pub height: u64,
    /// Block timestamp.
    pub block_ts: DateTime<Utc>,
    /// Transaction index inside the block.
    pub tx_index: u32,
    /// Event index inside the transaction (outer ordering).
    pub ev_index: u32,
    /// Inner-instruction index (Solana); 0 otherwise.
    pub inner_ix: u32,
    /// Stack height (Solana); 0 otherwise.
    pub stack_height: u32,
    /// Emitter: log address / program id / `package::module`.
    pub emitter: String,
    /// Topics: EVM `topic0..3` / Anchor discriminator / Move event type.
    pub topics: Vec<String>,
    /// Raw payload bytes (log data / instruction data / BCS bytes).
    pub payload: Vec<u8>,
    /// Transaction hash / signature / digest bytes.
    pub tx_hash: Vec<u8>,
    /// Sender / signer.
    pub sender: String,
    /// Chain-specific remainder (CU, priority fee, tip, checkpoint, …).
    pub extra: serde_json::Value,
}

impl Ev {
    /// Ordering key MEV detection depends on: `(height, tx_index, ev_index)`.
    ///
    /// Inner instructions sort *after* their outer instruction and keep
    /// `(inner_ix, stack_height)` as tie-breakers.
    #[must_use]
    pub fn ordering_key(&self) -> (u64, u32, u32, u32, u32) {
        (
            self.height,
            self.tx_index,
            self.ev_index,
            self.inner_ix,
            self.stack_height,
        )
    }
}

/// One native-table row set: `(table, JSON rows)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowSet {
    /// Native table name.
    pub table: String,
    /// One JSON object per row.
    pub rows: Vec<serde_json::Value>,
}

/// Block metadata handed to WASM rules alongside the event window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockMeta {
    /// Block height.
    pub height: u64,
    /// Block timestamp.
    pub ts: DateTime<Utc>,
    /// Chain family.
    pub kind: ChainKind,
    /// Fee / compute-unit summary for the block.
    pub fee_summary: serde_json::Value,
}

/// One job-output row: user columns plus lineage columns.
///
/// Every job output carries `_height` (reorg cleanup + incremental windows)
/// and `_rule_version` (lineage).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutRow {
    /// Block height the row was derived from.
    pub height: u64,
    /// Rule version that produced the row.
    pub rule_version: i32,
    /// Commitment at emission time.
    pub commitment: i16,
    /// User-defined payload columns.
    pub values: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample_ev(height: u64, tx: u32, ev: u32) -> Ev {
        Ev {
            height,
            block_ts: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            tx_index: tx,
            ev_index: ev,
            inner_ix: 0,
            stack_height: 0,
            emitter: "emitter".to_owned(),
            topics: Vec::new(),
            payload: Vec::new(),
            tx_hash: Vec::new(),
            sender: String::new(),
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn events_sort_by_height_tx_index_ev_index() {
        let mut events = [sample_ev(10, 1, 0), sample_ev(9, 0, 0), sample_ev(10, 0, 5)];
        events.sort_by_key(Ev::ordering_key);
        let keys: Vec<(u64, u32, u32)> = events
            .iter()
            .map(|e| (e.height, e.tx_index, e.ev_index))
            .collect();
        assert_eq!(keys, vec![(9, 0, 0), (10, 0, 5), (10, 1, 0)]);
    }

    #[test]
    fn skipped_marker_carries_no_rows() {
        let b = DecodedBlock::skipped(42);
        assert!(b.skipped);
        assert!(b.events.is_empty());
        assert!(b.native.is_empty());
    }

    proptest! {
        #[test]
        fn ordering_key_is_monotone_in_ev_index(
            height in 0u64..1_000_000,
            tx in 0u32..10_000,
            a in 0u32..1_000,
            b in 0u32..1_000,
        ) {
            let ea = sample_ev(height, tx, a);
            let eb = sample_ev(height, tx, b);
            prop_assert_eq!(
                ea.ordering_key().cmp(&eb.ordering_key()),
                a.cmp(&b),
            );
        }
    }
}
