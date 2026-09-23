//! Skeleton writers: chain-neutral `blocks` / `txs` / `chain_events`.
//!
//! The adapter's contract is decoded rows; this module lands them with the
//! same idempotency discipline as the canonical tables (`ON CONFLICT DO
//! NOTHING`). Reorg cleanup deletes everything above the fork point for
//! block-scoped tables.

use chain_core::DecodedBlock;
use sqlx::PgPool;

/// Rows written by one [`store_decoded_block`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkeletonCounts {
    /// Skeleton block rows (0 for skipped markers).
    pub blocks: u64,
    /// Skeleton transaction rows.
    pub txs: u64,
    /// Universal event rows.
    pub events: u64,
}

/// Store one decoded height. Skipped-slot markers record a `skipped` block
/// row and nothing else (normal data, never retried).
pub async fn store_decoded_block(
    pool: &PgPool,
    chain_id: &str,
    block: &DecodedBlock,
) -> eyre::Result<SkeletonCounts> {
    if block.skipped {
        sqlx::query(
            "INSERT INTO blocks (chain_id, height, hash, ts, commitment, skipped)
             VALUES ($1, $2, '\\x', NOW(), 2, TRUE)
             ON CONFLICT (chain_id, height) DO NOTHING",
        )
        .bind(chain_id)
        .bind(block.height as i64)
        .execute(pool)
        .await?;
        return Ok(SkeletonCounts::default());
    }

    let mut counts = SkeletonCounts::default();
    if let Some(row) = block.rows.block.as_ref() {
        let result = sqlx::query(
            "INSERT INTO blocks
               (chain_id, height, hash, parent_hash, parent_ref, ts,
                commitment, skipped, chain_meta)
             VALUES ($1,$2,$3,$4,$5,$6,$7,FALSE,$8)
             ON CONFLICT (chain_id, height) DO NOTHING",
        )
        .bind(chain_id)
        .bind(row.height as i64)
        .bind(row.hash.clone())
        .bind(row.parent_hash.clone())
        .bind(row.parent_ref.clone())
        .bind(row.ts)
        .bind(row.commitment)
        .bind(row.chain_meta.clone())
        .execute(pool)
        .await?;
        counts.blocks = result.rows_affected();
    }

    if !block.rows.txs.is_empty() {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO txs
               (chain_id, height, block_ts, tx_index, tx_hash, sender,
                success, fee, chain_meta) ",
        );
        builder.push_values(block.rows.txs.iter(), |mut b, tx| {
            b.push_bind(chain_id)
                .push_bind(tx.height as i64)
                .push_bind(tx.block_ts)
                .push_bind(tx.tx_index as i32)
                .push_bind(tx.tx_hash.clone())
                .push_bind(tx.sender.clone())
                .push_bind(tx.success)
                .push_bind(tx.fee.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
                .push_bind(tx.chain_meta.clone());
        });
        builder.push(" ON CONFLICT (chain_id, height, tx_index) DO NOTHING");
        counts.txs = builder.build().execute(pool).await?.rows_affected();
    }

    if !block.events.is_empty() {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO chain_events
               (chain_id, height, block_ts, tx_index, ev_index, inner_ix,
                stack_height, emitter, topics, payload, tx_hash, sender, extra) ",
        );
        builder.push_values(block.events.iter(), |mut b, ev| {
            b.push_bind(chain_id)
                .push_bind(ev.height as i64)
                .push_bind(ev.block_ts)
                .push_bind(ev.tx_index as i32)
                .push_bind(ev.ev_index as i32)
                .push_bind(ev.inner_ix as i32)
                .push_bind(ev.stack_height as i32)
                .push_bind(ev.emitter.clone())
                .push_bind(ev.topics.clone())
                .push_bind(ev.payload.clone())
                .push_bind(ev.tx_hash.clone())
                .push_bind(ev.sender.clone())
                .push_bind(ev.extra.clone());
        });
        builder.push(" ON CONFLICT (chain_id, height, tx_index, ev_index, inner_ix) DO NOTHING");
        counts.events = builder.build().execute(pool).await?.rows_affected();
    }
    Ok(counts)
}

/// Build the reorg-prune statement for one table: everything strictly above
/// `height` goes. Table and column names are allow-listed first.
pub fn prune_sql(table: &str, block_column: &str) -> Result<String, String> {
    validate_sql_name(table)?;
    validate_sql_name(block_column)?;
    Ok(format!("DELETE FROM {table} WHERE {block_column} > $1"))
}

/// Allow-list for interpolated SQL names (mirrors the job-engine guard).
pub(crate) fn validate_sql_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("invalid SQL name: {name}"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(format!("invalid SQL name: {name}"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("invalid SQL name: {name}"));
    }
    Ok(())
}

/// Prune skeleton rows above `height` plus every block-scoped catalog table.
///
/// `catalog` carries `(table, block_column)` for job outputs with
/// `reorg_mode = "block_scoped"`. Refreshable outputs are recomputed, not
/// pruned. Returns total rows removed.
pub async fn prune_above(
    pool: &PgPool,
    chain_id: &str,
    height: u64,
    catalog: &[(String, String)],
) -> eyre::Result<u64> {
    let mut removed = 0u64;
    // Fixed table set: literal SQL, no interpolation.
    let result = sqlx::query("DELETE FROM chain_events WHERE chain_id = $1 AND height > $2")
        .bind(chain_id)
        .bind(height as i64)
        .execute(pool)
        .await?;
    removed += result.rows_affected();
    let result = sqlx::query("DELETE FROM txs WHERE chain_id = $1 AND height > $2")
        .bind(chain_id)
        .bind(height as i64)
        .execute(pool)
        .await?;
    removed += result.rows_affected();
    let result = sqlx::query("DELETE FROM blocks WHERE chain_id = $1 AND height > $2")
        .bind(chain_id)
        .bind(height as i64)
        .execute(pool)
        .await?;
    removed += result.rows_affected();
    for (table, column) in catalog {
        let sql = prune_sql(table, column).map_err(|e| eyre::eyre!("{e}"))?;
        // Audited: names passed the allow-list above; height is an integer bind.
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(height as i64)
            .execute(pool)
            .await?;
        removed += result.rows_affected();
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_sql_targets_rows_above_height() {
        assert_eq!(
            prune_sql("job_sandwich__v3", "_height").expect("sql"),
            "DELETE FROM job_sandwich__v3 WHERE _height > $1"
        );
    }

    #[test]
    fn prune_sql_rejects_injection() {
        assert!(prune_sql("t; DROP TABLE t; --", "_height").is_err());
        assert!(prune_sql("t", "_height OR TRUE --").is_err());
        assert!(prune_sql("", "_height").is_err());
        assert!(prune_sql("1t", "_height").is_err());
    }

    #[test]
    fn sql_name_length_boundary_is_64() {
        let ok = "a".repeat(64);
        assert!(prune_sql(&ok, "_height").is_ok());
        assert!(prune_sql(&"a".repeat(65), "_height").is_err());
    }
}
