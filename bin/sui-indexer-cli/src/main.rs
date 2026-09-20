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
        /// Run the background repair worker alongside indexing
        #[arg(long, default_value_t = true)]
        repair: bool,
        /// Archive canonical segments alongside indexing
        #[arg(long)]
        archive: bool,
    },
    /// Stop the indexer
    Stop,
    /// Check indexer health
    Health,
    /// Show detailed status information
    Status {
        /// Watch mode with 1s refresh
        #[arg(short, long)]
        watch: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run a read-only SQL query over canonical tables
    Query {
        /// SELECT statement (SELECT-only, canonical tables)
        sql: String,
        /// Move event selector package::module::Name exposed as decoded_events CTE
        #[arg(long)]
        event: Option<String>,
        /// Max rows
        #[arg(short, long, default_value_t = 100)]
        limit: u64,
        /// Output format (table, json, csv)
        #[arg(short, long, default_value = "table")]
        format: String,
    },
    /// Manage balance insights derived from coin flows
    Insight {
        /// Subcommand: holders, metadata, refresh
        #[command(subcommand)]
        command: InsightCommands,
    },
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
    /// Repair due checkpoints from the durable queue then exit
    Repair {
        /// Max entries to reprocess
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Archive canonical checkpoint segments then exit
    Archive {
        /// First checkpoint (inclusive)
        #[arg(long)]
        from: u64,
        /// Last checkpoint (inclusive)
        #[arg(long)]
        to: u64,
    },
    /// Self-update the binary from GitHub releases
    SelfUpdate,
}

#[derive(Subcommand)]
enum InsightCommands {
    /// Top holders for a coin type
    Holders {
        /// Coin type (e.g. 0x2::sui::SUI)
        #[arg(long)]
        coin: String,
        /// Max rows
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// Balances held by an address
    Balances {
        /// Holder address
        #[arg(long)]
        holder: String,
        /// Max rows
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// Coin discovery metadata
    Metadata {
        /// Max rows
        #[arg(long, default_value_t = 20)]
        limit: u64,
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
            repair,
            archive,
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
            if archive {
                config.archive.enabled = true;
            }
            if !repair {
                config.repair.enabled = false;
            }
            let mut indexer = IndexerCore::new(config.clone()).await?;

            indexer.initialize().await?;

            info!("Sui Indexer initialized successfully");

            if config.api.enabled {
                let api_config = config.clone();
                let api_storage = indexer.storage().clone();
                let api_feed = indexer.block_feed().clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        sui_indexer_core::api::serve(&api_config, &api_storage, &api_feed).await
                    {
                        error!("Query API exited: {e}");
                    }
                });
            }

            if config.repair.enabled {
                let repair_config = config.repair.clone();
                let repair_storage = indexer.storage().clone();
                tokio::spawn(async move {
                    run_repair_loop(&repair_config, &repair_storage).await;
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
        Commands::Status { watch, json } => {
            if watch {
                loop {
                    run_status(&cli.config, json).await?;
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    print!("\x1B[2J\x1B[1;1H");
                }
            } else {
                run_status(&cli.config, json).await?;
            }
        }
        Commands::Query {
            sql,
            event,
            limit,
            format,
        } => {
            run_query(&cli.config, &sql, event.as_deref(), limit, &format).await?;
        }
        Commands::Insight { command } => {
            run_insight(&cli.config, command).await?;
        }
        Commands::Repair { limit } => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config.clone()).await?;
            indexer.initialize().await?;
            let bound = limit.max(1);
            let worker = sui_indexer_core::repair::RepairWorker::new(config.repair.clone());
            let storage = indexer.storage().clone();
            let repaired = worker
                .tick(&storage, async |sequence| {
                    let mut backfill = IndexerCore::new(config.clone()).await?;
                    backfill
                        .process_checkpoint_range(sequence, sequence)
                        .await?;
                    Ok(true)
                })
                .await
                .map(|repaired| repaired.min(bound))?;
            info!("Repair pass completed: {repaired} checkpoints");
        }
        Commands::Archive { from, to } => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config.clone()).await?;
            indexer.initialize().await?;
            let writer = sui_indexer_core::archive::ArchiveWriter::new(config.archive.clone());
            writer
                .archive_range(indexer.storage(), "default", from, to)
                .await?;
        }
        Commands::SelfUpdate => {
            run_self_update().await?;
        }
        Commands::Backfill { from, to } => {
            if from > to {
                error!("--from must be <= --to");
                std::process::exit(2);
            }
            let mut config = ConfigLoader::from_file(&cli.config)?;
            config.events.start_checkpoint = Some(from);
            config.events.last_checkpoint = Some(to);
            let mut indexer = IndexerCore::new(config.clone()).await?;
            indexer.initialize().await?;
            let committed = indexer.process_checkpoint_range(from, to).await?;
            indexer.storage().repair_derived_insights().await?;
            let writer = sui_indexer_core::archive::ArchiveWriter::new(config.archive.clone());
            if writer.enabled() {
                writer
                    .archive_range(indexer.storage(), "default", from, committed)
                    .await?;
            }
            if let Some(hot_keep) = config.archive.hot_keep_checkpoints
                && committed > hot_keep
            {
                let hot_boundary = committed.saturating_sub(hot_keep);
                indexer
                    .storage()
                    .record_archive_window(
                        "default",
                        Some(from),
                        Some(committed),
                        Some(hot_boundary),
                    )
                    .await?;
                info!("Hot boundary recorded at {hot_boundary}");
            }
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
            indexer
                .storage()
                .advance_continuous(&pipeline, to, to.saturating_sub(1), None)
                .await
                .unwrap_or(());
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
            sui_indexer_core::api::serve(&config, indexer.storage(), indexer.block_feed()).await?;
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

async fn run_status(config_path: &str, json: bool) -> Result<()> {
    let config = ConfigLoader::from_file(config_path)?;
    let mut indexer = IndexerCore::new(config.clone()).await?;

    info!("Checking indexer status");

    let healthy = indexer.health_check().await?;
    let snapshot = indexer.metrics_snapshot();
    let watermark = indexer
        .storage()
        .get_latest_checkpoint()
        .await
        .unwrap_or(None);
    let progress = indexer
        .storage()
        .get_progress("default")
        .await
        .unwrap_or(None);
    let counts = indexer.storage().table_counts().await.unwrap_or_default();
    let lag = snapshot
        .latest_checkpoint
        .saturating_sub(snapshot.committed_checkpoint);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "healthy": healthy,
                "network": config.network.network,
                "latest": snapshot.latest_checkpoint,
                "committed": snapshot.committed_checkpoint,
                "watermark": watermark,
                "lag": lag,
                "errors": snapshot.errors,
                "tables": {
                    "checkpoints": counts.checkpoints,
                    "transactions": counts.transactions,
                    "events": counts.events,
                    "objects": counts.objects,
                },
                "progress": progress,
            })
        );
        if !healthy {
            std::process::exit(1);
        }
        return Ok(());
    }

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
        info!("  - Lag behind tip: {lag}");
        info!("  - Errors: {}", snapshot.errors);
        info!(
            "  - Tables: checkpoints={} transactions={} events={} objects={}",
            counts.checkpoints, counts.transactions, counts.events, counts.objects
        );

        if let Some(watermark) = watermark {
            info!("Stored watermark: {watermark}");
        }

        info!("Status check completed successfully");
    } else {
        error!("Indexer Status: UNHEALTHY");
        std::process::exit(1);
    }

    Ok(())
}

