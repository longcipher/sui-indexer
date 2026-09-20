use clap::{Parser, Subcommand};
use eyre::Result;
use sui_indexer_config::ConfigLoader;
use sui_indexer_core::IndexerCore;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "sui-indexer")]
#[command(about = "Sui blockchain indexer")]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the indexer
    Start {
        /// Override the start checkpoint
        #[arg(long)]
        from: Option<u64>,
        /// Stop after this checkpoint (inclusive backfill)
        #[arg(long)]
        to: Option<u64>,
        /// Serve the HTTP query API alongside indexing
        #[arg(long)]
        serve_api: bool,
    },
    /// Stop the indexer
    Stop,
    /// Check indexer health
    Health,
    /// Show detailed status information
    Status,
    /// Backfill a checkpoint range then exit
    Backfill {
        /// First checkpoint (inclusive)
        #[arg(long)]
        from: u64,
        /// Last checkpoint (inclusive)
        #[arg(long)]
        to: u64,
    },
    /// Validate config, gRPC, database, and filters
    Doctor,
    /// Rewind the watermark for replay
    Rewind {
        /// Checkpoint to rewind to
        #[arg(long)]
        to: u64,
        /// Pipeline name (default: default)
        #[arg(long, default_value = "default")]
        pipeline: String,
    },
    /// Export events to stdout as JSON lines
    Export {
        /// First checkpoint (inclusive)
        #[arg(long)]
        from: u64,
        /// Last checkpoint (inclusive)
        #[arg(long)]
        to: u64,
        /// Optional package filter
        #[arg(long)]
        package: Option<String>,
        /// Max rows
        #[arg(long, default_value_t = 1000)]
        limit: u64,
    },
    /// Serve the HTTP query API only
    Serve,
    /// Generate shell completions
    Completion {
        /// Shell name (bash, zsh, fish, powershell)
        shell: String,
    },
    /// Generate an example config file
    Config {
        /// Output path
        #[arg(default_value = "config.toml")]
        output: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with info level by default
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            from,
            to,
            serve_api,
        } => {
            info!("Starting Sui Indexer");

            let mut config = ConfigLoader::from_file(&cli.config)?;
            if let Some(from) = from {
                config.events.start_checkpoint = Some(from);
            }
            if let Some(to) = to {
                config.events.last_checkpoint = Some(to);
            }
            if serve_api {
                config.api.enabled = true;
            }
            let mut indexer = IndexerCore::new(config.clone()).await?;

            indexer.initialize().await?;

            info!("Sui Indexer initialized successfully");

            if config.api.enabled {
                let api_config = config.clone();
                let api_storage = indexer.storage().clone();
                tokio::spawn(async move {
                    if let Err(e) = sui_indexer_core::api::serve(&api_config, &api_storage).await {
                        error!("Query API exited: {e}");
                    }
                });
            }

            indexer.start().await?;
        }
        Commands::Stop => {
            info!("Stopping Sui Indexer gracefully");
            std::process::exit(0);
        }
        Commands::Health => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let mut indexer = IndexerCore::new(config).await?;

            let healthy = indexer.health_check().await?;

            if healthy {
                info!("Indexer is healthy");
                std::process::exit(0);
            } else {
                info!("Some components are unhealthy");
                std::process::exit(1);
            }
        }
        Commands::Status => {
            run_status(&cli.config).await?;
        }
        Commands::Backfill { from, to } => {
            if from > to {
                error!("--from must be <= --to");
                std::process::exit(2);
            }
            let mut config = ConfigLoader::from_file(&cli.config)?;
            config.events.start_checkpoint = Some(from);
            config.events.last_checkpoint = Some(to);
            let mut indexer = IndexerCore::new(config).await?;
            indexer.initialize().await?;
            let committed = indexer.process_checkpoint_range(from, to).await?;
            info!("Backfill complete through checkpoint {committed}");
        }
        Commands::Doctor => {
            run_doctor(&cli.config).await?;
        }
        Commands::Rewind { to, pipeline } => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config).await?;
            indexer.initialize().await?;
            indexer.storage().rewind_watermark(&pipeline, to).await?;
            info!("Rewound pipeline {pipeline} to checkpoint {to}");
        }
        Commands::Export {
            from,
            to,
            package,
            limit,
        } => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config).await?;
            indexer.initialize().await?;
            let events = indexer
                .storage()
                .query_events(sui_indexer_storage::EventQueryFilter {
                    package: package.as_deref(),
                    module: None,
                    event_type: None,
                    sender: None,
                    from_checkpoint: Some(from),
                    to_checkpoint: Some(to),
                    limit,
                })
                .await?;
            for event in &events {
                use std::io::Write;
                let line = serde_json::to_string(event)?;
                writeln!(std::io::stdout(), "{line}")?;
            }
        }
        Commands::Serve => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config.clone()).await?;
            indexer.initialize().await?;
            sui_indexer_core::api::serve(&config, indexer.storage()).await?;
        }
        Commands::Completion { shell } => {
            use clap::CommandFactory;
            use std::io::BufWriter;
            use std::str::FromStr;
            let mut command = Cli::command();
            let shell = clap_complete::Shell::from_str(&shell)
                .map_err(|e| eyre::eyre!("Unknown shell: {e}"))?;
            let mut output = BufWriter::new(std::io::stdout());
            clap_complete::generate(shell, &mut command, "sui-indexer", &mut output);
        }
        Commands::Config { output } => {
            let example = ConfigLoader::generate_example();
            std::fs::write(&output, example)?;
            info!("Wrote example config to {output}");
        }
    }

    Ok(())
}

