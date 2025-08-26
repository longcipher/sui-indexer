/// PostgreSQL storage backend implementation
use async_trait::async_trait;
use eyre::Result;
use sqlx::{PgPool, Row};
use sui_indexer_config::DatabaseConfig;
use sui_indexer_events::{ProcessedEvent, ProcessedTransaction};
use tracing::{error, info};

use crate::Storage;

/// PostgreSQL storage implementation
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Create a new PostgreSQL storage backend
    pub async fn new(config: DatabaseConfig) -> Result<Self> {
        let pool = PgPool::connect(&config.url).await?;

        Ok(Self { pool })
    }

    /// Get the database pool
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn initialize(&self) -> Result<()> {
        info!("Initializing PostgreSQL storage backend");

        // Run migrations to create tables
        crate::migrations::run_migrations(&self.pool).await?;

        Ok(())
    }

    async fn store_events(&self, events: Vec<ProcessedEvent>) -> Result<()> {
        info!("Storing {} events", events.len());

        if events.is_empty() {
            return Ok(());
        }

        // Batch insert events
        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO processed_events (
                id, event_data, transaction_digest, checkpoint_sequence,
                timestamp, package_id, module_name, event_type,
                sender, fields, metadata, processed_at
            ) ",
        );

        query_builder.push_values(events, |mut b, event| {
            b.push_bind(event.id)
                .push_bind(
                    serde_json::to_value(&event.event).expect("Event should serialize to JSON"),
                )
                .push_bind(event.transaction_digest.to_string())
                .push_bind(event.checkpoint_sequence as i64)
                .push_bind(event.timestamp)
                .push_bind(event.package_id.to_string())
                .push_bind(event.module_name.clone())
                .push_bind(event.event_type.clone())
                .push_bind(event.sender.clone())
                .push_bind(event.fields.clone())
                .push_bind(
                    serde_json::to_value(&event.metadata)
                        .expect("Event metadata should serialize to JSON"),
                )
                .push_bind(event.metadata.processed_at);
        });

        let query = query_builder.build();
        query.execute(&self.pool).await?;

        Ok(())
    }

    async fn store_transactions(&self, transactions: Vec<ProcessedTransaction>) -> Result<()> {
        info!("Storing {} transactions", transactions.len());

        if transactions.is_empty() {
            return Ok(());
        }

        // Batch insert transactions
        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO processed_transactions (
                id, transaction_data, digest, checkpoint_sequence,
                timestamp, sender, gas_used, status, effects,
                metadata, processed_at
            ) ",
        );

        query_builder.push_values(transactions, |mut b, tx| {
            b.push_bind(tx.id)
                .push_bind(
                    serde_json::to_value(&tx.transaction)
                        .expect("Transaction should serialize to JSON"),
                )
                .push_bind(tx.transaction.digest.to_string())
                .push_bind(tx.checkpoint_sequence as i64)
                .push_bind(tx.timestamp)
                .push_bind("0x0".to_string()) // Placeholder for sender - would need proper extraction
                .push_bind(tx.metadata.gas_used.unwrap_or(0) as i64)
                .push_bind(tx.metadata.success.to_string())
                .push_bind(
                    serde_json::to_value(&tx.transaction.effects)
                        .expect("Transaction effects should serialize to JSON"),
                )
                .push_bind(
                    serde_json::to_value(&tx.metadata)
                        .expect("Transaction metadata should serialize to JSON"),
                )
                .push_bind(tx.metadata.processed_at);
        });

        let query = query_builder.build();
        query.execute(&self.pool).await?;

        Ok(())
    }

    async fn get_events_by_checkpoint_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<ProcessedEvent>> {
        info!("Getting events for checkpoint range {}-{}", start, end);

        let rows = sqlx::query(
            "SELECT id, event_data, transaction_digest, checkpoint_sequence,
                    timestamp, package_id, module_name, event_type,
                    sender, fields, metadata, processed_at
             FROM processed_events 
             WHERE checkpoint_sequence >= $1 AND checkpoint_sequence <= $2
             ORDER BY checkpoint_sequence, processed_at",
        )
        .bind(start as i64)
        .bind(end as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::new();
        for row in rows {
            let event = ProcessedEvent {
                id: row.get("id"),
                event: serde_json::from_value(row.get("event_data"))?,
                transaction_digest: row
                    .get::<String, _>("transaction_digest")
                    .parse()
                    .map_err(|e| eyre::eyre!("Failed to parse transaction digest: {}", e))?,
                checkpoint_sequence: row.get::<i64, _>("checkpoint_sequence") as u64,
                timestamp: row.get("timestamp"),
                package_id: row
                    .get::<String, _>("package_id")
                    .parse()
                    .map_err(|e| eyre::eyre!("Failed to parse package ID: {}", e))?,
                module_name: row.get("module_name"),
                event_type: row.get("event_type"),
                sender: row.get("sender"),
                fields: row.get("fields"),
                metadata: serde_json::from_value(row.get("metadata"))?,
            };
            events.push(event);
        }

        Ok(events)
    }

    async fn get_latest_checkpoint(&self) -> Result<Option<u64>> {
        let row =
            sqlx::query("SELECT checkpoint_sequence FROM indexer_state ORDER BY id DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;

        if let Some(row) = row {
            Ok(Some(row.get::<i64, _>("checkpoint_sequence") as u64))
        } else {
            Ok(None)
        }
    }

    async fn update_checkpoint_progress(&self, checkpoint: u64) -> Result<()> {
        info!("Updating checkpoint progress to {}", checkpoint);

        sqlx::query(
            "INSERT INTO indexer_state (checkpoint_sequence, updated_at) 
             VALUES ($1, NOW())
             ON CONFLICT (id) 
             DO UPDATE SET checkpoint_sequence = $1, updated_at = NOW()",
        )
        .bind(checkpoint as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => Ok(true),
            Err(err) => {
                error!("Database health check failed: {}", err);
                Ok(false)
            }
        }
    }
}
