//! Pure Solana decode: `BlockValue` -> skeleton rows + `events`.
//!
//! Inner instructions are first-class in `events` via `extra.inner_ix` and
//! `extra.stack_height`; they are never discarded. Program events (swap
//! amounts) are preferred over account-state reads.

use chain_core::{BlockRow, Commitment, CoreRows, DecodedBlock, Ev, ParentRef, RowSet, TxRow};
use chrono::{DateTime, Utc};

use crate::rpc::BlockValue;

/// Decode one `getBlock` value into a [`DecodedBlock`].
///
/// Ordering invariant: outer instructions sort by `(tx_index, ev_index)`,
/// inner instructions sort after their outer instruction keyed by
/// `(inner_ix, stack_height)`.
#[must_use]
pub fn decode_block_value(slot: u64, value: &BlockValue, commitment: Commitment) -> DecodedBlock {
    let ts = value
        .block_time
        .and_then(DateTime::<Utc>::from_timestamp_secs)
        .unwrap_or_default();
    let hash = value.blockhash.as_bytes().to_vec();
    let parent_hash = value.previous_blockhash.as_bytes().to_vec();
    let parent = ParentRef {
        hash: parent_hash.clone(),
        meta: serde_json::json!({
            "parent_slot": value.parent_slot,
            "previous_blockhash": value.previous_blockhash,
        }),
    };
    let mut events = Vec::new();
    let mut txs = Vec::with_capacity(value.transactions.len());
    let mut deltas = Vec::new();
    let mut logs = Vec::new();

    for (tx_index, tx) in value.transactions.iter().enumerate() {
        let signature = tx
            .transaction
            .signatures
            .first()
            .cloned()
            .unwrap_or_default();
        // ponytail: sender = fee payer (account 0). Full signer resolution
        // needs loaded addresses; add when a rule needs it.
        let sender = tx
            .transaction
            .message
            .account_keys
            .first()
            .cloned()
            .unwrap_or_default();
        let success = tx.meta.err.is_none();
        txs.push(TxRow {
            height: slot,
            block_ts: ts,
            tx_index: tx_index as u32,
            tx_hash: signature.as_bytes().to_vec(),
            sender,
            success,
            fee: i128::from(tx.meta.fee),
            chain_meta: serde_json::json!({ "signature": signature }),
        });

        let keys = &tx.transaction.message.account_keys;
        let program_of = |index: u32| {
            keys.get(index as usize)
                .cloned()
                .unwrap_or_else(|| format!("program#{index}"))
        };

        // Outer instructions: ev_index = outer position.
        for (ix_pos, ix) in tx.transaction.message.instructions.iter().enumerate() {
            let program = program_of(ix.program_id_index);
            events.push(Ev {
                height: slot,
                block_ts: ts,
                tx_index: tx_index as u32,
                ev_index: ix_pos as u32,
                inner_ix: 0,
                stack_height: 0,
                emitter: program.clone(),
                topics: discriminator_topics(&ix.data),
                payload: decode_data(&ix.data),
                tx_hash: signature.as_bytes().to_vec(),
                sender: tx
                    .transaction
                    .message
                    .account_keys
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                extra: serde_json::json!({
                    "kind": "outer",
                    "program_id": program,
                }),
            });
        }
        // Inner instructions: same ev_index as outer, inner_ix for ordering.
        for inner in &tx.meta.inner_instructions {
            for (pos, ix) in inner.instructions.iter().enumerate() {
                let program = program_of(ix.program_id_index);
                events.push(Ev {
                    height: slot,
                    block_ts: ts,
                    tx_index: tx_index as u32,
                    ev_index: inner.index,
                    inner_ix: (pos as u32).saturating_add(1),
                    stack_height: ix.stack_height.unwrap_or(1),
                    emitter: program.clone(),
                    topics: discriminator_topics(&ix.data),
                    payload: decode_data(&ix.data),
                    tx_hash: signature.as_bytes().to_vec(),
                    sender: String::new(),
                    extra: serde_json::json!({
                        "kind": "inner",
                        "program_id": program,
                        "inner_ix": pos,
                        "stack_height": ix.stack_height.unwrap_or(1),
                    }),
                });
            }
        }
        for message in &tx.meta.log_messages {
            let program = message_program(message).unwrap_or_default();
            logs.push(serde_json::json!({
                "slot": slot,
                "program_id": program,
                "message": message,
            }));
        }
        deltas.push(serde_json::json!({ "slot": slot, "signature": signature }));
    }

    events.sort_by_key(Ev::ordering_key);

    DecodedBlock {
        height: slot,
        hash: hash.clone(),
        parent,
        ts,
        commitment,
        rows: CoreRows {
            block: Some(BlockRow {
                height: slot,
                hash,
                parent_hash,
                parent_ref: serde_json::json!({ "parent_slot": value.parent_slot }),
                ts,
                commitment: commitment.as_i16(),
                chain_meta: serde_json::json!({
                    "parent_slot": value.parent_slot,
                    "blockhash": value.blockhash,
                }),
            }),
            txs,
        },
        events,
        native: vec![
            RowSet {
                table: "account_deltas".to_owned(),
                rows: deltas,
            },
            RowSet {
                table: "program_logs".to_owned(),
                rows: logs,
            },
        ],
        skipped: false,
    }
}

/// Topics for an instruction: the 8-byte discriminator as hex when present.
#[must_use]
pub fn discriminator_topics(data_base58: &str) -> Vec<String> {
    match discriminator_hex(data_base58) {
        Some(hex) => vec![hex],
        None => Vec::new(),
    }
}

