use eyre::Result;
use sui_indexer_config::RepairConfig;
use sui_indexer_storage::StorageManager;
use tracing::{info, warn};

/// Background repair worker: claims due checkpoints with SKIP LOCKED and
/// reprocesses them through the canonical pipeline with backoff.
pub struct RepairWorker {
    repair: RepairConfig,
}

impl RepairWorker {
    /// Create a worker from repair config.
    pub fn new(repair: RepairConfig) -> Self {
        Self { repair }
    }

    /// Claim due entries and reprocess them one by one.
    pub async fn tick(
        &self,
        storage: &StorageManager,
        reprocess: impl AsyncFn(u64) -> Result<bool>,
    ) -> Result<usize> {
        let entries = storage
            .claim_repair_entries(self.repair.batch_size.max(1))
            .await?;
        let mut completed = 0_usize;
        for entry in entries {
            let sequence = entry.checkpoint_sequence as u64;
            match reprocess(sequence).await {
                Ok(true) => {
                    storage
                        .complete_repair(sequence, true, None, self.repair.max_attempts, 0)
                        .await?;
                    completed = completed.saturating_add(1);
                    info!("Repair completed for checkpoint {sequence}");
                }
                Ok(false) => {
                    let backoff = self.backoff(entry.attempts);
                    storage
                        .complete_repair(
                            sequence,
                            false,
                            Some("reprocess reported no progress"),
                            self.repair.max_attempts,
                            backoff,
                        )
                        .await?;
                }
                Err(e) => {
                    let backoff = self.backoff(entry.attempts);
                    warn!("Repair failed for {sequence}: {e}");
                    storage
                        .complete_repair(
                            sequence,
                            false,
                            Some(&e.to_string()),
                            self.repair.max_attempts,
                            backoff,
                        )
                        .await?;
                }
            }
        }
        Ok(completed)
    }

    /// Exponential backoff in seconds, capped at the configured max.
    pub fn backoff(&self, attempts: i32) -> u64 {
        let shift = attempts.clamp(0, 16) as u32;
        let backoff = self
            .repair
            .backoff_base_secs
            .max(1)
            .saturating_mul(1_u64 << shift);
        backoff.min(self.repair.backoff_max_secs.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> sui_indexer_config::RepairConfig {
        sui_indexer_config::RepairConfig {
            enabled: true,
            poll_interval_secs: 5,
            batch_size: 10,
            max_attempts: 4,
            backoff_base_secs: 30,
            backoff_max_secs: 3600,
        }
    }

    #[test]
    fn backoff_doubles_until_cap() {
        let worker = RepairWorker::new(config());
        assert_eq!(worker.backoff(0), 30);
        assert_eq!(worker.backoff(1), 60);
        assert_eq!(worker.backoff(10), 3600);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    async fn live_storage() -> Option<StorageManager> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let db = sui_indexer_config::DatabaseConfig {
            url,
            max_connections: 2,
            min_connections: 1,
            connect_timeout: 10,
            idle_timeout: None,
            auto_migrate: false,
        };
        let storage = StorageManager::new_postgres(db).await.ok()?;
        storage.initialize().await.ok()?;
        Some(storage)
    }

    #[tokio::test]
    async fn tick_completes_fails_and_errors() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let worker = RepairWorker::new(sui_indexer_config::RepairConfig {
            enabled: true,
            poll_interval_secs: 1,
            batch_size: 10,
            max_attempts: 3,
            backoff_base_secs: 1,
            backoff_max_secs: 60,
        });
        let base = 8_000_000 + (std::process::id() as u64 % 1000);
        // Drain first: entries from previous runs may have matured.
        let peeked: Vec<i64> = storage
            .claim_repair_entries(100)
            .await
            .expect("peek")
            .iter()
            .map(|e| e.checkpoint_sequence)
            .collect();
        for seq in &peeked {
            storage
                .complete_repair(*seq as u64, true, None, 3, 0)
                .await
                .expect("drain");
        }
        for seq in [base, base + 1] {
            storage.enqueue_repair(seq, "boom").await.expect("enqueue");
        }
        // Success path completes both entries.
        let done = worker
            .tick(&storage, async |_| Ok(true))
            .await
            .expect("tick");
        assert_eq!(done, 2);
        // Nothing due: quiet tick.
        let quiet = worker
            .tick(&storage, async |_| Ok(true))
            .await
            .expect("tick");
        assert_eq!(quiet, 0);

        // No-progress path requeues without completing.
        storage
            .enqueue_repair(base + 2, "boom")
            .await
            .expect("enqueue");
        let stalled = worker
            .tick(&storage, async |_| Ok(false))
            .await
            .expect("tick");
        assert_eq!(stalled, 0);

        // Error path records the error and requeues.
        storage
            .enqueue_repair(base + 3, "boom")
            .await
            .expect("enqueue");
        let failed = worker
            .tick(&storage, async |_| {
                Err::<bool, eyre::Report>(eyre::eyre!("kaput"))
            })
            .await
            .expect("tick");
        assert_eq!(failed, 0);
    }

    #[test]
    fn backoff_clamps_and_caps() {
        let worker = RepairWorker::new(sui_indexer_config::RepairConfig {
            enabled: true,
            poll_interval_secs: 1,
            batch_size: 10,
            max_attempts: 4,
            backoff_base_secs: 30,
            backoff_max_secs: 100,
        });
        assert_eq!(worker.backoff(-5), 30);
        assert_eq!(worker.backoff(0), 30);
        assert_eq!(worker.backoff(1), 60);
        assert_eq!(worker.backoff(100), 100);
    }
}
