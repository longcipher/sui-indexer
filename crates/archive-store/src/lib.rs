//! Tiered archive: ClickHouse is the archive of record, PostgreSQL keeps
//! the hot window and the control plane.
//!
//! Path-A re-scans are only cheap inside a columnar store. The boundary
//! (`pruned_below`) moves only when the archive provably covers the range,
//! then the tiered views are re-baked atomically.

pub mod client;
pub mod ddl;
pub mod error;
pub mod scan;
pub mod sink;
pub mod tier;

pub use client::ClickHouseClient;
pub use ddl::{
    archive_table_ddl, job_output_ddl, job_table_as_select_ddl, live_view_ddl, ranged_backfill_sql,
    tiered_view_ddl,
};
pub use error::ArchiveError;
pub use scan::ClickHouseExecutor;
pub use sink::{BatchSink, InsertBatch};
pub use tier::{BoundaryCheck, Tier, TierRouter};
