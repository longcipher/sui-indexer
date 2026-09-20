/// PostgreSQL storage backend implementation
use async_trait::async_trait;
use eyre::Result;
use sqlx::{PgPool, Row};
use sui_indexer_config::DatabaseConfig;
use sui_indexer_events::{ProcessedEvent, ProcessedTransaction};
use tracing::{error, info};

use crate::{
    CanonicalEventModel, CheckpointModel, CoinFlowModel, EventQueryFilter, GatewayResult,
    IndexerProgressModel, ObjectModel, RepairQueueEntry, Storage, TableCounts, TransactionModel,
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
        let options = config.url.parse::<sqlx::postgres::PgConnectOptions>()?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(std::time::Duration::from_secs(
                config.connect_timeout.max(1),
            ))
            .idle_timeout(config.idle_timeout.map(std::time::Duration::from_secs))
            .connect_with(options)
            .await?;

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
                id, digest, checkpoint_sequence, timestamp, sender,
                gas_used, gas_price, success, error_message, effects
            ) ",
        );
        query_builder.push_values(transactions, |mut b, transaction| {
            b.push_bind(transaction.id)
                .push_bind(transaction.digest)
                .push_bind(transaction.checkpoint_sequence)
                .push_bind(transaction.created_at)
                .push_bind(transaction.sender)
                .push_bind(transaction.gas_used)
                .push_bind(transaction.gas_price)
                .push_bind(transaction.success)
                .push_bind(transaction.error_message)
                .push_bind(serde_json::Value::Null);
        });
        query_builder.push(
            " ON CONFLICT (digest) DO UPDATE SET
                checkpoint_sequence = EXCLUDED.checkpoint_sequence,
                sender = EXCLUDED.sender,
                gas_used = EXCLUDED.gas_used,
                gas_price = EXCLUDED.gas_price,
                success = EXCLUDED.success,
                error_message = EXCLUDED.error_message",
        );

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
                sequence_number, digest, prev_digest, epoch, timestamp_ms,
                transaction_count, network_total_transactions,
                validator_signature, end_of_epoch_data, updated_at
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
             ON CONFLICT (sequence_number)
             DO UPDATE SET
                digest = EXCLUDED.digest,
                prev_digest = EXCLUDED.prev_digest,
                epoch = EXCLUDED.epoch,
                timestamp_ms = EXCLUDED.timestamp_ms,
                transaction_count = EXCLUDED.transaction_count,
                network_total_transactions = EXCLUDED.network_total_transactions,
                validator_signature = EXCLUDED.validator_signature,
                end_of_epoch_data = EXCLUDED.end_of_epoch_data,
                updated_at = NOW()",
        )
        .bind(checkpoint.sequence_number)
        .bind(checkpoint.digest)
        .bind(checkpoint.prev_digest)
        .bind(checkpoint.epoch)
        .bind(checkpoint.timestamp_ms)
        .bind(checkpoint.transaction_count)
        .bind(checkpoint.network_total_transactions)
        .bind(checkpoint.validator_signature)
        .bind(checkpoint.end_of_epoch_data)
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

    async fn sql_query(&self, sql: &str, limit: u64, max_bytes: usize) -> Result<GatewayResult> {
        use sqlx::Column;
        // Gateway SQL is validated SELECT-only upstream (allowlist tables, no
        // multi-statements); assert safety explicitly for sqlx 0.9.
        let owned = sqlx::AssertSqlSafe(sql.to_string());
        let rows = sqlx::query(owned)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| sanitize_sql_error(&e))?;
        let mut result = GatewayResult {
            columns: Vec::new(),
            rows: Vec::with_capacity(rows.len().min(limit.max(1) as usize)),
            row_count: 0,
            truncated: false,
        };
        let mut bytes = 0_usize;
        for (index, row) in rows.iter().enumerate() {
            if index == 0 {
                result.columns = row
                    .columns()
                    .iter()
                    .map(|column| column.name().to_string())
                    .collect();
            }
            if index as u64 >= limit.max(1) {
                result.truncated = true;
                break;
            }
            let mut object = serde_json::Map::new();
            for column in row.columns() {
                let type_name = column.type_info().to_string();
                let value: serde_json::Value = if type_name.contains("INT8")
                    || type_name.contains("INT4")
                    || type_name.contains("INT2")
                {
                    row.try_get::<i64, _>(column.name())
                        .or_else(|_| row.try_get::<i32, _>(column.name()).map(i64::from))
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("BOOL") {
                    row.try_get::<bool, _>(column.name())
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("NUMERIC") {
                    row.try_get::<String, _>(column.name())
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("JSON") {
                    row.try_get::<serde_json::Value, _>(column.name())
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("BYTEA") {
                    row.try_get::<Vec<u8>, _>(column.name())
                        .map(|v| serde_json::json!(hex::encode(v)))
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("TIMESTAMPTZ") || type_name.contains("TIMESTAMP") {
                    row.try_get::<chrono::DateTime<chrono::Utc>, _>(column.name())
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                } else if type_name.contains("UUID") {
                    row.try_get::<uuid::Uuid, _>(column.name())
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    row.try_get::<String, _>(column.name())
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null)
                };
                object.insert(column.name().to_string(), value);
            }
            let line = serde_json::Value::Object(object);
            bytes += line.to_string().len();
            if bytes > max_bytes.max(1024) {
                result.truncated = true;
                break;
            }
            result.rows.push(line);
        }
        result.row_count = result.rows.len();
        Ok(result)
    }

    async fn repair_derived_insights(&self) -> Result<()> {
        self.refresh_coin_insights().await
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

    async fn store_canonical_events(&self, events: Vec<CanonicalEventModel>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO events_v2 (
                id, checkpoint_sequence, transaction_digest, event_index,
                package_id, module_name, event_type, sender, timestamp_ms,
                bcs, fields
            ) ",
        );
        query_builder.push_values(events, |mut b, event| {
            b.push_bind(event.id)
                .push_bind(event.checkpoint_sequence)
                .push_bind(event.transaction_digest)
                .push_bind(event.event_index)
                .push_bind(event.package_id)
                .push_bind(event.module_name)
                .push_bind(event.event_type)
                .push_bind(event.sender)
                .push_bind(event.timestamp_ms)
                .push_bind(event.bcs)
                .push_bind(event.fields);
        });
        query_builder.push(" ON CONFLICT (transaction_digest, event_index) DO NOTHING");

        query_builder.build().execute(&self.pool).await?;
        Ok(())
    }

    async fn store_coin_flows(&self, flows: Vec<CoinFlowModel>) -> Result<()> {
        if flows.is_empty() {
            return Ok(());
        }

        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO coin_flows (
                checkpoint_sequence, timestamp_ms, transaction_digest,
                coin_type, holder, object_id, version, balance
            ) ",
        );
        query_builder.push_values(flows, |mut b, flow| {
            b.push_bind(flow.checkpoint_sequence)
                .push_bind(flow.timestamp_ms)
                .push_bind(flow.transaction_digest)
                .push_bind(flow.coin_type)
                .push_bind(flow.holder)
                .push_bind(flow.object_id)
                .push_bind(flow.version)
                .push_bind(balance_to_numeric(&flow.balance));
        });
        query_builder.push(
            " ON CONFLICT (object_id, version) DO UPDATE SET
                balance = EXCLUDED.balance,
                checkpoint_sequence = EXCLUDED.checkpoint_sequence",
        );

        query_builder.build().execute(&self.pool).await?;
        self.refresh_coin_insights().await?;
        Ok(())
    }

    async fn claim_repair_entries(&self, limit: usize) -> Result<Vec<RepairQueueEntry>> {
        let entries = sqlx::query_as::<_, RepairQueueEntry>(
            "SELECT checkpoint_sequence, attempts, next_retry_at, last_error,
                    parked, created_at, updated_at
             FROM repair_queue
             WHERE parked = FALSE AND next_retry_at <= NOW()
             ORDER BY checkpoint_sequence
             LIMIT $1
             FOR UPDATE SKIP LOCKED",
        )
        .bind(limit.max(1) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(entries)
    }

    async fn enqueue_repair(&self, checkpoint: u64, error: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO repair_queue (checkpoint_sequence, last_error, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT (checkpoint_sequence)
             DO UPDATE SET last_error = EXCLUDED.last_error, updated_at = NOW()",
        )
        .bind(checkpoint as i64)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn complete_repair(
        &self,
        checkpoint: u64,
        success: bool,
        error: Option<&str>,
        max_attempts: i32,
        backoff_secs: u64,
    ) -> Result<()> {
        if success {
            sqlx::query("DELETE FROM repair_queue WHERE checkpoint_sequence = $1")
                .bind(checkpoint as i64)
                .execute(&self.pool)
                .await?;
            return Ok(());
        }
        sqlx::query(
            "UPDATE repair_queue
             SET attempts = attempts + 1,
                 last_error = COALESCE($2, last_error),
                 next_retry_at = NOW() + make_interval(secs => $3),
                 parked = (attempts + 1) >= $4,
                 updated_at = NOW()
             WHERE checkpoint_sequence = $1",
        )
        .bind(checkpoint as i64)
        .bind(error)
        .bind(backoff_secs as f64)
        .bind(max_attempts.max(1))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_progress(&self, pipeline: &str) -> Result<Option<IndexerProgressModel>> {
        let progress = sqlx::query_as::<_, IndexerProgressModel>(
            "SELECT pipeline, continuous_checkpoint, floor_checkpoint, archive_lo,
                    archive_hi, hot_boundary, hot_boundary_ts, digest, updated_at
             FROM indexer_progress WHERE pipeline = $1",
        )
        .bind(pipeline)
        .fetch_optional(&self.pool)
        .await?;
        Ok(progress)
    }

    async fn advance_continuous(
        &self,
        pipeline: &str,
        continuous: u64,
        floor: u64,
        digest: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO indexer_progress (
                pipeline, continuous_checkpoint, floor_checkpoint, digest, updated_at
             )
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT (pipeline)
             DO UPDATE SET
                continuous_checkpoint = GREATEST(indexer_progress.continuous_checkpoint, EXCLUDED.continuous_checkpoint),
                floor_checkpoint = GREATEST(indexer_progress.floor_checkpoint, EXCLUDED.floor_checkpoint),
                digest = COALESCE(EXCLUDED.digest, indexer_progress.digest),
                updated_at = NOW()",
        )
        .bind(pipeline)
        .bind(continuous as i64)
        .bind(floor as i64)
        .bind(digest)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_archive_window(
        &self,
        pipeline: &str,
        archive_lo: Option<u64>,
        archive_hi: Option<u64>,
        hot_boundary: Option<u64>,
    ) -> Result<()> {
        let to_i64 = |value: Option<u64>| value.map(|v| v as i64);
        sqlx::query(
            "INSERT INTO indexer_progress (
                pipeline, continuous_checkpoint, floor_checkpoint,
                archive_lo, archive_hi, hot_boundary, updated_at
             )
             VALUES ($1, 0, 0, $2, $3, $4, NOW())
             ON CONFLICT (pipeline)
             DO UPDATE SET
                archive_lo = COALESCE(EXCLUDED.archive_lo, indexer_progress.archive_lo),
                archive_hi = CASE
                    WHEN EXCLUDED.archive_hi IS NULL THEN indexer_progress.archive_hi
                    ELSE GREATEST(COALESCE(indexer_progress.archive_hi, 0), EXCLUDED.archive_hi)
                END,
                hot_boundary = COALESCE(EXCLUDED.hot_boundary, indexer_progress.hot_boundary),
                updated_at = NOW()",
        )
        .bind(pipeline)
        .bind(to_i64(archive_lo))
        .bind(to_i64(archive_hi))
        .bind(to_i64(hot_boundary))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn detect_gaps(&self, floor: u64, tip: u64) -> Result<Vec<(u64, u64)>> {
        if floor > tip {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "WITH wanted AS (
                SELECT generate_series($1, $2) AS seq
             ),
             missing AS (
                SELECT seq FROM wanted
                WHERE NOT EXISTS (
                    SELECT 1 FROM checkpoints WHERE sequence_number = seq
                )
             ),
             grouped AS (
                SELECT seq,
                       seq - ROW_NUMBER() OVER (ORDER BY seq) AS grp
                FROM missing
             )
             SELECT MIN(seq) AS gap_start, MAX(seq) AS gap_end
             FROM grouped GROUP BY grp ORDER BY gap_start DESC",
        )
        .bind(floor as i64)
        .bind(tip as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                (
                    row.get::<i64, _>("gap_start") as u64,
                    row.get::<i64, _>("gap_end") as u64,
                )
            })
            .collect())
    }

    async fn checkpoint_digests(&self, start: u64, end: u64) -> Result<Vec<(u64, String)>> {
        let rows = sqlx::query(
            "SELECT sequence_number, digest FROM checkpoints
             WHERE sequence_number >= $1 AND sequence_number <= $2
             ORDER BY sequence_number",
        )
        .bind(start as i64)
        .bind(end as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                (
                    row.get::<i64, _>("sequence_number") as u64,
                    row.get::<String, _>("digest"),
                )
            })
            .collect())
    }

    async fn table_counts(&self) -> Result<TableCounts> {
        let checkpoints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checkpoints")
            .fetch_one(&self.pool)
            .await?;
        let transactions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions")
            .fetch_one(&self.pool)
            .await?;
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events_v2")
            .fetch_one(&self.pool)
            .await?;
        let objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM objects")
            .fetch_one(&self.pool)
            .await?;
        Ok(TableCounts {
            checkpoints: checkpoints.max(0) as u64,
            transactions: transactions.max(0) as u64,
            events: events.max(0) as u64,
            objects: objects.max(0) as u64,
        })
    }
}

