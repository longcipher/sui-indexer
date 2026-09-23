use std::path::Path;

use eyre::Result;
use serde::{Deserialize, Serialize};
use url::Url;

pub mod multichain;
pub use multichain::{
    ChainConfig, ClickHouseConfig, FeedConfig, JobFilter, JobSpec, JobState, JobTier, OutputConfig,
    RuleHostConfig, RuntimeConfig, ScanConfig, WindowConfig,
};

/// Main configuration for the Sui Indexer
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexerConfig {
    /// Network configuration for connecting to Sui
    pub network: NetworkConfig,
    /// Database configuration
    pub database: DatabaseConfig,
    /// Event indexing configuration
    pub events: EventsConfig,
    /// Sync engine configuration (tracker + backfiller + repair worker)
    pub sync: SyncConfig,
    /// Repair queue configuration
    pub repair: RepairConfig,
    /// Cold archive configuration
    pub archive: ArchiveConfig,
    /// Read-only SQL gateway configuration
    pub query: QueryConfig,
    /// HTTP query API configuration
    pub api: ApiConfig,
    /// Webhook sinks for event push
    #[serde(default)]
    pub sinks: Vec<WebhookSink>,
    /// Threshold alert rules
    #[serde(default)]
    pub alerts: Vec<AlertRule>,
    /// Protocol presets for tagging
    #[serde(default)]
    pub protocols: Vec<ProtocolPreset>,
    /// Chain identity (one process serves one chain).
    #[serde(default)]
    pub chain: ChainConfig,
    /// User-defined indexing jobs (a job is data, not a process).
    #[serde(default)]
    pub jobs: Vec<JobSpec>,
    /// Columnar archive tier.
    #[serde(default)]
    pub clickhouse: ClickHouseConfig,
    /// WASM rule-host quotas.
    #[serde(default)]
    pub rule_host: RuleHostConfig,
    /// External data sources with window/cursor semantics.
    #[serde(default)]
    pub feeds: Vec<FeedConfig>,
}

/// Network configuration for Sui blockchain connection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Sui gRPC endpoint URL
    pub grpc_url: Url,
    /// Network name (mainnet, testnet, devnet, localnet)
    pub network: String,
    /// Connection pool settings
    pub pool: PoolConfig,
    /// Retry configuration
    pub retry: RetryConfig,
}

/// Database connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Database connection URL
    pub url: String,
    /// Maximum number of connections in pool
    pub max_connections: u32,
    /// Minimum idle connections
    pub min_connections: u32,
    /// Connection timeout in seconds
    pub connect_timeout: u64,
    /// Idle connection timeout in seconds
    pub idle_timeout: Option<u64>,
    /// Whether to run migrations on startup
    pub auto_migrate: bool,
}

/// Event indexing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsConfig {
    /// Starting checkpoint for indexing
    pub start_checkpoint: Option<u64>,
    /// Last checkpoint for backfill runs (inclusive). `None` means follow the tip.
    pub last_checkpoint: Option<u64>,
    /// Number of events to process in a batch
    pub batch_size: usize,
    /// Maximum concurrent event processors
    pub max_concurrent_batches: usize,
    /// Poll interval in seconds when running in poll ingestion mode
    pub poll_interval_secs: u64,
    /// Ingestion mode: `stream` (subscription + backfill) or `poll`
    pub ingestion_mode: IngestionMode,
    /// Event filters to apply
    #[serde(default)]
    pub filters: Vec<EventFilter>,
    /// Whether to index transaction effects
    pub index_transactions: bool,
    /// Whether to index object changes
    pub index_objects: bool,
    /// Checkpoint retention for pruning. `None` disables pruning.
    pub retention: Option<u64>,
}

/// Ingestion mode for pulling checkpoint data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IngestionMode {
    /// Follow the tip via subscription-style streaming with backfill catch-up.
    #[default]
    Stream,
    /// Poll `get_latest_checkpoint` on a fixed interval.
    Poll,
}

/// Event filter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFilter {
    /// Package ID to filter by (optional)
    pub package: Option<String>,
    /// Module name to filter by (optional)
    pub module: Option<String>,
    /// Event type to filter by (optional)
    pub event_type: Option<String>,
    /// Sender address to filter by (optional)
    pub sender: Option<String>,
}

/// Connection pool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Maximum number of connections
    pub max_connections: usize,
    /// Connection timeout in seconds
    pub timeout: u64,
    /// Keep-alive interval in seconds
    pub keep_alive: u64,
}

/// Retry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: usize,
    /// Initial delay between retries in milliseconds
    pub initial_delay: u64,
    /// Maximum delay between retries in milliseconds
    pub max_delay: u64,
    /// Exponential backoff multiplier
    pub backoff_multiplier: f64,
}

/// HTTP query API configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Enable the HTTP query API.
    pub enabled: bool,
    /// Bind address for the HTTP server.
    pub listen: String,
}

