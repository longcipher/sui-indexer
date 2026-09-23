use eyre::Result;
use std::path::{Path, PathBuf};
use sui_indexer_config::ArchiveConfig;
use sui_indexer_storage::StorageManager;
use tracing::info;

/// Cold archive writer: persists canonical checkpoint segments to local files
/// for full-history retention outside PostgreSQL.
pub struct ArchiveWriter {
    config: ArchiveConfig,
}

impl ArchiveWriter {
    /// Create a writer from archive config.
    pub fn new(config: ArchiveConfig) -> Self {
        Self { config }
    }

    /// Whether archiving is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Archive one segment [start, end] as newline-delimited JSON.
    pub async fn archive_range(
        &self,
        storage: &StorageManager,
        pipeline: &str,
        start: u64,
        end: u64,
    ) -> Result<(u64, u64)> {
        if !self.config.enabled || start > end {
            return Ok((start, end));
        }
        let directory = PathBuf::from(&self.config.directory);
        std::fs::create_dir_all(&directory)?;
        let digests = storage.checkpoint_digests(start, end).await?;
        let segment_start = start - (start % self.config.segment_size.max(1));
        let path = directory.join(format!("checkpoints-{segment_start:020}.jsonl"));
        let mut bytes = 0_usize;
        let mut file = std::fs::File::create(&path)?;
        use std::io::Write;
        for (sequence, digest) in &digests {
            let line = serde_json::json!({
                "sequence": sequence,
                "digest": digest,
            });
            let mut text = serde_json::to_string(&line)?;
            text.push('\n');
            bytes += text.len();
            file.write_all(text.as_bytes())?;
        }
        info!(
            "Archived checkpoints {start}..={end} to {} ({bytes} bytes)",
            path.display()
        );
        storage
            .record_archive_window(pipeline, Some(start), Some(end), None)
            .await?;
        Ok((start, end))
    }

    /// Archive directory path.
    pub fn directory(&self) -> &Path {
        Path::new(&self.config.directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_writer_reports_not_enabled() {
        let writer = ArchiveWriter::new(ArchiveConfig {
            enabled: false,
            directory: "./archive".to_string(),
            segment_size: 100,
            hot_keep_checkpoints: None,
        });
        assert!(!writer.enabled());
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

    fn enabled(dir: &std::path::Path) -> ArchiveWriter {
        ArchiveWriter::new(sui_indexer_config::ArchiveConfig {
            enabled: true,
            directory: dir.to_string_lossy().into_owned(),
            segment_size: 100,
            hot_keep_checkpoints: None,
        })
    }

    #[tokio::test]
    async fn range_archives_segment_files_and_window() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("archive-{}", std::process::id()));
        let writer = enabled(&dir);
        assert!(writer.enabled());
        assert_eq!(writer.directory(), dir.as_path());
        // Seed digests for heights 150..=152 (unique to this suite).
        for seq in [150u64, 151, 152] {
            storage
                .store_checkpoint_model(sui_indexer_storage::CheckpointModel::new(
                    sui_indexer_storage::CheckpointModelConfig {
                        sequence_number: seq as i64,
                        digest: format!("digest-{seq}"),
                        prev_digest: None,
                        epoch: 1,
                        timestamp_ms: 1_000,
                        transaction_count: 0,
                        network_total_transactions: 0,
                        validator_signature: String::new(),
                        end_of_epoch_data: None,
                    },
                ))
                .await
                .expect("checkpoint");
        }
        let pipe = format!("archive-{}", std::process::id());
        let committed = writer
            .archive_range(&storage, &pipe, 150, 152)
            .await
            .expect("archive");
        assert_eq!(committed, (150, 152));
        // Segment 100 covers [100, 200): three JSON lines.
        let path = dir.join("checkpoints-00000000000000000100.jsonl");
        let text = std::fs::read_to_string(&path).expect("segment");
        assert_eq!(text.lines().count(), 3);
        assert!(text.contains("digest-151"));
        let progress = storage
            .get_progress(&pipe)
            .await
            .expect("progress")
            .expect("row");
        assert_eq!(progress.archive_lo, Some(150));
        assert_eq!(progress.archive_hi, Some(152));
        // Single-height ranges archive one segment, not a passthrough:
        // the window narrows its low end while the high watermark holds.
        let single = writer
            .archive_range(&storage, &pipe, 151, 151)
            .await
            .expect("single");
        assert_eq!(single, (151, 151));
        let narrowed = storage
            .get_progress(&pipe)
            .await
            .expect("progress")
            .expect("row");
        assert_eq!(
            (narrowed.archive_lo, narrowed.archive_hi),
            (Some(151), Some(152))
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[tokio::test]
    async fn disabled_or_empty_range_passes_through() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("archive-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let off = ArchiveWriter::new(sui_indexer_config::ArchiveConfig {
            enabled: false,
            directory: dir.to_string_lossy().into_owned(),
            segment_size: 100,
            hot_keep_checkpoints: None,
        });
        assert!(!off.enabled());
        let pipe = format!("pipe-{}", std::process::id());
        // Disabled: pure passthrough, no directory, no progress row.
        assert_eq!(
            off.archive_range(&storage, &pipe, 5, 9).await.expect("off"),
            (5, 9)
        );
        assert!(!dir.exists());
        assert!(
            storage
                .get_progress(&pipe)
                .await
                .expect("progress")
                .is_none()
        );
        // Empty range with an enabled writer: same passthrough, and no
        // segment file may appear for a degenerate range.
        let empty_dir = std::env::temp_dir().join(format!("archive-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty_dir);
        let on = enabled(&empty_dir);
        assert_eq!(
            on.archive_range(&storage, &pipe, 9, 5)
                .await
                .expect("empty"),
            (9, 5)
        );
        assert!(
            !empty_dir
                .join("checkpoints-00000000000000000000.jsonl")
                .exists()
        );
    }
}
