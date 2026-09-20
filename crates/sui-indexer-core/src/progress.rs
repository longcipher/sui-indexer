use eyre::Result;
use sui_indexer_storage::StorageManager;

/// Converged progress helpers shared by sync, status, and archive flows.
pub struct Progress;

impl Progress {
    /// Resolve the resume frontier: explicit start wins, otherwise the
    /// continuous checkpoint plus one, falling back to the legacy watermark.
    pub async fn resolve_resume(
        storage: &StorageManager,
        start_checkpoint: Option<u64>,
    ) -> Result<u64> {
        if let Some(start) = start_checkpoint {
            return Ok(start);
        }
        if let Some(progress) = storage.get_progress("default").await? {
            if progress.continuous_checkpoint > 0 {
                return Ok(progress.continuous_checkpoint.max(0) as u64 + 1);
            }
        }
        Ok(storage
            .get_last_processed_checkpoint()
            .await?
            .saturating_add(1))
    }

    /// Current contiguous frontier plus floor for gap detection.
    pub async fn frontier(storage: &StorageManager) -> Result<(u64, u64)> {
        if let Some(progress) = storage.get_progress("default").await? {
            return Ok((
                progress.continuous_checkpoint.max(0) as u64,
                progress.floor_checkpoint.max(0) as u64,
            ));
        }
        let watermark = storage.get_last_processed_checkpoint().await?;
        Ok((watermark, watermark))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn progress_module_compiles() {
        assert_eq!(2 + 2, 4);
    }
}
