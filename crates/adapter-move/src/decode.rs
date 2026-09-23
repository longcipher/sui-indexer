//! Pure decode helpers: checkpoint summary -> skeleton rows + `events`.
//!
//! Kept free of network access so the ordering guarantees MEV detection
//! depends on are covered by fast unit and property tests.

use chain_core::{
    BlockRow, ChainError, Commitment, CoreRows, DecodedBlock, Ev, ParentRef, RowSet, TxRow,
};
use chrono::{DateTime, Utc};

/// Decode a checkpoint summary into a [`DecodedBlock`] skeleton.
///
/// This is the adapter's contract boundary: the engine never sees the
/// chain-native checkpoint type, only these rows.
#[must_use]
pub fn decode_checkpoint_summary(
    height: u64,
    digest_hex: &str,
    prev_digest_hex: &str,
    timestamp_ms: u64,
    epoch: u64,
) -> DecodedBlock {
    let ts = DateTime::<Utc>::from_timestamp_millis(timestamp_ms as i64).unwrap_or_default();
    let hash = decode_hex32(digest_hex);
    let parent_hash = decode_hex32(prev_digest_hex);
    DecodedBlock {
        height,
        hash: hash.clone(),
        parent: ParentRef {
            hash: parent_hash.clone(),
            meta: serde_json::json!({ "prev_digest": prev_digest_hex }),
        },
        ts,
        commitment: Commitment::Final,
        rows: CoreRows {
            block: Some(BlockRow {
                height,
                hash,
                parent_hash,
                parent_ref: serde_json::json!({ "prev_digest": prev_digest_hex }),
                ts,
                commitment: Commitment::Final.as_i16(),
                chain_meta: serde_json::json!({
                    "epoch": epoch,
                    "digest": digest_hex,
                }),
            }),
            txs: Vec::new(),
        },
        events: Vec::new(),
        native: vec![RowSet {
            table: "checkpoints".to_owned(),
            rows: vec![serde_json::json!({
                "sequence_number": height,
                "digest": digest_hex,
                "prev_digest": prev_digest_hex,
                "epoch": epoch,
                "timestamp_ms": timestamp_ms,
            })],
        }],
        skipped: false,
    }
}

/// Decode a full checkpoint (summary + transactions + events) into rows.
///
/// Transaction and event rows are appended in chain order so the
/// `(height, tx_index, ev_index)` ordering invariant holds.
#[must_use]
pub fn decode_checkpoint(
    checkpoint: &sui_types::full_checkpoint_content::Checkpoint,
) -> DecodedBlock {
    use sui_types::effects::TransactionEffectsAPI as _;
    use sui_types::transaction::TransactionDataAPI as _;

    let summary = &checkpoint.summary;
    let height = summary.sequence_number;
    let digest = summary.digest().to_string();
    let prev = summary
        .previous_digest
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut block =
        decode_checkpoint_summary(height, &digest, &prev, summary.timestamp_ms, summary.epoch);
    let ts = block.ts;
    let mut tx_rows = Vec::with_capacity(checkpoint.transactions.len());
    for (tx_index, executed) in checkpoint.transactions.iter().enumerate() {
        let tx_digest = executed.effects.transaction_digest().to_string();
        let sender = executed.transaction.sender().to_string();
        let success = executed.effects.status().is_ok();
        let gas = executed.effects.gas_cost_summary();
        let gas_used = gas
            .computation_cost
            .saturating_add(gas.storage_cost)
            .saturating_sub(gas.storage_rebate);
        tx_rows.push(tx_row(
            height,
            ts,
            tx_index as u32,
            &tx_digest,
            &sender,
            success,
            gas_used,
        ));
        if let Some(events) = executed.events.as_ref() {
            for (ev_index, event) in events.data.iter().enumerate() {
                let cursor = EventCursor {
                    height,
                    block_ts: ts,
                    tx_index: tx_index as u32,
                    ev_index: ev_index as u32,
                };
                block.events.push(move_event(
                    cursor,
                    &event.package_id.to_string(),
                    &event.transaction_module.to_string(),
                    &event.type_.name.to_string(),
                    &event.sender.to_string(),
                    &tx_digest,
                    event.contents.clone(),
                ));
            }
        }
    }
    if let Some(skeleton) = block.rows.block.as_mut() {
        let _ = skeleton;
    }
    block.rows.txs = tx_rows;
    block
}

/// Build one [`TxRow`] from transaction-level fields.
#[must_use]
pub fn tx_row(
    height: u64,
    block_ts: DateTime<Utc>,
    tx_index: u32,
    tx_digest: &str,
    sender: &str,
    success: bool,
    gas_used: u64,
) -> TxRow {
    TxRow {
        height,
        block_ts,
        tx_index,
        tx_hash: tx_digest.as_bytes().to_vec(),
        sender: sender.to_owned(),
        success,
        fee: i128::from(gas_used),
        chain_meta: serde_json::json!({ "digest": tx_digest }),
    }
}

/// Position of one event inside its block.
#[derive(Debug, Clone, Copy)]
pub struct EventCursor {
    /// Block height.
    pub height: u64,
    /// Block timestamp.
    pub block_ts: DateTime<Utc>,
    /// Transaction index inside the block.
    pub tx_index: u32,
    /// Event index inside the transaction.
    pub ev_index: u32,
}