/// Webhook sink configuration for event push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookSink {
    /// Sink name.
    pub name: String,
    /// Target URL.
    pub url: String,
    /// Optional package allowlist (empty means all packages).
    #[serde(default)]
    pub packages: Vec<String>,
    /// Optional bearer token sent as Authorization header.
    pub bearer_token: Option<String>,
}

/// Threshold alert rule configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// Rule name.
    pub name: String,
    /// Package to watch (empty means all packages).
    pub package: String,
    /// Minimum matching events in a checkpoint to trigger.
    pub min_events_per_checkpoint: u64,
}

/// Protocol preset configuration for the protocol registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolPreset {
    /// Protocol name (e.g. `navi`, `cetus`, `deepbook`).
    pub name: String,
    /// Package IDs belonging to the protocol.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Tags attached to matched events.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            grpc_url: "https://fullnode.testnet.sui.io:443"
                .parse()
                .expect("Default gRPC URL should be valid"),
            network: "testnet".to_string(),
            pool: PoolConfig::default(),
            retry: RetryConfig::default(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgresql://localhost/sui_indexer".to_string(),
            max_connections: 20,
            min_connections: 5,
            connect_timeout: 30,
            idle_timeout: Some(600),
            auto_migrate: true,
        }
    }
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            start_checkpoint: None,
            last_checkpoint: None,
            batch_size: 100,
            max_concurrent_batches: 10,
            poll_interval_secs: 10,
            ingestion_mode: IngestionMode::Stream,
            filters: vec![],
            index_transactions: true,
            index_objects: true,
            retention: None,
        }
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            timeout: 30,
            keep_alive: 60,
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            tip_interval_secs: 2,
            tracker_batch_size: 50,
            backfill_enabled: true,
            backfill_batch_size: 200,
            backfill_concurrency: 8,
            lag_yield_threshold: 10,
            max_range_span: 200,
            failure_backoff_threshold: 5,
        }
    }
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 30,
            batch_size: 20,
            max_attempts: 10,
            backoff_base_secs: 30,
            backoff_max_secs: 3600,
        }
    }
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: "./archive".to_string(),
            segment_size: 10_000,
            hot_keep_checkpoints: None,
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_rows: 1000,
            timeout_ms: 5000,
            max_bytes: 10 * 1024 * 1024,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: 1000,
            max_delay: 10000,
            backoff_multiplier: 2.0,
        }
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:8080".to_string(),
        }
    }
}

/// Dual-lane sync engine configuration: a tip tracker follows the chain
/// head while a backfiller heals historical gaps newest-first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Poll interval in seconds for the tip tracker.
    pub tip_interval_secs: u64,
    /// Max checkpoints per tracker tick.
    pub tracker_batch_size: u64,
    /// Enable the historical backfiller lane.
    pub backfill_enabled: bool,
    /// Max checkpoints per backfiller tick.
    pub backfill_batch_size: u64,
    /// Max concurrent checkpoint fetches in the backfiller.
    pub backfill_concurrency: usize,
    /// Yield to the tracker when tip lag exceeds this many checkpoints.
    pub lag_yield_threshold: u64,
    /// Max single process_checkpoint_range span; larger spans split into chunks.
    pub max_range_span: u64,
    /// Consecutive failures after which the engine backs off a full interval.
    pub failure_backoff_threshold: u32,
}

/// Background repair worker configuration for checkpoints that failed to
/// ingest cleanly (durable queue with exponential backoff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairConfig {
    /// Enable the repair worker.
    pub enabled: bool,
    /// Poll interval in seconds.
    pub poll_interval_secs: u64,
    /// Max rows claimed per poll.
    pub batch_size: usize,
    /// Max attempts per checkpoint before parking it.
    pub max_attempts: i32,
    /// Base backoff in seconds, doubled per attempt up to the cap.
    pub backoff_base_secs: u64,
    /// Max backoff in seconds.
    pub backoff_max_secs: u64,
}

/// Cold archive configuration for full-history retention outside PostgreSQL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    /// Enable the archive writer.
    pub enabled: bool,
    /// Filesystem directory (or object prefix) for archive segments.
    pub directory: String,
    /// Archive segment after this many checkpoints per file.
    pub segment_size: u64,
    /// Keep PostgreSQL rows for at least this many checkpoints.
    pub hot_keep_checkpoints: Option<u64>,
}

/// Read-only SQL gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConfig {
    /// Max rows a single query may return.
    pub max_rows: u64,
    /// Query timeout in milliseconds.
    pub timeout_ms: u64,
    /// Max response payload in bytes before truncation.
    pub max_bytes: usize,
}

