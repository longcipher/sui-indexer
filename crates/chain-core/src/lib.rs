//! Chain-neutral primitives for the multichain job engine.
//!
//! This crate owns the four core abstractions from the design:
//! [`ChainKind`], decoded-row contract ([`DecodedBlock`]), the dynamic-catalog
//! vocabulary ([`ReorgMode`]), and the height-cursor / sync / reorg machinery
//! that every chain adapter and every job shares.

pub mod adapter;
pub mod chain;
pub mod cursor;
pub mod error;
pub mod metrics;
pub mod reorg;
pub mod rows;
pub mod sync;
pub mod throttle;

pub use adapter::{ChainAdapter, ChainAdapterFactory, ChainRegistry};
pub use chain::{ChainKind, ChainSchema, ColumnDescriptor, Commitment, CommitmentModel, ParentRef};
pub use cursor::{HeightGap, SyncState, contiguous_prefix, gaps_in};
pub use error::ChainError;
pub use metrics::{MetricLabels, metric_name};
pub use reorg::{ForkPoint, ReorgMode, plan_rollback, verify_continuity};
pub use rows::{BlockMeta, BlockRow, CoreRows, DecodedBlock, Ev, OutRow, RowSet, TxRow};
pub use sync::{HeightRange, SyncEngine, SyncPlannerConfig, SyncTick, split_range};
pub use throttle::ThrottledPool;
