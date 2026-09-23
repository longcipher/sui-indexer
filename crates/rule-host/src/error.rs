//! Rule-host errors (library layer: `thiserror`).

use thiserror::Error;

/// Errors raised by the rule host.
#[derive(Debug, Error)]
pub enum RuleError {
    /// Guest module failed to load or link.
    #[error("rule module failed to load: {0}")]
    Load(String),

    /// ABI version mismatch.
    #[error("unsupported abi version {got}, host supports {supported:?}")]
    AbiMismatch {
        /// Version the guest asked for.
        got: u32,
        /// Versions the host supports.
        supported: Vec<u32>,
    },

    /// Guest trapped or violated its budget.
    #[error("rule execution failed: {0}")]
    Execution(String),

    /// Fuel exhausted: the rule is quarantined, never allowed to starve sync.
    #[error("rule exhausted fuel budget")]
    OutOfFuel,

    /// KV failure.
    #[error("kv store failed: {0}")]
    Kv(String),

    /// Anything else.
    #[error(transparent)]
    Other(#[from] eyre::Report),
}
