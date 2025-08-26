/// Database migrations using sqlx migrate functionality
use eyre::Result;
use tracing::info;

/// Run database migrations using sqlx migrate
pub async fn run_migrations(pool: &sqlx::PgPool) -> Result<()> {
    info!("Running database migrations using sqlx migrate");

    // Run all pending migrations from the migrations directory within this crate
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| eyre::eyre!("Failed to run migrations: {}", e))?;

    info!("Database migrations completed successfully");
    Ok(())
}