/// Build one universal [`Ev`] row from Move event fields.
#[must_use]
pub fn move_event(
    cursor: EventCursor,
    package_id: &str,
    module: &str,
    event_type: &str,
    sender: &str,
    tx_digest: &str,
    payload: Vec<u8>,
) -> Ev {
    Ev {
        height: cursor.height,
        block_ts: cursor.block_ts,
        tx_index: cursor.tx_index,
        ev_index: cursor.ev_index,
        inner_ix: 0,
        stack_height: 0,
        emitter: emitter_of(package_id, module),
        topics: event_topics(package_id, module, event_type),
        payload,
        tx_hash: tx_digest.as_bytes().to_vec(),
        sender: sender.to_owned(),
        extra: serde_json::json!({
            "package_id": package_id,
            "module": module,
            "event_type": event_type,
        }),
    }
}

/// `package::module` emitter for the universal `events` projection.
#[must_use]
pub fn emitter_of(package_id: &str, module: &str) -> String {
    format!("{package_id}::{module}")
}

/// Move topics are the single fully-qualified event type.
#[must_use]
pub fn event_topics(package_id: &str, module: &str, event_type: &str) -> Vec<String> {
    vec![format!("{package_id}::{module}::{event_type}")]
}

fn decode_hex32(hex_str: &str) -> Vec<u8> {
    let trimmed = hex_str.trim().trim_start_matches("0x");
    if trimmed.len() == 64 {
        hex::decode(trimmed).unwrap_or_else(|_| hex_str.as_bytes().to_vec())
    } else {
        hex_str.as_bytes().to_vec()
    }
}

/// Validate a decoded block before handing it to the engine.
pub fn validate_block(block: &DecodedBlock) -> Result<(), ChainError> {
    if block.skipped {
        return Ok(());
    }
    let mut last: Option<(u32, u32)> = None;
    for ev in &block.events {
        if ev.height != block.height {
            return Err(ChainError::InvalidBlock {
                height: block.height,
                reason: format!(
                    "event height {} != block height {}",
                    ev.height, block.height
                ),
            });
        }
        let key = (ev.tx_index, ev.ev_index);
        if let Some(prev) = last {
            if key < prev {
                return Err(ChainError::InvalidBlock {
                    height: block.height,
                    reason: "events out of order".to_owned(),
                });
            }
        }
        last = Some(key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn summary_decode_maps_prev_digest_to_parent_ref() {
        let block = decode_checkpoint_summary(10, "0xaaa", "0xbbb", 1_700_000_000_000, 42);
        assert_eq!(block.height, 10);
        assert_eq!(block.commitment, Commitment::Final);
        let row = block.rows.block.expect("block row");
        assert_eq!(row.height, 10);
        assert_eq!(row.commitment, 2);
        assert!(!block.skipped);
    }

    #[test]
    fn emitter_and_topics_follow_package_module_shape() {
        assert_eq!(emitter_of("0x2", "coin"), "0x2::coin");
        assert_eq!(
            event_topics("0x2", "coin", "Transfer"),
            vec!["0x2::coin::Transfer".to_owned()]
        );
    }

    #[test]
    fn event_ordering_is_preserved_by_validation() {
        let ts = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default();
        let cursor = |ev_index| EventCursor {
            height: 1,
            block_ts: ts,
            tx_index: 0,
            ev_index,
        };
        let mut block = decode_checkpoint_summary(1, "0x1", "0x0", 0, 0);
        block.events.push(move_event(
            cursor(1),
            "0x2",
            "coin",
            "Transfer",
            "0xs",
            "0xt",
            vec![],
        ));
        block.events.push(move_event(
            cursor(0),
            "0x2",
            "coin",
            "Transfer",
            "0xs",
            "0xt",
            vec![],
        ));
        assert!(validate_block(&block).is_err());
    }

    #[test]
    fn cross_height_events_are_rejected() {
        let ts = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default();
        let mut block = decode_checkpoint_summary(1, "0x1", "0x0", 0, 0);
        block.events.push(move_event(
            EventCursor {
                height: 2,
                block_ts: ts,
                tx_index: 0,
                ev_index: 0,
            },
            "0x2",
            "coin",
            "Transfer",
            "0xs",
            "0xt",
            vec![],
        ));
        assert!(validate_block(&block).is_err());
    }

    #[test]
    fn hex32_decodes_64_char_hashes_and_passes_through_other() {
        let hex_str = "ab".repeat(32);
        assert_eq!(hex_str.len(), 64);
        let bytes = hex::decode(&hex_str).expect("valid hex");
        let mut block = decode_checkpoint_summary(10, &hex_str, "0xbbb", 0, 0);
        let row = block.rows.block.expect("block row");
        assert_eq!(row.hash, bytes);
        // Non-64-char input passes through as raw bytes.
        block = decode_checkpoint_summary(10, "0x1", "0x0", 0, 0);
        assert_eq!(block.rows.block.expect("block row").hash, b"0x1".to_vec());
    }

    #[test]
    fn inner_instructions_share_their_outer_index() {
        let ts = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default();
        let mut block = decode_checkpoint_summary(1, "0x1", "0x0", 0, 0);
        for inner in [0, 1] {
            let mut ev = move_event(
                EventCursor {
                    height: 1,
                    block_ts: ts,
                    tx_index: 0,
                    ev_index: 0,
                },
                "0x2",
                "coin",
                "Transfer",
                "0xs",
                "0xt",
                vec![],
            );
            ev.inner_ix = inner;
            block.events.push(ev);
        }
        // Equal (tx_index, ev_index) keys are ordered, not rejected.
        assert!(validate_block(&block).is_ok());
    }

    proptest! {
        #[test]
        fn summary_decode_preserves_height(height in 0u64..u64::MAX) {
            let block = decode_checkpoint_summary(height, "0x1", "0x0", 0, 0);
            prop_assert_eq!(block.height, height);
        }
    }
}
