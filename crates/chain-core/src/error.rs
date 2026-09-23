//! Library-level errors.

use thiserror::Error;

/// Errors raised by chain adapters and the height-cursor machinery.
#[derive(Debug, Error)]
pub enum ChainError {
    /// Unknown `[chain] kind` value.
    #[error("unknown chain kind: {0}")]
    UnknownChainKind(String),

    /// No adapter factory registered for the requested kind.
    #[error("no adapter registered for chain kind: {0}")]
    NoAdapter(String),

    /// Range is invalid (`start >= end` where a non-empty range is required).
    #[error("invalid height range [{0}, {1})")]
    InvalidRange(u64, u64),

    /// Upstream RPC / transport failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// A fetched row failed validation (ordering, parent link, …).
    #[error("invalid block at height {height}: {reason}")]
    InvalidBlock {
        /// Offending height.
        height: u64,
        /// Why the block was rejected.
        reason: String,
    },

    /// Historical data is unavailable from this endpoint.
    #[error("history unavailable at height {0}: {1}")]
    HistoryUnavailable(u64, String),

    /// Anything else, with the source preserved as a string.
    #[error(transparent)]
    Other(#[from] eyre::Report),
}
