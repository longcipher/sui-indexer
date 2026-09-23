//! Rule execution: fuel-metered WASM guests plus in-process native rules.
//!
//! The driver reads an archive window, calls the rule, writes the returned
//! rows. Every scan loop checks cancellation between windows; quotas are
//! enforced by fuel, the memory cap and `max_rows_per_window`.

use std::sync::Arc;

use async_trait::async_trait;
use chain_core::OutRow;
use tracing::{debug, warn};

use crate::{RuleError, RuleSpec, WindowInput};

/// A rule: stateless or KV-backed detection over one height window.
#[async_trait]
pub trait Rule: Send + Sync {
    /// Rule identity (name, version, ABI).
    fn spec(&self) -> RuleSpec;

    /// Run detection over `input`; return output rows.
    async fn on_window(&self, input: &WindowInput) -> Result<Vec<OutRow>, RuleError>;
}

/// In-process native rule (built-in detections, tests, offline runs).
///
/// Hosts the same [`WindowInput`] → `Vec<OutRow>` contract without a guest
/// module; the scheduler treats native and WASM rules identically.
pub struct NativeRule<F> {
    spec: RuleSpec,
    func: F,
}

impl<F> NativeRule<F>
where
    F: Fn(&WindowInput) -> Result<Vec<OutRow>, RuleError> + Send + Sync,
{
    /// Wrap a closure as a rule.
    #[must_use]
    pub fn new(spec: RuleSpec, func: F) -> Self {
        Self { spec, func }
    }
}

#[async_trait]
impl<F> Rule for NativeRule<F>
where
    F: Fn(&WindowInput) -> Result<Vec<OutRow>, RuleError> + Send + Sync,
{
    fn spec(&self) -> RuleSpec {
        self.spec.clone()
    }

    async fn on_window(&self, input: &WindowInput) -> Result<Vec<OutRow>, RuleError> {
        (self.func)(input)
    }
}

/// Maximum guest output document: a DoS guard well above legitimate rows.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Maximum rows a guest may return per window.
pub const MAX_OUTPUT_ROWS: usize = 100_000;

/// Reject oversized guest output documents.
fn check_output_len(len: usize) -> Result<(), RuleError> {
    if len > MAX_OUTPUT_BYTES {
        return Err(RuleError::Execution("rule output too large".to_owned()));
    }
    Ok(())
}

/// Reject over-budget row counts.
fn check_row_count(rows: usize) -> Result<(), RuleError> {
    if rows > MAX_OUTPUT_ROWS {
        return Err(RuleError::Execution("rule exceeded row budget".to_owned()));
    }
    Ok(())
}

/// Fuel- and memory-capped wasmtime host for one guest module.
pub struct RuleHost {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    spec: RuleSpec,
    fuel: u64,
    max_memory_bytes: usize,
}

struct HostState {
    limiter: MemoryCap,
}

struct MemoryCap {
    max: usize,
}

impl wasmtime::ResourceLimiter for MemoryCap {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, anyhow::Error> {
        Ok(desired <= self.max)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, anyhow::Error> {
        Ok(true)
    }
}

impl RuleHost {
    /// Compile `wasm_bytes` and verify its declared [`RuleSpec`].
    pub fn load(
        wasm_bytes: &[u8],
        spec: RuleSpec,
        fuel: u64,
        max_memory_mb: u64,
    ) -> Result<Self, RuleError> {
        if !crate::abi::SUPPORTED_ABI_VERSIONS.contains(&spec.abi_version) {
            return Err(RuleError::AbiMismatch {
                got: spec.abi_version,
                supported: crate::abi::SUPPORTED_ABI_VERSIONS.to_vec(),
            });
        }
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).map_err(|e| RuleError::Load(e.to_string()))?;
        let module = wasmtime::Module::new(&engine, wasm_bytes)
            .map_err(|e| RuleError::Load(e.to_string()))?;
        Ok(Self {
            engine,
            module,
            spec,
            fuel: fuel.max(1),
            max_memory_bytes: (max_memory_mb.max(1) as usize).saturating_mul(1024 * 1024),
        })
    }

    /// Declared rule identity.
    #[must_use]
    pub fn spec(&self) -> &RuleSpec {
        &self.spec
    }

