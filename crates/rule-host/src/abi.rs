//! Versioned host ABI: one explicit JSON encoding, never Rust types.
//!
//! Guest contract (ABI v1):
//! - exports `memory`, `alloc(u32) -> u32`, `on_window(u32, u32) -> u64`.
//! - `on_window(ptr, len)` reads a [`WindowInput`] JSON document at
//!   `ptr..ptr+len`, writes a JSON `Vec<OutRow>` document via `alloc`, and
//!   returns the output range packed as `(len_hi32 << 32) | ptr_lo32`.
//! - the host rejects guests whose embedded `abi_version` differs from a
//!   supported version, so a failed deployment rolls back by swapping the
//!   module, never by rebuilding rules.

use serde::{Deserialize, Serialize};

/// Current host ABI version.
pub const ABI_VERSION: u32 = 1;

/// Previously supported ABI versions (rollback without rebuilding rules).
pub const SUPPORTED_ABI_VERSIONS: &[u32] = &[1];

/// Rule identity declared by the module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    /// Rule name.
    pub name: String,
    /// Rule version.
    pub version: u32,
    /// ABI version the module was built against.
    pub abi_version: u32,
}

/// One feed slice visible to the rule at a height (e.g. CEX prices).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedSlice {
    /// Feed name.
    pub name: String,
    /// Height the slice is valid at.
    pub at_height: u64,
    /// Feed rows as JSON.
    pub rows: serde_json::Value,
}

/// Window handed to `rule.on_window`: lookback + current + lookahead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInput {
    /// ABI version of this document.
    pub abi_version: u32,
    /// Universal events in `(height, tx_index, ev_index)` order.
    pub events: Vec<chain_core::Ev>,
    /// Block metadata (fees, compute units, tips).
    pub blocks: Vec<chain_core::BlockMeta>,
    /// Visible feed slices.
    pub feeds: Vec<FeedSlice>,
    /// First height of the window (KV reads clamp here).
    pub window_lo: u64,
    /// Past-the-end height of the window.
    pub window_hi: u64,
}

impl WindowInput {
    /// Encode for the guest call.
    pub fn encode(&self) -> Result<Vec<u8>, crate::RuleError> {
        serde_json::to_vec(self).map_err(|e| crate::RuleError::Execution(e.to_string()))
    }

    /// Decode a guest-produced output document.
    pub fn decode_output(bytes: &[u8]) -> Result<Vec<chain_core::OutRow>, crate::RuleError> {
        serde_json::from_slice(bytes).map_err(|e| crate::RuleError::Execution(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_is_pinned_and_supported() {
        assert_eq!(ABI_VERSION, 1);
        assert!(SUPPORTED_ABI_VERSIONS.contains(&ABI_VERSION));
    }

    #[test]
    fn window_round_trips_through_json() {
        let input = WindowInput {
            abi_version: ABI_VERSION,
            events: Vec::new(),
            blocks: Vec::new(),
            feeds: vec![FeedSlice {
                name: "binance_price".to_owned(),
                at_height: 10,
                rows: serde_json::json!({ "SOL": 100.0 }),
            }],
            window_lo: 10,
            window_hi: 11,
        };
        let bytes = input.encode().expect("encode");
        let back: WindowInput = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back, input);
    }

    #[test]
    fn output_decodes_rows() {
        let rows = vec![chain_core::OutRow {
            height: 7,
            rule_version: 1,
            commitment: 1,
            values: serde_json::json!({ "profit": 5 }),
        }];
        let bytes = serde_json::to_vec(&rows).expect("encode");
        let back = WindowInput::decode_output(&bytes).expect("decode");
        assert_eq!(back, rows);
    }
}