/// Configuration loader
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<IndexerConfig> {
        let content = std::fs::read_to_string(path)?;
        let config: IndexerConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration from environment variables and command line arguments
    pub fn from_env() -> Result<IndexerConfig> {
        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("SUI_INDEXER"))
            .build()?;

        let config: IndexerConfig = settings.try_deserialize()?;
        Ok(config)
    }

    /// Load configuration with multiple sources (file, env, args)
    pub fn load_with_sources<P: AsRef<Path>>(config_file: Option<P>) -> Result<IndexerConfig> {
        let mut builder = config::Config::builder();

        // Add default values
        builder = builder.add_source(config::Config::try_from(&IndexerConfig::default())?);

        // Add config file if provided
        if let Some(path) = config_file {
            if path.as_ref().exists() {
                builder = builder.add_source(config::File::from(path.as_ref()));
            }
        }

        // Add environment variables
        builder = builder.add_source(
            config::Environment::with_prefix("SUI_INDEXER")
                .prefix_separator("_")
                .separator("__"),
        );

        let settings = builder.build()?;
        let config: IndexerConfig = settings.try_deserialize()?;

        Ok(config)
    }

    /// Save configuration to a TOML file
    pub fn save_to_file<P: AsRef<Path>>(config: &IndexerConfig, path: P) -> Result<()> {
        let toml_content = toml::to_string_pretty(config)?;
        std::fs::write(path, toml_content)?;
        Ok(())
    }

    /// Generate example configuration file
    pub fn generate_example() -> String {
        let config = IndexerConfig::default();
        toml::to_string_pretty(&config)
            .unwrap_or_else(|_| "# Failed to generate example config".to_string())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn test_default_config() {
        let config = IndexerConfig::default();
        assert_eq!(config.network.network, "testnet");
        assert_eq!(config.database.max_connections, 20);
        assert_eq!(config.events.batch_size, 100);
        assert_eq!(config.events.ingestion_mode, IngestionMode::Stream);
        assert!(!config.api.enabled);
    }

    #[test]
    fn test_config_serialization() {
        let config = IndexerConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: IndexerConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.network.network, deserialized.network.network);
        assert_eq!(
            config.database.max_connections,
            deserialized.database.max_connections
        );
    }

    #[test]
    fn test_config_file_operations() -> Result<()> {
        let config = IndexerConfig::default();
        let temp_file = NamedTempFile::new()?;

        // Save config
        ConfigLoader::save_to_file(&config, temp_file.path())?;

        // Load config
        let loaded_config = ConfigLoader::from_file(temp_file.path())?;

        assert_eq!(config.network.network, loaded_config.network.network);
        Ok(())
    }

    #[test]
    fn test_example_generation() {
        let example = ConfigLoader::generate_example();
        assert!(!example.is_empty());
        assert!(example.contains("[network]"));
        assert!(example.contains("[database]"));
    }

    #[test]
    fn test_sync_repair_archive_query_defaults() {
        let config = IndexerConfig::default();
        assert!(config.sync.backfill_enabled);
        assert_eq!(config.sync.tip_interval_secs, 2);
        assert!(config.repair.enabled);
        assert_eq!(config.repair.max_attempts, 10);
        assert!(!config.archive.enabled);
        assert_eq!(config.query.max_rows, 1000);
        assert_eq!(config.query.timeout_ms, 5000);
    }

    #[test]
    fn test_example_file_parses_with_jobs() {
        let config = ConfigLoader::from_file("../../config.example.toml")
            .expect("config.example.toml parses");
        assert_eq!(config.chain.kind, "move");
        assert_eq!(config.chain.chain_id, "sui-testnet");
        assert!(!config.clickhouse.enabled);
        assert_eq!(config.rule_host.abi_versions, vec![1]);
        assert_eq!(config.jobs.len(), 1);
        assert!(config.jobs[0].validate().is_ok());
    }

    #[test]
    fn test_load_with_sources_prefers_file_over_defaults() -> Result<()> {
        let dir = tempfile::TempDir::new()?;
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            r#"
[chain]
kind = "svm"
chain_id = "test-chain-xyz"
"#,
        )?;
        let config = ConfigLoader::load_with_sources(Some(&path))?;
        assert_eq!(config.chain.chain_id, "test-chain-xyz");
        assert_eq!(config.chain.kind, "svm");
        Ok(())
    }

    #[test]
    fn test_from_env_fails_without_configuration() {
        // Scrub the process environment so the loader sees an empty source.
        // No other test reads the environment, so this is race-free here.
        let scrubbed: Vec<String> = std::env::vars()
            .map(|(key, _)| key)
            .filter(|key| key.starts_with("SUI_INDEXER"))
            .collect();
        for key in &scrubbed {
            unsafe { std::env::remove_var(key) };
        }
        assert!(ConfigLoader::from_env().is_err());
    }

    #[test]
    fn test_new_sections_roundtrip() {
        let config = IndexerConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("[sync]"));
        assert!(toml_str.contains("[repair]"));
        assert!(toml_str.contains("[archive]"));
        assert!(toml_str.contains("[query]"));
        let restored: IndexerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.sync.backfill_batch_size, 200);
        assert_eq!(restored.query.max_bytes, 10 * 1024 * 1024);
    }
}
