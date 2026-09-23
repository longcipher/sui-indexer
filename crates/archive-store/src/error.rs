//! Archive-tier errors (library layer: `thiserror`).

use thiserror::Error;

/// Errors raised by the archive tier.
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// ClickHouse HTTP failure after retries.
    #[error("clickhouse request failed after {attempts} attempts: {reason}")]
    RequestFailed {
        /// Attempts made.
        attempts: u32,
        /// Last error.
        reason: String,
    },

    /// Archive does not cover the range required for a boundary move.
    #[error("archive gap: [{lo}, {hi}) not covered")]
    CoverageGap {
        /// Range start.
        lo: u64,
        /// Range end.
        hi: u64,
    },

    /// Configuration problem.
    #[error("invalid archive config: {0}")]
    BadConfig(String),

    /// Anything else.
    #[error(transparent)]
    Other(#[from] eyre::Report),
}