async fn run_query(
    config_path: &str,
    sql: &str,
    event: Option<&str>,
    limit: u64,
    format: &str,
) -> Result<()> {
    let config = ConfigLoader::from_file(config_path)?;
    let indexer = IndexerCore::new(config.clone()).await?;
    indexer.initialize().await?;
    let gateway = sui_indexer_core::query_gateway::QueryGateway::new(config.query.clone());
    let result = gateway
        .execute(
            indexer.storage(),
            sui_indexer_core::query_gateway::GatewayQuery {
                sql: sql.to_string(),
                event: event.map(str::to_string),
                limit: Some(limit),
            },
        )
        .await?;
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        "csv" => print_gateway_csv(&result),
        _ => print_gateway_table(&result),
    }
    Ok(())
}

fn print_gateway_table(result: &sui_indexer_storage::GatewayResult) {
    println!("{}", result.columns.join(" | "));
    for row in &result.rows {
        let cells = result
            .columns
            .iter()
            .map(|column| row.get(column).map(|v| v.to_string()).unwrap_or_default())
            .collect::<Vec<_>>();
        println!("{}", cells.join(" | "));
    }
    if result.truncated {
        println!("... truncated at {} rows", result.row_count);
    }
}

fn print_gateway_csv(result: &sui_indexer_storage::GatewayResult) {
    println!("{}", result.columns.join(","));
    for row in &result.rows {
        let cells = result
            .columns
            .iter()
            .map(|column| {
                row.get(column)
                    .map(|v| v.to_string().replace(',', ";"))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        println!("{}", cells.join(","));
    }
}

async fn run_insight(config_path: &str, command: InsightCommands) -> Result<()> {
    let (sql, limit) = match &command {
        InsightCommands::Holders { coin, limit } => (
            format!(
                "SELECT holder, balance FROM balance_snapshots WHERE coin_type = '{}' ORDER BY balance DESC",
                coin.replace('\'', "''")
            ),
            *limit,
        ),
        InsightCommands::Balances { holder, limit } => (
            format!(
                "SELECT coin_type, balance FROM balance_snapshots WHERE holder = '{}' ORDER BY balance DESC",
                holder.replace('\'', "''")
            ),
            *limit,
        ),
        InsightCommands::Metadata { limit } => (
            "SELECT coin_type, flow_count, first_seen_checkpoint, last_seen_checkpoint FROM coin_metadata ORDER BY flow_count DESC"
                .to_string(),
            *limit,
        ),
    };
    run_query(config_path, &sql, None, limit, "table").await
}

async fn run_repair_loop(
    config: &sui_indexer_config::RepairConfig,
    storage: &sui_indexer_storage::StorageManager,
) {
    let worker = sui_indexer_core::repair::RepairWorker::new(config.clone());
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        config.poll_interval_secs.max(1),
    ));
    loop {
        interval.tick().await;
        let result = worker.tick(storage, async |_| Ok(false)).await;
        if let Err(e) = result {
            error!("Repair loop tick failed: {e}");
        }
    }
}

async fn run_self_update() -> Result<()> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    info!("Checking latest release for {os}/{arch}");
    let client = reqwest::Client::builder().build()?;
    let release: serde_json::Value = client
        .get("https://api.github.com/repos/longcipher/sui-indexer/releases/latest")
        .header("User-Agent", "sui-indexer")
        .send()
        .await?
        .json()
        .await?;
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    info!("Latest release: {tag} (manual install required for this build)");
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
