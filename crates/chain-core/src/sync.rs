//! Dual-lane sync planner: realtime follows the head while a gap-filler
//! heals holes newest-first, pausing when realtime lags too far behind.

use serde::{Deserialize, Serialize};

/// Inclusive height range with an emptiness flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeightRange {
    /// First height (inclusive).
    pub start: u64,
    /// Last height (inclusive).
    pub end: u64,
    /// True when there is nothing to fetch.
    pub is_empty: bool,
}

impl HeightRange {
    /// Build an inclusive range; empty when `start > end`.
    #[must_use]
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

    /// Number of heights covered (0 when empty).
    #[must_use]
    pub fn len(&self) -> u64 {
        if self.is_empty {
            0
        } else {
            self.end.saturating_sub(self.start).saturating_add(1)
        }
    }

    /// Whether the range covers no heights.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.is_empty
    }
}

/// Dual-lane sync tick plan.
#[derive(Debug, Clone, Default)]
pub struct SyncTick {
    /// Gap ranges to backfill before tracking, newest first.
    pub backfill_ranges: Vec<HeightRange>,
    /// Tracker range advancing the frontier, if any.
    pub tracker_range: Option<HeightRange>,
}

/// Planner configuration (mirrors the existing `SyncConfig` lanes).
#[derive(Debug, Clone, Copy)]
pub struct SyncPlannerConfig {
    /// Tracker window size.
    pub tracker_batch_size: u64,
    /// Whether the backfill lane runs.
    pub backfill_enabled: bool,
    /// Backfill slice size.
    pub backfill_batch_size: u64,
    /// Lag beyond which realtime pauses the backfiller.
    pub lag_yield_threshold: u64,
}

impl Default for SyncPlannerConfig {
    fn default() -> Self {
        Self {
            tracker_batch_size: 50,
            backfill_enabled: true,
            backfill_batch_size: 200,
            lag_yield_threshold: 10,
        }
    }
}

/// Dual-lane sync planner: tip tracker + historical backfiller.
#[derive(Debug, Clone)]
pub struct SyncEngine {
    config: SyncPlannerConfig,
    ceiling: Option<u64>,
}

impl SyncEngine {
    /// Create a planner with an optional run ceiling (`last_checkpoint`).
    #[must_use]
    pub fn new(config: SyncPlannerConfig, ceiling: Option<u64>) -> Self {
        Self { config, ceiling }
    }

    /// Plan one tick: split `[next, end]` into a tracker window at the
    /// frontier plus backfill slices for the remainder, newest first.
    #[must_use]
    pub fn plan_tick(&self, next: u64, end: u64, latest: u64) -> SyncTick {
        let ceiling = self.ceiling.map(|last| last.min(latest)).unwrap_or(end);
        let end = end.min(ceiling);
        if next > end {
            return SyncTick::default();
        }
        let tracker_span = self.config.tracker_batch_size.max(1);
        let backfill_span = self.config.backfill_batch_size.max(1);
        let tracker_end = next.saturating_add(tracker_span).saturating_sub(1).min(end);

        let mut backfill_ranges = Vec::new();
        // `tracker_end <= end` always holds (`min(end)` above), so `!=`
        // is the exact "remainder exists" test.
        if self.config.backfill_enabled && tracker_end != end {
            let mut cursor = end;
            // `cursor >= tracker_end` is invariant (it starts above and only
            // steps down to `slice_start - 1 >= tracker_end`), so `!=` is the
            // exact "slices remain" test.
            while cursor != tracker_end {
                let slice_end = cursor;
                // `slice_start >= tracker_end + 1` always holds (the `max`
                // below), so `==` is the exact "final slice" test.
                let slice_start = cursor
                    .saturating_sub(backfill_span)
                    .saturating_add(1)
                    .max(tracker_end.saturating_add(1));
                backfill_ranges.push(HeightRange::new(slice_start, slice_end));
                if slice_start == tracker_end.saturating_add(1) {
                    break;
                }
                cursor = slice_start.saturating_sub(1);
            }
        }

        SyncTick {
            backfill_ranges,
            tracker_range: Some(HeightRange::new(next, tracker_end)),
        }
    }