    /// Run `on_window` for one height window.
    pub async fn on_window(&self, input: &WindowInput) -> Result<Vec<OutRow>, RuleError> {
        // ponytail: blocking guest call on a blocking thread; the async
        // boundary stays thin. Move to epoch-based pre-emption if a guest
        // ever needs mid-window cancellation.
        let input_bytes = input.encode()?;
        let engine = self.engine.clone();
        let module = self.module.clone();
        let fuel = self.fuel;
        let max_memory = self.max_memory_bytes;
        tokio::task::spawn_blocking(move || {
            Self::call_guest(&engine, &module, fuel, max_memory, &input_bytes)
        })
        .await
        .map_err(|e| RuleError::Execution(format!("rule task panicked: {e}")))?
    }

    fn call_guest(
        engine: &wasmtime::Engine,
        module: &wasmtime::Module,
        fuel: u64,
        max_memory: usize,
        input_bytes: &[u8],
    ) -> Result<Vec<OutRow>, RuleError> {
        let mut store = wasmtime::Store::new(
            engine,
            HostState {
                limiter: MemoryCap { max: max_memory },
            },
        );
        store
            .set_fuel(fuel)
            .map_err(|e| RuleError::Execution(e.to_string()))?;
        store.limiter(|state| &mut state.limiter);

        let linker = wasmtime::Linker::new(engine);
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| RuleError::Load(e.to_string()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| RuleError::Load("guest must export `memory`".to_owned()))?;
        let alloc = instance
            .get_typed_func::<u32, u32>(&mut store, "alloc")
            .map_err(|e| RuleError::Load(format!("guest must export `alloc`: {e}")))?;
        let on_window = instance
            .get_typed_func::<(u32, u32), u64>(&mut store, "on_window")
            .map_err(|e| RuleError::Load(format!("guest must export `on_window`: {e}")))?;

        let input_len = u32::try_from(input_bytes.len())
            .map_err(|_| RuleError::Execution("window too large".to_owned()))?;
        let input_ptr = alloc.call(&mut store, input_len).map_err(map_trap)?;
        memory
            .write(&mut store, input_ptr as usize, input_bytes)
            .map_err(|e| RuleError::Execution(e.to_string()))?;
        debug!(bytes = input_bytes.len(), "rule window in");

        let packed = on_window
            .call(&mut store, (input_ptr, input_len))
            .map_err(map_trap)?;
        let out_ptr = (packed & 0xffff_ffff) as usize;
        let out_len = (packed >> 32) as usize;
        check_output_len(out_len)?;
        let mut out_bytes = vec![0u8; out_len];
        memory
            .read(&store, out_ptr, &mut out_bytes)
            .map_err(|e| RuleError::Execution(e.to_string()))?;
        let rows = WindowInput::decode_output(&out_bytes)?;
        if let Err(e) = check_row_count(rows.len()) {
            warn!(rows = rows.len(), "rule exceeded row budget");
            return Err(e);
        }
        Ok(rows)
    }
}

fn map_trap(error: anyhow::Error) -> RuleError {
    let text = format!("{error:#}").to_ascii_lowercase();
    if text.contains("fuel") {
        return RuleError::OutOfFuel;
    }
    RuleError::Execution(format!("{error:#}"))
}

/// Shared rule handle: native or hosted.
#[derive(Clone)]
pub enum RuleHandle {
    /// In-process rule.
    Native(Arc<dyn Rule>),
}

