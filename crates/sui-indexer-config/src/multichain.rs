//! Multichain job-engine configuration: chain identity, job specs, tiers.
//!
//! A job is data, not a process: adding a data condition is a config/DB
//! operation, not a code change. All new sections default so pre-existing
//! single-chain TOML files keep loading unchanged.

use serde::{Deserialize, Serialize};

/// Chain identity: which adapter serves this process.
///
/// One process serves one chain (`[chain] kind = "solana"`); adding a chain
/// never touches the sync engine, the job engine or the query layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChainConfig {
    /// Chain family: `move` | `svm` | `evm` (aliases: `sui`, `solana`).
    pub kind: String,
    /// Stable chain key used in tables, metrics and logs (`sui-mainnet`).
    pub chain_id: String,
    /// Chain endpoint. Empty means the legacy `[network]` endpoint.
    pub endpoint: String,
    /// Read commitment: `confirmed` (Solana detection) or `finalized`.
    pub commitment: String,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            kind: "move".to_owned(),
            chain_id: "sui-testnet".to_owned(),
            endpoint: String::new(),
            commitment: "finalized".to_owned(),
        }
    }
}

/// Execution tier for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobTier {
    /// Declarative SQL: `INSERT INTO <target> SELECT …` plus an MV for live rows.
    #[default]
    Sql,
    /// Stateful detection: `rule.on_window(ctx)` via the WASM host.
    Wasm,
}

/// Height context a job needs around the current window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Heights behind the cursor included in the window.
    pub lookback: u64,
    /// Heights ahead of the cursor included in the window.
    pub lookahead: u64,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            lookback: 1,
            lookahead: 0,
        }
    }
}

/// Event pre-filter for a job (mapped onto the universal `events` table).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobFilter {
    /// Allowed emitters (log address / program id / `package::module`).
    pub emitters: Vec<String>,
    /// Allowed topics (topic0 / discriminator / event type).
    pub topics: Vec<String>,
}

/// Scan plan for a job version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanConfig {
    /// First height to scan.
    pub from: u64,
    /// Last height to scan (`"head"` follows the tip; a number is bounded).
    pub to: String,
    /// Heights per `INSERT … SELECT` chunk.
    pub chunk: u64,
    /// Scheduling priority: `backfill` or `realtime`.
    pub priority: String,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            from: 0,
            to: "head".to_owned(),
            chunk: 10_000,
            priority: "backfill".to_owned(),
        }
    }
}

/// Versioned output table declaration for a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Stable alias consumed by queries (`job_sandwich`).
    pub table: String,
    /// `ORDER BY` columns of the physical table.
    pub order_by: Vec<String>,
    /// Table engine (`ReplacingMergeTree` on ClickHouse, heap on PG).
    pub engine: String,
    /// Partition expression (`toYYYYMM(block_ts)` on ClickHouse, `RANGE (height)` on PG).
    pub partition_by: String,
    /// Retention (`180d`; empty means keep).
    pub ttl: String,
    /// Reorg handling: `block_scoped` | `refreshable` | `none`.
    pub reorg_mode: String,
    /// Hot-window mirror in PostgreSQL (`7d`; empty means no mirror).
    pub pg_hot: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            table: String::new(),
            order_by: Vec::new(),
            engine: "ReplacingMergeTree".to_owned(),
            partition_by: "toYYYYMM(block_ts)".to_owned(),
            ttl: String::new(),
            reorg_mode: "block_scoped".to_owned(),
            pg_hot: String::new(),
        }
    }
}

/// Per-job state requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Stateless detection from event amounts and ordering.
    #[default]
    None,
    /// Height-scoped MVCC key/value snapshot + delta.
    Kv,
}

/// Runtime quotas: a job that exceeds its budget is failed and quarantined,
// never allowed to starve the sync engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// WASM fuel budget per window.
    pub fuel: u64,
    /// WASM memory cap in MiB.
    pub max_memory_mb: u64,
    /// Max rows written per window.
    pub max_rows_per_window: u64,
    /// Max rows written per second (write throttle).
    pub max_rows_per_second: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            fuel: 5_000_000_000,
            max_memory_mb: 512,
            max_rows_per_window: 100_000,
            max_rows_per_second: 50_000,
        }
    }
}

/// One user-defined indexing job (TOML `[[jobs]]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    /// Job name (`solana-sandwich`).
    pub name: String,
    /// Output version. A logic change bumps the version; the scheduler
    /// builds the new version and swaps the alias when it catches up.
    #[serde(default = "default_job_version")]
    pub version: u32,
    /// Source table: `events` | `txs` | `native.<table>` | `feed.<name>`.
    #[serde(default = "default_job_source")]
    pub source: String,
    /// Execution tier.
    #[serde(default)]
    pub tier: JobTier,
    /// Height context needed.
    #[serde(default)]
    pub window: WindowConfig,
    /// Event pre-filter.
    #[serde(default)]
    pub filter: JobFilter,
    /// Scan plan.
    #[serde(default)]
    pub scan: ScanConfig,
    /// Output table declaration.
    #[serde(default)]
    pub output: OutputConfig,
    /// SQL body (SQL tier): `SELECT … FROM events WHERE height ∈ [lo, hi)`.
    /// The executor binds `{lo}` / `{hi}` per chunk.
    #[serde(default)]
    pub sql: String,
    /// WASM module path (WASM tier).
    #[serde(default)]
    pub module: String,
    /// State requirements.
    #[serde(default)]
    pub state: JobState,
    /// Runtime quotas.
    #[serde(default)]
    pub runtime: RuntimeConfig,
    /// Desired state: `active` | `paused` | `retired`.
    #[serde(default = "default_job_desired")]
    pub desired: String,
}

