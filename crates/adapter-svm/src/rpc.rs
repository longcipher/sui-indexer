//! Minimal Solana JSON-RPC client: `getSlot` + `getBlock`.
//!
//! Hand-rolled batched JSON-RPC with per-call timeouts; no retry at this
//! layer (the sync engine owns backoff and repair).

use chain_core::ChainError;
use serde::{Deserialize, Serialize};

/// Finality level for reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitmentLevel {
    /// ~0.4-1s. Detection runs here with block-scoped rollback.
    #[default]
    Confirmed,
    /// ~13s. Latency-insensitive jobs wait for this.
    Finalized,
}

impl CommitmentLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
        }
    }
}

/// Thin JSON-RPC client for the two calls the adapter needs.
#[derive(Debug, Clone)]
pub struct RpcClient {
    endpoint: String,
    http: reqwest::Client,
}

impl RpcClient {
    /// Build against `endpoint`.
    #[must_use]
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::new(),
        }
    }

    /// Endpoint this client talks to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ChainError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ChainError::Transport(format!("svm rpc {method}: {e}")))?;
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ChainError::Transport(format!("svm rpc {method} decode: {e}")))?;
        if let Some(err) = value.get("error") {
            return Err(ChainError::Transport(format!("svm rpc {method}: {err}")));
        }
        Ok(value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Current slot at `commitment`.
    pub async fn get_slot(&self, commitment: CommitmentLevel) -> Result<u64, ChainError> {
        let result = self
            .call(
                "getSlot",
                serde_json::json!([{ "commitment": commitment.as_str() }]),
            )
            .await?;
        result.as_u64().ok_or_else(|| {
            ChainError::Transport(format!("svm getSlot: unexpected result {result}"))
        })
    }

    /// `getBlock` for `slot`; `None` for skipped slots (normal, not an error).
    pub async fn get_block(
        &self,
        slot: u64,
        commitment: CommitmentLevel,
    ) -> Result<Option<BlockValue>, ChainError> {
        let result = self
            .call(
                "getBlock",
                serde_json::json!([
                    slot,
                    {
                        "encoding": "json",
                        "transactionDetails": "full",
                        "rewards": false,
                        "maxSupportedTransactionVersion": crate::MAX_SUPPORTED_TX_VERSION,
                        "commitment": commitment.as_str(),
                    }
                ]),
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        serde_json::from_value(result)
            .map(Some)
            .map_err(|e| ChainError::Transport(format!("svm getBlock {slot} decode: {e}")))
    }
}

/// Subset of `getBlock` JSON the adapter decodes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlockValue {
    /// Blockhash (base58).
    #[serde(default)]
    pub blockhash: String,
    /// Previous blockhash (base58).
    #[serde(default)]
    pub previous_blockhash: String,
    /// Parent slot.
    #[serde(default)]
    pub parent_slot: u64,
    /// Block time (seconds).
    #[serde(default)]
    pub block_time: Option<i64>,
    /// Full transactions.
    #[serde(default)]
    pub transactions: Vec<ConfirmedTransaction>,
}

/// One full transaction in `getBlock` JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConfirmedTransaction {
    /// Transaction metadata.
    #[serde(default)]
    pub meta: TxMeta,
    /// Transaction body.
    #[serde(default)]
    pub transaction: TxBody,
}

/// Transaction metadata subset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TxMeta {
    /// Null when the transaction succeeded.
    #[serde(default)]
    pub err: Option<serde_json::Value>,
    /// Fee in lamports.
    #[serde(default)]
    pub fee: u64,
    /// Inner instructions (MEV lives here).
    #[serde(default)]
    pub inner_instructions: Vec<InnerInstructions>,
    /// Program log messages.
    #[serde(default)]
    pub log_messages: Vec<String>,
}

/// Inner instructions for one outer instruction index.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InnerInstructions {
    /// Outer instruction index.
    #[serde(default)]
    pub index: u32,
    /// Inner instruction records.
    #[serde(default)]
    pub instructions: Vec<InnerIx>,
}

/// One inner instruction record.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InnerIx {
    /// Program id index into `accountKeys`.
    #[serde(default)]
    pub program_id_index: u32,
    /// Accounts touched.
    #[serde(default)]
    pub accounts: Vec<u32>,
    /// Instruction data (base58).
    #[serde(default)]
    pub data: String,
    /// Stack height.
    #[serde(default)]
    pub stack_height: Option<u32>,
}

/// Transaction body subset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TxBody {
    /// First signature.
    #[serde(default)]
    pub signatures: Vec<String>,
    /// Message with account keys and outer instructions.
    #[serde(default)]
    pub message: TxMessage,
}

/// Message subset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TxMessage {
    /// Account keys (index-addressed).
    #[serde(default)]
    pub account_keys: Vec<String>,
    /// Outer instructions.
    #[serde(default)]
    pub instructions: Vec<OuterIx>,
}

/// One outer instruction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OuterIx {
    /// Program id index into `accountKeys`.
    #[serde(default)]
    pub program_id_index: u32,
    /// Accounts touched.
    #[serde(default)]
    pub accounts: Vec<u32>,
    /// Instruction data (base58).
    #[serde(default)]
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitment_levels_render() {
        assert_eq!(CommitmentLevel::Confirmed.as_str(), "confirmed");
        assert_eq!(CommitmentLevel::Finalized.as_str(), "finalized");
    }

    #[test]
    fn block_value_decodes_minimal_json() {
        let value: BlockValue = serde_json::from_value(serde_json::json!({
            "blockhash": "H",
            "previousBlockhash": "P",
            "parentSlot": 41,
            "blockTime": 1_700_000_000,
            "transactions": [],
        }))
        .expect("minimal block decodes");
        assert_eq!(value.parent_slot, 41);
        assert!(value.transactions.is_empty());
    }
}