impl RuleHandle {
    /// Run whichever rule this handle wraps.
    pub async fn on_window(&self, input: &WindowInput) -> Result<Vec<OutRow>, RuleError> {
        match self {
            Self::Native(rule) => rule.on_window(input).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ABI_VERSION, FeedSlice};

    fn input() -> WindowInput {
        WindowInput {
            abi_version: ABI_VERSION,
            events: Vec::new(),
            blocks: Vec::new(),
            feeds: vec![FeedSlice {
                name: "f".to_owned(),
                at_height: 1,
                rows: serde_json::json!({}),
            }],
            window_lo: 1,
            window_hi: 2,
        }
    }

    /// Minimal guest: ignores input, returns `[]` from a static segment.
    const GUEST_WAT: &str = r#"
        (module
            (memory (export "memory") 1)
            (data (i32.const 1024) "[]")
            (func (export "alloc") (param i32) (result i32)
                i32.const 2048)
            (func (export "on_window") (param i32 i32) (result i64)
                i64.const 0x0000000200000400))"#;

    fn guest_bytes() -> Vec<u8> {
        wat::parse_str(GUEST_WAT).expect("wat parses")
    }

    fn guest_spec() -> RuleSpec {
        RuleSpec {
            name: "test".to_owned(),
            version: 1,
            abi_version: ABI_VERSION,
        }
    }

    #[test]
    fn host_rejects_unknown_abi() {
        let mut spec = guest_spec();
        spec.abi_version = 999;
        assert!(matches!(
            RuleHost::load(&guest_bytes(), spec, 1_000_000, 16),
            Err(RuleError::AbiMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn guest_round_trip_returns_empty_rows() {
        let host =
            RuleHost::load(&guest_bytes(), guest_spec(), 10_000_000, 16).expect("host loads");
        let rows = host.on_window(&input()).await.expect("call");
        assert!(rows.is_empty());
    }

    #[test]
    fn output_and_row_limits_are_pinned() {
        assert_eq!(MAX_OUTPUT_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_OUTPUT_ROWS, 100_000);
        assert!(check_output_len(MAX_OUTPUT_BYTES).is_ok());
        assert!(check_output_len(MAX_OUTPUT_BYTES.saturating_add(1)).is_err());
        assert!(check_row_count(MAX_OUTPUT_ROWS).is_ok());
        assert!(check_row_count(MAX_OUTPUT_ROWS.saturating_add(1)).is_err());
        assert!(check_output_len(2).is_ok());
        assert!(check_row_count(0).is_ok());
    }

    /// Guest declaring 100 pages up front: instantiation under a 1MiB cap
    /// must fail, proving the memory limiter is consulted.
    #[tokio::test]
    async fn memory_cap_rejects_oversized_guest() {
        const BIG_WAT: &str = r#"
            (module
                (memory (export "memory") 100)
                (data (i32.const 1024) "[]")
                (func (export "alloc") (param i32) (result i32)
                    i32.const 2048)
                (func (export "on_window") (param i32 i32) (result i64)
                    i64.const 0x0000000200000400))"#;
        let bytes = wat::parse_str(BIG_WAT).expect("wat parses");
        let host = RuleHost::load(&bytes, guest_spec(), 10_000_000, 1).expect("host loads");
        assert!(host.on_window(&input()).await.is_err());
    }

    /// Guest growing a table: the `table_growing` allowance is load-bearing
    /// (denying it traps the guest).
    #[tokio::test]
    async fn table_growth_is_allowed() {
        const TABLE_WAT: &str = r#"
            (module
                (table $t 0 funcref)
                (memory (export "memory") 1)
                (data (i32.const 1024) "[]")
                (func (export "alloc") (param i32) (result i32)
                    i32.const 2048)
                (func (export "on_window") (param i32 i32) (result i64)
                    (if (i32.eq (table.grow $t (ref.null func) (i32.const 1)) (i32.const -1))
                        (then unreachable))
                    i64.const 0x0000000200000400))"#;
        let bytes = wat::parse_str(TABLE_WAT).expect("wat parses");
        let host = RuleHost::load(&bytes, guest_spec(), 10_000_000, 16).expect("host loads");
        let rows = host.on_window(&input()).await.expect("call");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn rule_handle_dispatches_to_the_wrapped_rule() {
        use std::sync::Arc;
        let rule = NativeRule::new(guest_spec(), |input| {
            Ok(vec![OutRow {
                height: input.window_lo,
                rule_version: 1,
                commitment: 1,
                values: serde_json::json!({}),
            }])
        });
        let handle = RuleHandle::Native(Arc::new(rule));
        let rows = handle.on_window(&input()).await.expect("run");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].height, 1);
    }

    #[tokio::test]
    async fn guest_out_of_fuel_is_quarantine_signal() {
        // 1 unit of fuel cannot complete instantiation + call.
        let host = RuleHost::load(&guest_bytes(), guest_spec(), 1, 16).expect("host loads");
        let result = host.on_window(&input()).await;
        assert!(matches!(
            result,
            Err(RuleError::OutOfFuel) | Err(RuleError::Execution(_))
        ));
    }

    #[tokio::test]
    async fn native_rule_runs_in_process() {
        let rule = NativeRule::new(guest_spec(), |input| {
            Ok(vec![OutRow {
                height: input.window_lo,
                rule_version: 1,
                commitment: 1,
                values: serde_json::json!({}),
            }])
        });
        let rows = rule.on_window(&input()).await.expect("run");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].height, 1);
    }
}
