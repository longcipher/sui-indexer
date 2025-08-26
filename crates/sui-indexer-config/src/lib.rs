use std::path::Path;

use eyre::Result;
use serde::{Deserialize, Serialize};
use url::Url;

/// Main configuration for the Sui Indexer
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexerConfig {
    /// Network configuration for connecting to Sui
    pub network: NetworkConfig,
    /// Database configuration
    pub database: DatabaseConfig,
    /// Event indexing configuration
    pub events: EventsConfig,
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
    /// Number of events to process in a batch
    pub batch_size: usize,
    /// Maximum concurrent event processors
    pub max_concurrent_batches: usize,
    /// Event filters to apply
    pub filters: Vec<EventFilter>,
    /// Whether to index transaction effects
    pub index_transactions: bool,
    /// Whether to index object changes
    pub index_objects: bool,
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
            batch_size: 100,
            max_concurrent_batches: 10,
            filters: vec![],
            index_transactions: true,
            index_objects: true,
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
}
