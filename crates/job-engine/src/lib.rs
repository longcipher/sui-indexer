//! Multichain job engine: N user-defined indexing jobs in one process.
//!
//! A job is data, not a process. [`Reconciler`] owns desired state: it diffs
//! the registry against running tasks and starts/stops/updates them with
//! cancellation. Outputs are versioned physical tables with an atomic alias
//! swap; re-scans replay the archive in chunks with persisted cursors.

pub mod catalog;
pub mod ddl;
pub mod error;
pub mod executor;
pub mod metrics;
pub mod outputs;
pub mod queue;
pub mod reconciler;
pub mod spec;

pub use catalog::{CatalogPlan, plan_catalog_apply};
pub use ddl::{DdlStatement, bind_range, build_create_table_as, validate_identifier};
pub use error::JobError;
pub use executor::{ChunkOutcome, SqlExecutor, VersionExecutor};
pub use metrics::{JobMetrics, JobMetricsRegistry, registry_key};
pub use outputs::{alias_swap_ddl, checksum, physical_table};
pub use queue::{ScanChunk, parse_scan_to, plan_chunks};
pub use reconciler::{DesiredState, ReconcileAction, Reconciler, RunningState, RunningVersion};
pub use spec::{ApplyDecision, decide_apply, plan_job};
