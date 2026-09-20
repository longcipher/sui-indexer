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
