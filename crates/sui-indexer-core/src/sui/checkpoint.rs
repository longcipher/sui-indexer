use eyre::Result;
use serde::{Deserialize, Serialize};
use sui_types::base_types::TransactionDigest;

/// Checkpoint data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointData {
    /// Checkpoint sequence number
    pub sequence_number: u64,
    /// Checkpoint digest
    pub digest: String,
    /// Previous checkpoint digest
    pub previous_digest: Option<String>,
    /// Epoch number
    pub epoch: u64,
    /// Round number within epoch
    pub round: u64,
    /// Timestamp in milliseconds
    pub timestamp_ms: u64,
    /// Network total transactions count up to this checkpoint
    pub network_total_transactions: u64,
    /// Transactions included in this checkpoint
    pub transactions: Vec<TransactionDigest>,
    /// End of epoch data (if this checkpoint ends an epoch)
    pub end_of_epoch_data: Option<EndOfEpochData>,
    /// Validator signature
    pub validator_signature: String,
}

/// End of epoch data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndOfEpochData {
    /// Next epoch committee
    pub next_epoch_committee: Vec<CommitteeMember>,
    /// Next epoch protocol version
    pub next_epoch_protocol_version: u64,
    /// Epoch start timestamp
    pub epoch_start_timestamp_ms: u64,
}

/// Committee member information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitteeMember {
    /// Authority name
    pub authority_name: String,
    /// Stake amount
    pub stake: u64,
}

/// Checkpoint processor for managing checkpoint synchronization
#[derive(Debug)]
pub struct CheckpointProcessor {
    current_checkpoint: Option<u64>,
    target_checkpoint: Option<u64>,
}

impl CheckpointProcessor {
    /// Create a new checkpoint processor
    pub fn new(start_checkpoint: Option<u64>) -> Self {
        Self {
            current_checkpoint: start_checkpoint,
            target_checkpoint: None,
        }
    }

    /// Get the current checkpoint being processed
    pub fn current_checkpoint(&self) -> Option<u64> {
        self.current_checkpoint
    }

    /// Get the target checkpoint to sync to
    pub fn target_checkpoint(&self) -> Option<u64> {
        self.target_checkpoint
    }

    /// Update the target checkpoint
    pub fn set_target_checkpoint(&mut self, checkpoint: u64) {
        self.target_checkpoint = Some(checkpoint);
    }

    /// Advance to the next checkpoint
    pub fn advance_checkpoint(&mut self) -> Option<u64> {
        match self.current_checkpoint {
            Some(current) => {
                let next = current + 1;
                self.current_checkpoint = Some(next);
                Some(next)
            }
            None => {
                self.current_checkpoint = Some(0);
                Some(0)
            }
        }
    }

    /// Check if we've caught up to the target
    pub fn is_caught_up(&self) -> bool {
        match (self.current_checkpoint, self.target_checkpoint) {
            (Some(current), Some(target)) => current >= target,
            _ => false,
        }
    }

    /// Get the number of checkpoints remaining to sync
    pub fn checkpoints_remaining(&self) -> Option<u64> {
        match (self.current_checkpoint, self.target_checkpoint) {
            (Some(current), Some(target)) => {
                if target > current {
                    Some(target - current)
                } else {
                    Some(0)
                }
            }
            _ => None,
        }
    }

    /// Reset processor to a specific checkpoint
    pub fn reset_to_checkpoint(&mut self, checkpoint: u64) {
        self.current_checkpoint = Some(checkpoint);
    }
}

/// Checkpoint range for batch processing
#[derive(Debug, Clone)]
pub struct CheckpointRange {
    pub start: u64,
    pub end: u64,
}

impl CheckpointRange {
    /// Create a new checkpoint range
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if start > end {
            return Err(eyre::eyre!(
                "Start checkpoint cannot be greater than end checkpoint"
            ));
        }
        Ok(Self { start, end })
    }

    /// Get the number of checkpoints in this range
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Check if the range is empty
    pub fn is_empty(&self) -> bool {
        self.start > self.end
    }

    /// Split the range into smaller chunks
    pub fn split(&self, chunk_size: u64) -> Vec<CheckpointRange> {
        let mut ranges = Vec::new();
        let mut current_start = self.start;

        while current_start <= self.end {
            let current_end = std::cmp::min(current_start + chunk_size - 1, self.end);
            ranges.push(CheckpointRange {
                start: current_start,
                end: current_end,
            });
            current_start = current_end + 1;
        }

        ranges
    }

    /// Create an iterator over checkpoint numbers in this range
    pub fn iter(&self) -> CheckpointRangeIterator {
        CheckpointRangeIterator {
            current: self.start,
            end: self.end,
        }
    }
}

