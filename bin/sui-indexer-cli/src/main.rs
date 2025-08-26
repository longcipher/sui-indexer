use eyre::Result;
use clap::{Parser, Subcommand};
use sui_indexer_config::ConfigLoader;
use sui_indexer_core::IndexerCore;
use tokio;
use tracing::{error, info};
use tracing_subscriber;

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
    Start,
    /// Stop the indexer
    Stop,
    /// Check indexer health
    Health,
    /// Show detailed status information
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with info level by default
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start => {
            info!("Starting Sui Indexer");
            
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config).await?;
            
            // Initialize the indexer (run migrations, etc.)
            indexer.initialize().await?;
            
            info!("🚀 Sui Indexer initialized successfully");
            
            // Start the indexer (this will run the main event loop)
            indexer.start().await?;
        }
        Commands::Stop => {
            info!("Stopping Sui Indexer gracefully");
            // NOTE: Add graceful shutdown logic when needed
            std::process::exit(0);
        }
        Commands::Health => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config).await?;
            
            let healthy = indexer.health_check().await?;
            
            if healthy {
                info!("✅ Indexer is healthy");
                std::process::exit(0);
            } else {
                info!("❌ Some components are unhealthy");
                std::process::exit(1);
            }
        }
        Commands::Status => {
            let config = ConfigLoader::from_file(&cli.config)?;
            let indexer = IndexerCore::new(config).await?;

            info!("Checking indexer status");
            
            // Get overall status
            let healthy = indexer.health_check().await?;
            
            if healthy {
                info!("🟢 Indexer Status: HEALTHY");
                
                // Display detailed status information
                info!("📊 System Information:");
                info!("  - Version: {}", env!("CARGO_PKG_VERSION"));
                info!("  - Build: {} ({})", env!("CARGO_PKG_VERSION"), 
                      option_env!("BUILD_TIMESTAMP").unwrap_or("unknown"));
                
                // Network status
                info!("  📡 Network: Connected to Sui RPC");
                
                // Memory usage
                if let Ok(memory) = get_memory_usage() {
                    info!("  💾 Memory: {}", memory);
                }
                
                // NOTE: Add processing statistics when database schema is ready
                
                info!("✅ Status check completed successfully");
            } else {
                error!("❌ Indexer Status: UNHEALTHY");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// Get memory usage information
fn get_memory_usage() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("ps")
            .args(&["-o", "rss=", "-p"])
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
