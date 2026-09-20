/// PostgreSQL storage backend implementation
use async_trait::async_trait;
use eyre::Result;
use sqlx::{PgPool, Row};
use sui_indexer_config::DatabaseConfig;
use sui_indexer_events::{ProcessedEvent, ProcessedTransaction};
use tracing::{error, info};

use crate::{
    CheckpointModel, EventQueryFilter, ObjectModel, Storage, TransactionModel,
    TransactionQueryFilter, WatermarkModel,
};

/// Name of the default pipeline watermark row.
pub const DEFAULT_PIPELINE: &str = "default";

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
        if events.is_empty() {
            return Ok(());
        }

        // Batch insert events with idempotent replay semantics.
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
        query_builder.push(" ON CONFLICT DO NOTHING");

        let query = query_builder.build();
        query.execute(&self.pool).await?;

        Ok(())
    }

    async fn store_transactions(&self, transactions: Vec<ProcessedTransaction>) -> Result<()> {
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
        query_builder.push(" ON CONFLICT (digest) DO NOTHING");

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
        let watermark: Option<WatermarkModel> = sqlx::query_as(
            "SELECT pipeline, epoch_hi_inclusive, checkpoint_hi_inclusive, tx_hi,
                    timestamp_ms_hi_inclusive, reader_lo, pruner_hi, pruner_timestamp, updated_at
             FROM pipeline_watermarks WHERE pipeline = 'default'",
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(watermark) = watermark {
            return Ok((watermark.checkpoint_hi_inclusive > 0)
                .then_some(watermark.checkpoint_hi_inclusive as u64));
        }

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
        sqlx::query(
            "INSERT INTO pipeline_watermarks (
                pipeline, epoch_hi_inclusive, checkpoint_hi_inclusive, tx_hi,
                timestamp_ms_hi_inclusive, reader_lo, pruner_hi, updated_at
             )
             VALUES ('default', 0, $1, 0, 0, $1, 0, NOW())
             ON CONFLICT (pipeline)
             DO UPDATE SET
                checkpoint_hi_inclusive = GREATEST(pipeline_watermarks.checkpoint_hi_inclusive, EXCLUDED.checkpoint_hi_inclusive),
                reader_lo = GREATEST(pipeline_watermarks.reader_lo, EXCLUDED.reader_lo),
                updated_at = NOW()",
        )
        .bind(checkpoint as i64)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "INSERT INTO indexer_state (checkpoint_sequence, updated_at)
             VALUES ($1, NOW())
             ON CONFLICT (id)
             DO UPDATE SET checkpoint_sequence = GREATEST(indexer_state.checkpoint_sequence, EXCLUDED.checkpoint_sequence), updated_at = NOW()",
        )
        .bind(checkpoint as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn store_transaction_models(&self, transactions: Vec<TransactionModel>) -> Result<()> {
        if transactions.is_empty() {
            return Ok(());
        }

        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO transactions (
                id, digest, checkpoint_sequence, timestamp, gas_used, success
            ) ",
        );
        query_builder.push_values(transactions, |mut b, transaction| {
            b.push_bind(transaction.id)
                .push_bind(transaction.digest)
                .push_bind(transaction.checkpoint_sequence)
                .push_bind(transaction.created_at)
                .push_bind(transaction.gas_used)
                .push_bind(transaction.success);
        });
        query_builder.push(" ON CONFLICT (digest) DO NOTHING");

        query_builder.build().execute(&self.pool).await?;
        Ok(())
    }

    async fn store_object_models(&self, objects: Vec<ObjectModel>) -> Result<()> {
        if objects.is_empty() {
            return Ok(());
        }

        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO objects (
                id, object_id, version, digest, checkpoint_sequence,
                transaction_digest, sender
            ) ",
        );
        query_builder.push_values(objects, |mut b, object| {
            b.push_bind(object.id)
                .push_bind(object.object_id)
                .push_bind(object.version)
                .push_bind(object.digest)
                .push_bind(object.checkpoint_sequence)
                .push_bind(object.transaction_digest)
                .push_bind(object.sender);
        });
        query_builder.push(" ON CONFLICT (object_id, version) DO NOTHING");

        query_builder.build().execute(&self.pool).await?;
        Ok(())
    }

    async fn store_checkpoint_model(&self, checkpoint: CheckpointModel) -> Result<()> {
        sqlx::query(
            "INSERT INTO checkpoints (
                sequence_number, digest, epoch, timestamp_ms,
                transaction_count, network_total_transactions
             )
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (sequence_number)
             DO UPDATE SET
                digest = EXCLUDED.digest,
                epoch = EXCLUDED.epoch,
                timestamp_ms = EXCLUDED.timestamp_ms,
                transaction_count = EXCLUDED.transaction_count,
                network_total_transactions = EXCLUDED.network_total_transactions",
        )
        .bind(checkpoint.sequence_number)
        .bind(checkpoint.digest)
        .bind(checkpoint.epoch)
        .bind(checkpoint.timestamp_ms)
        .bind(checkpoint.transaction_count)
        .bind(checkpoint.network_total_transactions)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_watermark(&self, pipeline: &str) -> Result<Option<WatermarkModel>> {
        let watermark: Option<WatermarkModel> = sqlx::query_as(
            "SELECT pipeline, epoch_hi_inclusive, checkpoint_hi_inclusive, tx_hi,
                    timestamp_ms_hi_inclusive, reader_lo, pruner_hi, pruner_timestamp, updated_at
             FROM pipeline_watermarks WHERE pipeline = $1",
        )
        .bind(pipeline)
        .fetch_optional(&self.pool)
        .await?;

        Ok(watermark)
    }

    async fn set_watermark(&self, watermark: WatermarkModel) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO pipeline_watermarks (
                pipeline, epoch_hi_inclusive, checkpoint_hi_inclusive, tx_hi,
                timestamp_ms_hi_inclusive, reader_lo, pruner_hi, pruner_timestamp, updated_at
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
             ON CONFLICT (pipeline)
             DO UPDATE SET
                epoch_hi_inclusive = GREATEST(pipeline_watermarks.epoch_hi_inclusive, EXCLUDED.epoch_hi_inclusive),
                checkpoint_hi_inclusive = GREATEST(pipeline_watermarks.checkpoint_hi_inclusive, EXCLUDED.checkpoint_hi_inclusive),
                tx_hi = GREATEST(pipeline_watermarks.tx_hi, EXCLUDED.tx_hi),
                timestamp_ms_hi_inclusive = GREATEST(pipeline_watermarks.timestamp_ms_hi_inclusive, EXCLUDED.timestamp_ms_hi_inclusive),
                reader_lo = GREATEST(pipeline_watermarks.reader_lo, EXCLUDED.reader_lo),
                pruner_hi = GREATEST(pipeline_watermarks.pruner_hi, EXCLUDED.pruner_hi),
                updated_at = NOW()
             RETURNING pipeline",
        )
        .bind(watermark.pipeline)
        .bind(watermark.epoch_hi_inclusive)
        .bind(watermark.checkpoint_hi_inclusive)
        .bind(watermark.tx_hi)
        .bind(watermark.timestamp_ms_hi_inclusive)
        .bind(watermark.reader_lo)
        .bind(watermark.pruner_hi)
        .fetch_optional(&self.pool)
        .await?;

        Ok(result.is_some())
    }

    async fn prune_checkpoints(&self, latest: u64, retention: u64) -> Result<u64> {
        let cutoff = latest.saturating_sub(retention) as i64;
        if cutoff <= 0 {
            return Ok(0);
        }

        let mut pruned = 0_u64;
        for (table, column) in [
            ("processed_events", "checkpoint_sequence"),
            ("processed_transactions", "checkpoint_sequence"),
            ("transactions", "checkpoint_sequence"),
            ("objects", "checkpoint_sequence"),
        ] {
            let rows = match (table, column) {
                ("processed_events", _) => {
                    sqlx::query("DELETE FROM processed_events WHERE checkpoint_sequence < $1")
                        .bind(cutoff)
                        .execute(&self.pool)
                        .await?
                        .rows_affected()
                }
                ("processed_transactions", _) => {
                    sqlx::query("DELETE FROM processed_transactions WHERE checkpoint_sequence < $1")
                        .bind(cutoff)
                        .execute(&self.pool)
                        .await?
                        .rows_affected()
                }
                ("transactions", _) => {
                    sqlx::query("DELETE FROM transactions WHERE checkpoint_sequence < $1")
                        .bind(cutoff)
                        .execute(&self.pool)
                        .await?
                        .rows_affected()
                }
                _ => sqlx::query("DELETE FROM objects WHERE checkpoint_sequence < $1")
                    .bind(cutoff)
                    .execute(&self.pool)
                    .await?
                    .rows_affected(),
            };
            pruned = pruned.saturating_add(rows);
        }
        pruned = pruned.saturating_add(
            sqlx::query("DELETE FROM checkpoints WHERE sequence_number < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        );

        sqlx::query(
            "UPDATE pipeline_watermarks
             SET reader_lo = GREATEST(reader_lo, $1), pruner_hi = GREATEST(pruner_hi, $1),
                 pruner_timestamp = NOW(), updated_at = NOW()
             WHERE pipeline = 'default'",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;

        Ok(pruned)
    }

    async fn rewind_watermark(&self, pipeline: &str, checkpoint: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO pipeline_watermarks (pipeline, checkpoint_hi_inclusive, reader_lo, updated_at)
             VALUES ($1, $2, $2, NOW())
             ON CONFLICT (pipeline)
             DO UPDATE SET checkpoint_hi_inclusive = $2, reader_lo = $2, updated_at = NOW()",
        )
        .bind(pipeline)
        .bind(checkpoint as i64)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "UPDATE indexer_state SET checkpoint_sequence = $1, updated_at = NOW() WHERE id = 1",
        )
        .bind(checkpoint as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn query_events(&self, filter: EventQueryFilter<'_>) -> Result<Vec<ProcessedEvent>> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, event_data, transaction_digest, checkpoint_sequence,
                    timestamp, package_id, module_name, event_type,
                    sender, fields, metadata, processed_at
             FROM processed_events WHERE 1 = 1",
        );
        if let Some(package) = filter.package {
            builder.push(" AND package_id = ");
            builder.push_bind(package);
        }
        if let Some(module) = filter.module {
            builder.push(" AND module_name = ");
            builder.push_bind(module);
        }
        if let Some(event_type) = filter.event_type {
            builder.push(" AND event_type = ");
            builder.push_bind(event_type);
        }
        if let Some(sender) = filter.sender {
            builder.push(" AND sender = ");
            builder.push_bind(sender);
        }
        if let Some(from) = filter.from_checkpoint {
            builder.push(" AND checkpoint_sequence >= ");
            builder.push_bind(from as i64);
        }
        if let Some(to) = filter.to_checkpoint {
            builder.push(" AND checkpoint_sequence <= ");
            builder.push_bind(to as i64);
        }
        builder.push(" ORDER BY checkpoint_sequence DESC, processed_at DESC LIMIT ");
        builder.push_bind(filter.limit.min(1000) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter().map(processed_event_from_row).collect()
    }

    async fn query_transactions(
        &self,
        filter: TransactionQueryFilter<'_>,
    ) -> Result<Vec<ProcessedTransaction>> {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT id, transaction_data, digest, checkpoint_sequence,
                    timestamp, sender, gas_used, status, effects,
                    metadata, processed_at
             FROM processed_transactions WHERE 1 = 1",
        );
        if let Some(sender) = filter.sender {
            builder.push(" AND sender = ");
            builder.push_bind(sender);
        }
        if let Some(from) = filter.from_checkpoint {
            builder.push(" AND checkpoint_sequence >= ");
            builder.push_bind(from as i64);
        }
        if let Some(to) = filter.to_checkpoint {
            builder.push(" AND checkpoint_sequence <= ");
            builder.push_bind(to as i64);
        }
        builder.push(" ORDER BY checkpoint_sequence DESC, processed_at DESC LIMIT ");
        builder.push_bind(filter.limit.min(1000) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(processed_transaction_from_row)
            .collect()
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

/// Map a `processed_events` row into a `ProcessedEvent`.
fn processed_event_from_row(row: sqlx::postgres::PgRow) -> Result<ProcessedEvent> {
    Ok(ProcessedEvent {
        id: row.get("id"),
        event: serde_json::from_value(row.get("event_data"))?,
        transaction_digest: row
            .get::<String, _>("transaction_digest")
            .parse()
            .map_err(|e| eyre::eyre!("Failed to parse transaction digest: {e}"))?,
        checkpoint_sequence: row.get::<i64, _>("checkpoint_sequence") as u64,
        timestamp: row.get("timestamp"),
        package_id: row
            .get::<String, _>("package_id")
            .parse()
            .map_err(|e| eyre::eyre!("Failed to parse package ID: {e}"))?,
        module_name: row.get("module_name"),
        event_type: row.get("event_type"),
        sender: row.get("sender"),
        fields: row.get("fields"),
        metadata: serde_json::from_value(row.get("metadata"))?,
    })
}

/// Map a `processed_transactions` row into a `ProcessedTransaction`.
fn processed_transaction_from_row(row: sqlx::postgres::PgRow) -> Result<ProcessedTransaction> {
    Ok(ProcessedTransaction {
        id: row.get("id"),
        transaction: serde_json::from_value(row.get("transaction_data"))?,
        checkpoint_sequence: row.get::<i64, _>("checkpoint_sequence") as u64,
        timestamp: row.get("timestamp"),
        events: Vec::new(),
        metadata: serde_json::from_value(row.get("metadata"))?,
    })
}