/// Iterator over checkpoint numbers in a range
pub struct CheckpointRangeIterator {
    current: u64,
    end: u64,
}

impl Iterator for CheckpointRangeIterator {
    type Item = u64;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current <= self.end {
            let checkpoint = self.current;
            self.current += 1;
            Some(checkpoint)
        } else {
            None
        }
    }
}

/// Checkpoint statistics for monitoring
#[derive(Debug, Clone, Serialize)]
pub struct CheckpointStats {
    pub total_processed: u64,
    pub current_checkpoint: Option<u64>,
    pub target_checkpoint: Option<u64>,
    pub processing_rate: f64, // checkpoints per second
    pub estimated_time_remaining: Option<std::time::Duration>,
}

impl CheckpointStats {
    /// Calculate processing statistics
    pub fn calculate(
        processor: &CheckpointProcessor,
        total_processed: u64,
        start_time: std::time::Instant,
    ) -> Self {
        let elapsed = start_time.elapsed();
        let processing_rate = if elapsed.as_secs() > 0 {
            total_processed as f64 / elapsed.as_secs() as f64
        } else {
            0.0
        };

        let estimated_time_remaining = processor.checkpoints_remaining().and_then(|remaining| {
            if processing_rate > 0.0 {
                Some(std::time::Duration::from_secs(
                    (remaining as f64 / processing_rate) as u64,
                ))
            } else {
                None
            }
        });

        Self {
            total_processed,
            current_checkpoint: processor.current_checkpoint(),
            target_checkpoint: processor.target_checkpoint(),
            processing_rate,
            estimated_time_remaining,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_processor() {
        let mut processor = CheckpointProcessor::new(Some(100));

        assert_eq!(processor.current_checkpoint(), Some(100));
        assert_eq!(processor.advance_checkpoint(), Some(101));
        assert_eq!(processor.current_checkpoint(), Some(101));

        processor.set_target_checkpoint(105);
        assert_eq!(processor.checkpoints_remaining(), Some(4));
        assert!(!processor.is_caught_up());

        // Advance to target
        processor.advance_checkpoint(); // 102
        processor.advance_checkpoint(); // 103
        processor.advance_checkpoint(); // 104
        processor.advance_checkpoint(); // 105

        assert!(processor.is_caught_up());
        assert_eq!(processor.checkpoints_remaining(), Some(0));
    }

    #[test]
    fn test_checkpoint_range() {
        let range = CheckpointRange::new(10, 20).unwrap();
        assert_eq!(range.len(), 11);
        assert!(!range.is_empty());

        let chunks = range.split(5);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].start, 10);
        assert_eq!(chunks[0].end, 14);
        assert_eq!(chunks[1].start, 15);
        assert_eq!(chunks[1].end, 19);
        assert_eq!(chunks[2].start, 20);
        assert_eq!(chunks[2].end, 20);
    }

    #[test]
    fn test_checkpoint_range_iterator() {
        let range = CheckpointRange::new(5, 8).unwrap();
        let checkpoints: Vec<u64> = range.iter().collect();
        assert_eq!(checkpoints, vec![5, 6, 7, 8]);
    }

    #[test]
    fn test_invalid_checkpoint_range() {
        let result = CheckpointRange::new(20, 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_checkpoint_stats() {
        let mut processor = CheckpointProcessor::new(Some(100));
        processor.set_target_checkpoint(200);

        let start_time = std::time::Instant::now() - std::time::Duration::from_secs(10);
        let stats = CheckpointStats::calculate(&processor, 50, start_time);

        assert_eq!(stats.total_processed, 50);
        assert_eq!(stats.current_checkpoint, Some(100));
        assert_eq!(stats.target_checkpoint, Some(200));
        assert!(stats.processing_rate > 0.0);
    }
}