    /// Whether the backfiller should yield so the tracker catches up.
    #[must_use]
    pub fn should_yield_to_tracker(&self, lag: u64) -> bool {
        lag > self.config.lag_yield_threshold
    }
}

/// Split a large range into bounded chunks for contiguous commit.
#[must_use]
pub fn split_range(start: u64, end: u64, max_span: u64) -> Vec<HeightRange> {
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
        ranges.push(HeightRange::new(cursor, chunk_end));
        cursor = chunk_end.saturating_add(1);
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> SyncEngine {
        SyncEngine::new(
            SyncPlannerConfig {
                tracker_batch_size: 10,
                backfill_enabled: true,
                backfill_batch_size: 20,
                lag_yield_threshold: 10,
            },
            None,
        )
    }

    #[test]
    fn tick_plans_tracker_window_plus_newest_first_backfill() {
        let tick = engine().plan_tick(100, 159, 200);
        let tracker = tick.tracker_range.expect("tracker");
        assert_eq!((tracker.start, tracker.end), (100, 109));
        assert_eq!(tick.backfill_ranges.len(), 3);
        assert!(tick.backfill_ranges[0].start > tick.backfill_ranges[1].start);
        assert_eq!(tick.backfill_ranges.last().map(|gap| gap.start), Some(110));
    }

    #[test]
    fn tick_with_next_past_end_is_empty() {
        let tick = engine().plan_tick(200, 100, 500);
        assert!(tick.backfill_ranges.is_empty());
        assert!(tick.tracker_range.is_none());
    }

    #[test]
    fn tick_with_next_at_end_covers_one_height() {
        let tick = engine().plan_tick(100, 100, 500);
        let tracker = tick.tracker_range.expect("tracker");
        assert_eq!((tracker.start, tracker.end), (100, 100));
        assert!(tick.backfill_ranges.is_empty());
    }

    #[test]
    fn backfill_ranges_never_contain_empty_slices() {
        let tick = engine().plan_tick(100, 159, 200);
        assert_eq!(tick.backfill_ranges.len(), 3);
        for range in &tick.backfill_ranges {
            assert!(!range.is_empty());
            assert!(range.len() <= 20);
        }
    }

    #[test]
    fn range_len_and_emptiness() {
        assert_eq!(HeightRange::new(5, 9).len(), 5);
        assert_eq!(HeightRange::new(5, 5).len(), 1);
        assert_eq!(HeightRange::new(9, 5).len(), 0);
        assert!(HeightRange::new(9, 5).is_empty());
        assert!(!HeightRange::new(5, 5).is_empty());
    }

    #[test]
    fn split_range_single_height_is_one_chunk() {
        let chunks = split_range(5, 5, 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].start, chunks[0].end), (5, 5));
    }

    #[test]
    fn tick_respects_ceiling() {
        let engine = SyncEngine::new(
            SyncPlannerConfig {
                tracker_batch_size: 10,
                backfill_enabled: true,
                backfill_batch_size: 20,
                lag_yield_threshold: 10,
            },
            Some(120),
        );
        let tick = engine.plan_tick(100, 200, 500);
        let tracker = tick.tracker_range.expect("tracker");
        assert!(tracker.end <= 120);
    }

    #[test]
    fn disabled_backfill_emits_tracker_only() {
        let engine = SyncEngine::new(
            SyncPlannerConfig {
                tracker_batch_size: 10,
                backfill_enabled: false,
                backfill_batch_size: 20,
                lag_yield_threshold: 10,
            },
            None,
        );
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

    #[test]
    fn realtime_pauses_backfill_when_lag_exceeds_threshold() {
        assert!(engine().should_yield_to_tracker(11));
        assert!(!engine().should_yield_to_tracker(10));
    }
}