impl PostgresStorage {
    /// Refresh balance snapshots and coin metadata from fresh flows.
    async fn refresh_coin_insights(&self) -> Result<()> {
        sqlx::query(
            "INSERT INTO balance_snapshots (holder, coin_type, balance, checkpoint_sequence, updated_at)
             SELECT holder, coin_type, SUM(balance), MAX(checkpoint_sequence), NOW()
             FROM coin_flows
             GROUP BY holder, coin_type
             ON CONFLICT (holder, coin_type)
             DO UPDATE SET
                balance = EXCLUDED.balance,
                checkpoint_sequence = EXCLUDED.checkpoint_sequence,
                updated_at = NOW()",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO coin_metadata (coin_type, first_seen_checkpoint, last_seen_checkpoint, flow_count, updated_at)
             SELECT coin_type, MIN(checkpoint_sequence), MAX(checkpoint_sequence), COUNT(*), NOW()
             FROM coin_flows
             GROUP BY coin_type
             ON CONFLICT (coin_type)
             DO UPDATE SET
                first_seen_checkpoint = LEAST(coin_metadata.first_seen_checkpoint, EXCLUDED.first_seen_checkpoint),
                last_seen_checkpoint = GREATEST(coin_metadata.last_seen_checkpoint, EXCLUDED.last_seen_checkpoint),
                flow_count = EXCLUDED.flow_count,
                updated_at = NOW()",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Convert a balance JSON value into a NUMERIC(78,0)-compatible string.
fn balance_to_numeric(balance: &serde_json::Value) -> String {
    match balance {
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => {
            let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                "0".to_string()
            } else {
                digits
            }
        }
        _ => "0".to_string(),
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

/// Redact connection details from database errors before surfacing them.
fn sanitize_sql_error(error: &sqlx::Error) -> eyre::Error {
    let mut message = error.to_string();
    for secret in ["postgres://", "password=", "@localhost", "@127.0.0.1"] {
        message = message.replace(secret, "[redacted]");
    }
    if message.len() > 500 {
        message.truncate(500);
    }
    eyre::eyre!("query failed: {message}")
}