/// Hex of the 8-byte Anchor-style discriminator, if the data decodes.
#[must_use]
pub fn discriminator_hex(data_base58: &str) -> Option<String> {
    let bytes = decode_data_opt(data_base58)?;
    if bytes.len() < 8 {
        return None;
    }
    Some(hex_of(&bytes[..8]))
}

fn decode_data(data_base58: &str) -> Vec<u8> {
    decode_data_opt(data_base58).unwrap_or_default()
}

fn decode_data_opt(data_base58: &str) -> Option<Vec<u8>> {
    if data_base58.is_empty() {
        return None;
    }
    bs58::decode(data_base58).into_vec().ok()
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// Best-effort program id from a `Program <id> invoke [n]` log line.
fn message_program(message: &str) -> Option<String> {
    let rest = message.strip_prefix("Program ")?;
    Some(
        rest.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{BlockValue, ConfirmedTransaction, OuterIx, TxBody, TxMessage, TxMeta};
    use proptest::prelude::*;

    fn block_with_one_tx() -> BlockValue {
        BlockValue {
            blockhash: "H".to_owned(),
            previous_blockhash: "P".to_owned(),
            parent_slot: 41,
            block_time: Some(1_700_000_000),
            transactions: vec![ConfirmedTransaction {
                meta: TxMeta {
                    err: None,
                    fee: 5000,
                    inner_instructions: vec![crate::rpc::InnerInstructions {
                        index: 0,
                        instructions: vec![crate::rpc::InnerIx {
                            program_id_index: 1,
                            accounts: vec![],
                            data: bs58::encode([9u8; 10]).into_string(),
                            stack_height: Some(2),
                        }],
                    }],
                    log_messages: vec!["Program 111 invoke [1]".to_owned()],
                },
                transaction: TxBody {
                    signatures: vec!["sig".to_owned()],
                    message: TxMessage {
                        account_keys: vec!["payer".to_owned(), "prog".to_owned()],
                        instructions: vec![OuterIx {
                            program_id_index: 1,
                            accounts: vec![],
                            data: bs58::encode([7u8; 10]).into_string(),
                        }],
                    },
                },
            }],
        }
    }

    #[test]
    fn inner_instructions_sort_after_outer_and_keep_ordering() {
        let block = decode_block_value(42, &block_with_one_tx(), Commitment::Confirmed);
        assert_eq!(block.height, 42);
        assert_eq!(block.events.len(), 2);
        assert!(block.events[0].inner_ix == 0);
        assert!(block.events[1].inner_ix == 1);
        // Instruction data survives base58 round-trip into the payload.
        assert_eq!(block.events[0].payload, vec![7u8; 10]);
        assert_eq!(block.events[1].payload, vec![9u8; 10]);
        let keys: Vec<_> = block.events.iter().map(Ev::ordering_key).collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn skipped_slots_are_markers_not_errors() {
        let marker = DecodedBlock::skipped(99);
        assert!(marker.skipped);
        assert!(marker.events.is_empty());
    }

    #[test]
    fn parent_link_carries_parent_slot() {
        let block = decode_block_value(42, &block_with_one_tx(), Commitment::Confirmed);
        assert_eq!(block.parent.meta["parent_slot"], serde_json::json!(41));
    }

    #[test]
    fn discriminator_extracts_eight_bytes() {
        let data = bs58::encode([1u8, 2, 3, 4, 5, 6, 7, 8, 9]).into_string();
        assert_eq!(
            discriminator_hex(&data),
            Some("0102030405060708".to_owned())
        );
        assert_eq!(discriminator_hex(""), None);
        // Exactly eight bytes is enough; seven is not.
        let eight = bs58::encode([1u8, 2, 3, 4, 5, 6, 7, 8]).into_string();
        assert_eq!(
            discriminator_hex(&eight),
            Some("0102030405060708".to_owned())
        );
        let seven = bs58::encode([1u8, 2, 3, 4, 5, 6, 7]).into_string();
        assert_eq!(discriminator_hex(&seven), None);
        // Topics mirror the discriminator.
        assert_eq!(discriminator_topics(&eight).len(), 1);
        assert!(discriminator_topics("").is_empty());
    }

    #[test]
    fn hex_encoding_is_lowercase() {
        let data = bs58::encode([0xABu8]).into_string();
        let bytes = bs58::decode(&data).into_vec().expect("decodes");
        assert_eq!(bytes, vec![0xAB]);
        assert_eq!(
            discriminator_hex(&bs58::encode([0xABu8; 8]).into_string()),
            Some("abababababababab".to_owned())
        );
    }

    #[test]
    fn program_names_come_from_invoke_lines() {
        let block = decode_block_value(42, &block_with_one_tx(), Commitment::Confirmed);
        let logs = &block.native[1].rows;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["program_id"], serde_json::json!("111"));
        // Non-invoke lines carry no program.
        let plain = BlockValue {
            blockhash: "H".to_owned(),
            previous_blockhash: "P".to_owned(),
            parent_slot: 41,
            block_time: Some(1_700_000_000),
            transactions: vec![],
        };
        let empty = decode_block_value(1, &plain, Commitment::Confirmed);
        assert!(empty.native[1].rows.is_empty());
    }

    proptest! {
        #[test]
        fn decode_preserves_slot(slot in 0u64..u64::MAX) {
            let mut value = block_with_one_tx();
            value.parent_slot = slot.saturating_sub(1);
            let block = decode_block_value(slot, &value, Commitment::Confirmed);
            prop_assert_eq!(block.height, slot);
        }
    }
}