fn default_job_version() -> u32 {
    1
}

fn default_job_source() -> String {
    "events".to_owned()
}

fn default_job_desired() -> String {
    "active".to_owned()
}

impl Default for JobSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: 1,
            source: "events".to_owned(),
            tier: JobTier::Sql,
            window: WindowConfig::default(),
            filter: JobFilter::default(),
            scan: ScanConfig::default(),
            output: OutputConfig::default(),
            sql: String::new(),
            module: String::new(),
            state: JobState::None,
            runtime: RuntimeConfig::default(),
            desired: "active".to_owned(),
        }
    }
}

impl JobSpec {
    /// Stable content hash over the spec plus code digest (SQL body or module
    /// path). A logic change alters the hash, which is what triggers a rescan.
    ///
    /// The struct itself is serialized, never an ad-hoc `json!` map:
    /// struct field order is fixed by declaration, while map ordering
    /// follows the `preserve_order` feature flag and varies between builds.
    /// `desired` stays out: pausing a job must not trigger a re-scan.
    #[must_use]
    pub fn spec_hash(&self) -> String {
        #[derive(Serialize)]
        struct Hashed<'a> {
            name: &'a str,
            version: u32,
            source: &'a str,
            tier: JobTier,
            window: &'a WindowConfig,
            filter: &'a JobFilter,
            scan: &'a ScanConfig,
            output: &'a OutputConfig,
            sql: &'a str,
            module: &'a str,
            state: JobState,
            runtime: &'a RuntimeConfig,
        }
        let hashed = Hashed {
            name: &self.name,
            version: self.version,
            source: &self.source,
            tier: self.tier,
            window: &self.window,
            filter: &self.filter,
            scan: &self.scan,
            output: &self.output,
            sql: &self.sql,
            module: &self.module,
            state: self.state,
            runtime: &self.runtime,
        };
        let bytes = serde_json::to_vec(&hashed).unwrap_or_default();
        format!("{:016x}", fnv1a64(&bytes))
    }

    /// Validate the spec; returns the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("job name must not be empty".to_owned());
        }
        if self.version == 0 {
            return Err("job version must be >= 1".to_owned());
        }
        if !matches!(self.source.as_str(), "events" | "txs")
            && !self.source.starts_with("native.")
            && !self.source.starts_with("feed.")
        {
            return Err(format!("unknown job source: {}", self.source));
        }
        if self.output.table.trim().is_empty() {
            return Err("job output.table must not be empty".to_owned());
        }
        if self.scan.chunk == 0 {
            return Err("job scan.chunk must be >= 1".to_owned());
        }
        if !matches!(
            self.output.reorg_mode.as_str(),
            "block_scoped" | "refreshable" | "none"
        ) {
            return Err(format!("unknown reorg_mode: {}", self.output.reorg_mode));
        }
        if !matches!(self.desired.as_str(), "active" | "paused" | "retired") {
            return Err(format!("unknown desired state: {}", self.desired));
        }
        match self.tier {
            JobTier::Sql => {
                if self.sql.trim().is_empty() {
                    return Err("sql-tier jobs require a sql body".to_owned());
                }
            }
            JobTier::Wasm => {
                if self.module.trim().is_empty() {
                    return Err("wasm-tier jobs require a module path".to_owned());
                }
            }
        }
        Ok(())
    }
}

/// 64-bit FNV-1a: stable across runs (unlike `DefaultHasher`).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Columnar archive tier (ClickHouse): archive of record and all derived/job
/// data. PostgreSQL keeps the hot window and the control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickHouseConfig {
    /// Enable the ClickHouse archive tier.
    pub enabled: bool,
    /// HTTP URL of the ClickHouse server.
    pub url: String,
    /// Database for archive tables.
    pub database: String,
    /// Database for job outputs and analytics views.
    pub analytics_database: String,
    /// Rows per `RowBinary` insert chunk.
    pub insert_chunk_rows: usize,
    /// Insert retries.
    pub insert_retries: u32,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://localhost:8123".to_owned(),
            database: "indexer".to_owned(),
            analytics_database: "analytics".to_owned(),
            insert_chunk_rows: 10_000,
            insert_retries: 3,
        }
    }
}

