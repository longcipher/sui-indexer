use eyre::Result;
use sui_indexer_config::SyncConfig;

/// Dual-lane sync tick plan: historical gaps heal newest-first while the tip
/// tracker advances the contiguous frontier.
#[derive(Debug, Clone, Default)]
pub struct SyncTick {
    /// Gap ranges to backfill before tracking, newest first.
    pub backfill_ranges: Vec<CheckpointGap>,
    /// Tracker range advancing the frontier, if any.
    pub tracker_range: Option<CheckpointGap>,
}

/// Inclusive checkpoint range with emptiness flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointGap {
    /// First checkpoint (inclusive).
    pub start: u64,
    /// Last checkpoint (inclusive).
    pub end: u64,
    /// True when there is nothing to fetch.
    pub is_empty: bool,
}

impl CheckpointGap {
    /// Build an inclusive range; empty when start exceeds end.
    pub fn new(start: u64, end: u64) -> Self {
        if start > end {
            Self {
                start,
                end,
                is_empty: true,
            }
        } else {
            Self {
                start,
                end,
                is_empty: false,
            }
        }
    }

    /// Number of checkpoints covered (0 when empty).
    pub fn len(&self) -> u64 {
        if self.is_empty {
            0
        } else {
            self.end.saturating_sub(self.start).saturating_add(1)
        }
    }

    /// Whether the range covers no checkpoints.
    pub fn is_empty(&self) -> bool {
        self.is_empty
    }
}

/// Dual-lane sync planner: tip tracker + historical backfiller.
#[derive(Debug, Clone)]
pub struct SyncEngine {
    config: SyncConfig,
    last_checkpoint: Option<u64>,
}

impl SyncEngine {
    /// Create a planner from sync config and optional run ceiling.
    pub fn new(config: SyncConfig, last_checkpoint: Option<u64>) -> Self {
        Self {
            config,
            last_checkpoint,
        }
    }

    /// Plan one tick: split [next, end] into a tracker window at the frontier
    /// plus backfill slices for the remainder, newest first.
    pub fn plan_tick(&self, next: u64, end: u64, latest: u64) -> SyncTick {
        let ceiling = self
            .last_checkpoint
            .map(|last| last.min(latest))
            .unwrap_or(end);
        let end = end.min(ceiling);
        if next > end {
            return SyncTick::default();
        }
        let tracker_span = self.config.tracker_batch_size.max(1);
        let backfill_span = self.config.backfill_batch_size.max(1);
        let tracker_end = next.saturating_add(tracker_span).saturating_sub(1).min(end);

        let mut backfill_ranges = Vec::new();
        if self.config.backfill_enabled && tracker_end < end {
            let mut cursor = end;
            while cursor > tracker_end {
                let slice_end = cursor;
                let slice_start = cursor
                    .saturating_sub(backfill_span)
                    .saturating_add(1)
                    .max(tracker_end.saturating_add(1));
                backfill_ranges.push(CheckpointGap::new(slice_start, slice_end));
                if slice_start <= tracker_end.saturating_add(1) {
                    break;
                }
                cursor = slice_start.saturating_sub(1);
            }
        }

        SyncTick {
            backfill_ranges,
            tracker_range: Some(CheckpointGap::new(next, tracker_end)),
        }
    }

    /// Whether the backfiller should yield so the tracker catches up.
    pub fn should_yield_to_tracker(&self, lag: u64) -> bool {
        lag > self.config.lag_yield_threshold
    }
}

/// Split a large range into bounded chunks for contiguous commit.
pub fn split_range(start: u64, end: u64, max_span: u64) -> Vec<CheckpointGap> {
    if start > end {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let chunk_end = cursor
            .saturating_add(max_span.max(1))
            .saturating_sub(1)
            .min(end);
        ranges.push(CheckpointGap::new(cursor, chunk_end));
        cursor = chunk_end.saturating_add(1);
    }
    ranges
}

pub async fn detect_gaps_via_storage(
    storage: &sui_indexer_storage::StorageManager,
    floor: u64,
    tip: u64,
) -> Result<Vec<CheckpointGap>> {
    let gaps = storage.detect_gaps(floor, tip).await?;
    Ok(gaps
        .into_iter()
        .map(|(start, end)| CheckpointGap::new(start, end))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SyncConfig {
        SyncConfig {
            tip_interval_secs: 2,
            tracker_batch_size: 10,
            backfill_enabled: true,
            backfill_batch_size: 20,
            backfill_concurrency: 4,
            lag_yield_threshold: 10,
            max_range_span: 200,
            failure_backoff_threshold: 5,
        }
    }

    #[test]
    fn tick_plans_tracker_window_plus_newest_first_backfill() {
        let engine = SyncEngine::new(config(), None);
        let tick = engine.plan_tick(100, 159, 200);
        let tracker = tick.tracker_range.expect("tracker");
        assert_eq!((tracker.start, tracker.end), (100, 109));
        assert_eq!(tick.backfill_ranges.len(), 3);
        assert!(tick.backfill_ranges[0].start > tick.backfill_ranges[1].start);
        assert_eq!(tick.backfill_ranges.last().map(|gap| gap.start), Some(110));
    }

    #[test]
    fn tick_respects_last_checkpoint_ceiling() {
        let engine = SyncEngine::new(config(), Some(120));
        let tick = engine.plan_tick(100, 200, 500);
        let tracker = tick.tracker_range.expect("tracker");
        assert!(tracker.end <= 120);
    }

    #[test]
    fn disabled_backfill_emits_tracker_only() {
        let mut cfg = config();
        cfg.backfill_enabled = false;
        let engine = SyncEngine::new(cfg, None);
        let tick = engine.plan_tick(5, 50, 50);
        assert!(tick.backfill_ranges.is_empty());
        assert!(tick.tracker_range.is_some());
    }

    #[test]
    fn split_range_chunks_large_spans() {
        let chunks = split_range(0, 449, 200);
        assert_eq!(chunks.len(), 3);
        assert_eq!((chunks[0].start, chunks[0].end), (0, 199));
        assert_eq!((chunks[2].start, chunks[2].end), (400, 449));
        assert!(split_range(9, 5, 10).is_empty());
    }
}
