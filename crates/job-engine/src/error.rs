//! Job-engine errors (library layer: `thiserror`).

use thiserror::Error;

/// Errors raised by the job engine.
#[derive(Debug, Error)]
pub enum JobError {
    /// Job spec failed validation.
    #[error("invalid job spec {job}: {reason}")]
    InvalidSpec {
        /// Offending job name.
        job: String,
        /// Why the spec was rejected.
        reason: String,
    },

    /// Illegal lifecycle transition.
    #[error("illegal transition {from} -> {to} for {job} v{version}")]
    IllegalTransition {
        /// Job name.
        job: String,
        /// Version.
        version: u32,
        /// Current status.
        from: String,
        /// Requested status.
        to: String,
    },

    /// SQL identifier failed validation (injection guard).
    #[error("invalid SQL identifier: {0}")]
    BadIdentifier(String),

    /// User SQL failed the read-only check.
    #[error("job SQL must be a read-only SELECT: {0}")]
    BadSql(String),

    /// Quota exceeded; the version is quarantined.
    #[error("job {job} v{version} exceeded quota: {reason}")]
    QuotaExceeded {
        /// Job name.
        job: String,
        /// Version.
        version: u32,
        /// Which quota tripped.
        reason: String,
    },

    /// Storage failure.
    #[error(transparent)]
    Storage(#[from] eyre::Report),
}