/// WASM rule-host quotas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleHostConfig {
    /// Maximum WASM fuel per `on_window` call.
    pub fuel: u64,
    /// Maximum linear memory in MiB per instance.
    pub max_memory_mb: u64,
    /// Supported host ABI versions (newest first).
    pub abi_versions: Vec<u32>,
}

impl Default for RuleHostConfig {
    fn default() -> Self {
        Self {
            fuel: 5_000_000_000,
            max_memory_mb: 512,
            abi_versions: vec![1],
        }
    }
}

/// External data source (CEX prices, …) with the same window/cursor semantics
/// as chain tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedConfig {
    /// Feed name (`binance_price`).
    pub name: String,
    /// Feed kind (`cex_price`, …).
    pub kind: String,
    /// Source-specific settings (symbol, bucket, endpoint, …).
    pub settings: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_spec_validates_tier_requirements() {
        let mut spec = JobSpec {
            name: "sandwich".to_owned(),
            output: OutputConfig {
                table: "job_sandwich".to_owned(),
                ..OutputConfig::default()
            },
            sql: "SELECT 1".to_owned(),
            ..JobSpec::default()
        };
        assert!(spec.validate().is_ok());
        spec.sql.clear();
        assert!(spec.validate().is_err());
        spec.tier = JobTier::Wasm;
        spec.module = "sandwich.wasm".to_owned();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn job_spec_rejects_bad_source_reorg_and_desired() {
        let mut spec = JobSpec {
            name: "x".to_owned(),
            output: OutputConfig {
                table: "job_x".to_owned(),
                ..OutputConfig::default()
            },
            sql: "SELECT 1".to_owned(),
            ..JobSpec::default()
        };
        spec.source = "bogus".to_owned();
        assert!(spec.validate().is_err());
        spec.source = "events".to_owned();
        spec.output.reorg_mode = "bogus".to_owned();
        assert!(spec.validate().is_err());
        spec.output.reorg_mode = "refreshable".to_owned();
        spec.desired = "bogus".to_owned();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn spec_hash_is_stable_and_content_sensitive() {
        let spec = JobSpec {
            name: "sandwich".to_owned(),
            output: OutputConfig {
                table: "job_sandwich".to_owned(),
                ..OutputConfig::default()
            },
            sql: "SELECT 1".to_owned(),
            ..JobSpec::default()
        };
        let a = spec.spec_hash();
        let b = spec.spec_hash();
        assert_eq!(a, b);
        let mut changed = spec.clone();
        changed.sql = "SELECT 2".to_owned();
        assert_ne!(a, changed.spec_hash());
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn minimal_job_toml_gets_sane_defaults() {
        let spec: JobSpec = toml::from_str(
            r#"
name = "minimal"
[output]
table = "job_minimal"
"#,
        )
        .expect("minimal job parses");
        assert_eq!(spec.version, 1);
        assert_eq!(spec.source, "events");
        assert_eq!(spec.desired, "active");
        assert_eq!(spec.scan.to, "head");
        assert_eq!(spec.scan.chunk, 10_000);
    }

    #[test]
    fn fnv_known_vectors() {
        assert_eq!(format!("{:016x}", fnv1a64(b"")), "cbf29ce484222325");
        assert_eq!(format!("{:016x}", fnv1a64(b"foobar")), "85944171f73967e8");
    }

    #[test]
    fn spec_hash_is_pinned() {
        let spec = JobSpec {
            name: "pinned".to_owned(),
            output: OutputConfig {
                table: "job_pinned".to_owned(),
                ..OutputConfig::default()
            },
            sql: "SELECT 1".to_owned(),
            ..JobSpec::default()
        };
        // Golden: struct-serialized canonical form (see `spec_hash`).
        assert_eq!(spec.spec_hash(), "656976f562f40cbb");
    }

    #[test]
    fn chain_config_defaults_to_move_finalized() {
        let chain = ChainConfig::default();
        assert_eq!(chain.kind, "move");
        assert_eq!(chain.commitment, "finalized");
    }

    #[test]
    fn full_job_toml_round_trips() {
        let toml_str = r#"
name = "solana-sandwich"
version = 3
source = "events"
tier = "wasm"
module = "sandwich.wasm"
desired = "active"

[window]
lookback = 1
lookahead = 0

[filter]
emitters = ["raydium"]
topics = ["swap"]

[scan]
from = 0
to = "head"
chunk = 10000
priority = "backfill"

[output]
table = "job_sandwich"
order_by = ["slot", "ix_idx"]
engine = "ReplacingMergeTree"
partition_by = "toYYYYMM(block_ts)"
ttl = "180d"
reorg_mode = "block_scoped"
pg_hot = "7d"
state = "none"

[runtime]
fuel = 5000000000
max_memory_mb = 512
max_rows_per_window = 100000
max_rows_per_second = 50000
"#;
        let spec: JobSpec = toml::from_str(toml_str).expect("job toml parses");
        assert_eq!(spec.name, "solana-sandwich");
        assert_eq!(spec.version, 3);
        assert_eq!(spec.state, JobState::None);
        assert!(spec.validate().is_ok());
    }
}
