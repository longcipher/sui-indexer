//! Chain identity, commitment models and schema descriptors.

use serde::{Deserialize, Serialize};

/// Which chain family an adapter serves.
///
/// The sync engine, job engine and query layer only ever match on this
/// discriminator; chain-native types never cross the adapter boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChainKind {
    /// Ethereum-compatible chains (block number cursor).
    Evm,
    /// Solana (`svm`, slot cursor).
    Svm,
    /// Sui / Move (`move`, checkpoint-sequence cursor).
    #[default]
    Move,
}

impl ChainKind {
    /// Parse a user-supplied chain-kind string.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "evm" | "ethereum" | "tempo" => Some(Self::Evm),
            "svm" | "solana" => Some(Self::Svm),
            "move" | "sui" | "aptos" => Some(Self::Move),
            _ => None,
        }
    }

    /// Canonical config key used in metrics, tables and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evm => "evm",
            Self::Svm => "svm",
            Self::Move => "move",
        }
    }
}

impl core::fmt::Display for ChainKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::str::FromStr for ChainKind {
    type Err = crate::ChainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| crate::ChainError::UnknownChainKind(s.to_owned()))
    }
}

/// Per-row finality annotation stored on `blocks.commitment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    /// Seen but reversible.
    Pending,
    /// Sequencer / `confirmed` level. Solana detections run here.
    #[default]
    Confirmed,
    /// Irreversible. Sui checkpoints are always final.
    Final,
}

impl Commitment {
    /// Integer encoding used by the `blocks.commitment` column.
    #[must_use]
    pub fn as_i16(self) -> i16 {
        match self {
            Self::Pending => 0,
            Self::Confirmed => 1,
            Self::Final => 2,
        }
    }

    /// Decode the `blocks.commitment` column.
    #[must_use]
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Confirmed),
            2 => Some(Self::Final),
            _ => None,
        }
    }
}

/// How an adapter's heights behave under forks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CommitmentModel {
    /// Heights can be rolled back up to `max_depth`; deeper forks rewrite the tip.
    Reorgable {
        /// Maximum fork depth the engine searches before giving up.
        max_depth: u64,
    },
    /// Heights are final; the rollback path is a no-op.
    #[default]
    Final,
}

/// Chain-specific parent identity used for fork detection.
///
/// EVM uses the parent hash, Solana `parent_slot + previous_blockhash`,
/// Sui `prev_digest`. The engine compares these opaquely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ParentRef {
    /// Raw parent hash / digest bytes (empty when the chain has no hash link).
    pub hash: Vec<u8>,
    /// Extra parent identity (e.g. Solana `parent_slot` encoded as JSON).
    pub meta: serde_json::Value,
}

impl ParentRef {
    /// Build a hash-only parent reference.
    #[must_use]
    pub fn from_hash(hash: Vec<u8>) -> Self {
        Self {
            hash,
            meta: serde_json::Value::Null,
        }
    }
}

/// A single native-table column descriptor contributed by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDescriptor {
    /// Column name.
    pub name: String,
    /// SQL type fragment used when generating DDL.
    pub sql_type: String,
    /// Whether the column may be NULL.
    pub nullable: bool,
}

/// Native tables plus column descriptors contributed by one adapter.
///
/// Skeleton tables (`blocks`, `txs`, `events`) are fixed; everything
/// chain-specific lives here so the engine never hard-codes a chain column.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainSchema {
    /// Chain family this schema describes.
    pub kind: ChainKind,
    /// `(table, columns)` native tables kept for chain fidelity.
    pub native_tables: Vec<(String, Vec<ColumnDescriptor>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn chain_kind_round_trips_through_display_and_parse() {
        for kind in [ChainKind::Evm, ChainKind::Svm, ChainKind::Move] {
            assert_eq!(ChainKind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }

    #[test]
    fn chain_kind_accepts_aliases_and_rejects_unknown() {
        assert_eq!(ChainKind::parse("solana"), Some(ChainKind::Svm));
        assert_eq!(ChainKind::parse("SUI"), Some(ChainKind::Move));
        assert_eq!(ChainKind::parse("tempo"), Some(ChainKind::Evm));
        assert_eq!(ChainKind::parse("cosmos"), None);
    }

    #[test]
    fn chain_kind_from_str_matches_parse() {
        assert_eq!("svm".parse::<ChainKind>().expect("svm"), ChainKind::Svm);
        assert_eq!("move".parse::<ChainKind>().expect("move"), ChainKind::Move);
        assert!("cosmos".parse::<ChainKind>().is_err());
    }

    #[test]
    fn commitment_integer_encoding_is_stable() {
        assert_eq!(
            [
                Commitment::Pending,
                Commitment::Confirmed,
                Commitment::Final
            ]
            .iter()
            .map(|c| c.as_i16())
            .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(Commitment::from_i16(2), Some(Commitment::Final));
        assert_eq!(Commitment::from_i16(9), None);
    }

    proptest! {
        #[test]
        fn commitment_round_trips(v in 0i16..3) {
            let c = Commitment::from_i16(v).expect("valid commitment");
            prop_assert_eq!(c.as_i16(), v);
        }
    }
}