async fn run_status(config_path: &str) -> Result<()> {
    let config = ConfigLoader::from_file(config_path)?;
    let mut indexer = IndexerCore::new(config).await?;

    info!("Checking indexer status");

    let healthy = indexer.health_check().await?;

    if healthy {
        info!("Indexer Status: HEALTHY");

        info!("System Information:");
        info!("  - Version: {}", env!("CARGO_PKG_VERSION"));
        info!(
            "  - Build: {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("BUILD_TIMESTAMP").unwrap_or("unknown")
        );

        info!("  Network: Connected to Sui RPC");

        if let Ok(memory) = get_memory_usage() {
            info!("  Memory: {memory}");
        }

        let snapshot = indexer.metrics_snapshot();
        info!("Processing Statistics:");
        info!(
            "  - Checkpoints processed: {}",
            snapshot.checkpoints_processed
        );
        info!("  - Events processed: {}", snapshot.events_processed);
        info!(
            "  - Transactions processed: {}",
            snapshot.transactions_processed
        );
        info!("  - Latest checkpoint: {}", snapshot.latest_checkpoint);
        info!(
            "  - Committed checkpoint: {}",
            snapshot.committed_checkpoint
        );
        let lag = snapshot
            .latest_checkpoint
            .saturating_sub(snapshot.committed_checkpoint);
        info!("  - Lag behind tip: {lag}");
        info!("  - Errors: {}", snapshot.errors);

        if let Ok(Some(watermark)) = indexer.storage().get_latest_checkpoint().await {
            info!("Stored watermark: {watermark}");
        }

        info!("Status check completed successfully");
    } else {
        error!("Indexer Status: UNHEALTHY");
        std::process::exit(1);
    }

    Ok(())
}

async fn run_doctor(config_path: &str) -> Result<()> {
    let config = ConfigLoader::from_file(config_path)?;
    info!("Config loaded from {config_path}");

    let mut indexer = IndexerCore::new(config.clone()).await?;
    let healthy = indexer.health_check().await?;
    info!("gRPC + database health: {healthy}");

    for filter in &config.events.filters {
        if let Some(package) = &filter.package {
            match package.parse::<sui_types::base_types::ObjectID>() {
                Ok(_) => info!("Filter package OK: {package}"),
                Err(e) => error!("Filter package invalid {package}: {e}"),
            }
        }
        if let Some(event_type) = &filter.event_type
            && !event_type.contains("::")
        {
            error!("Filter event_type should be a fully qualified Move type: {event_type}");
        }
    }

    if config.events.batch_size == 0 {
        error!("events.batch_size must be > 0");
    }
    if config.events.max_concurrent_batches == 0 {
        error!("events.max_concurrent_batches must be > 0");
    }

    if !healthy {
        std::process::exit(1);
    }
    info!("Doctor checks passed");
    Ok(())
}

/// Get memory usage information
fn get_memory_usage() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()?;

        let rss_kb = String::from_utf8(output.stdout)?
            .trim()
            .parse::<u64>()
            .unwrap_or(0);

        let rss_mb = rss_kb / 1024;
        Ok(format!("{}MB RSS", rss_mb))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok("Memory info unavailable on this platform".to_string())
    }
}
