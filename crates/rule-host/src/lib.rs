//! WASM rule host: stateful/complex detections as hot-loadable modules.
//!
//! The ABI is narrow and versioned ([`ABI_VERSION`]). The host speaks one
//! encoding — JSON over a single linear-memory call — never Rust types.
//! Rules that only need ordering, amounts and fees stay stateless; true pool
//! state lives in the height-scoped [`Kv`] store with range-delete rollback.

pub mod abi;
pub mod error;
pub mod host;
pub mod kv;
pub mod pgkv;

pub use abi::{ABI_VERSION, FeedSlice, RuleSpec, WindowInput};
pub use error::RuleError;
pub use host::{NativeRule, Rule, RuleHost};
pub use kv::{Kv, MemoryKv};
pub use pgkv::PgKv;
