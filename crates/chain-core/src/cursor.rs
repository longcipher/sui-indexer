//! Height cursor: three watermarks plus gap helpers.
//!
//! Every chain is a monotone integer sequence; gap detection, reorg rollback
//! and tier boundaries all derive from [`SyncState`]:
//! `synced_num` (contiguous), `tip_num` (near head), `backfill_num`
//! (reverse fill).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Integer-height cursor with three watermarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SyncState {
    /// Highest contiguous height (inclusive).
    pub synced_num: u64,
    /// Highest observed tip height.
    pub tip_num: u64,
    /// Lowest height covered by the reverse fill (0 = no backfill running).
    pub backfill_num: u64,
}

impl SyncState {
    /// Number of heights the realtime lane is behind the tip.
    #[must_use]
    pub fn lag(&self) -> u64 {
        self.tip_num.saturating_sub(self.synced_num)
    }

    /// Whether the cursor has caught up with the tip.
    #[must_use]
    pub fn is_caught_up(&self) -> bool {
        self.synced_num >= self.tip_num
    }
}

/// One missing height interval, inclusive on both ends, newest-first order
/// when produced by gap detectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeightGap {
    /// First missing height (inclusive).
    pub start: u64,
    /// Last missing height (inclusive).
    pub end: u64,
}

impl HeightGap {
    /// Build an inclusive gap; `None` when `start > end`.
    #[must_use]
    pub fn new(start: u64, end: u64) -> Option<Self> {
        if start > end {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// Number of heights covered (0 for inverted gaps).
    #[must_use]
    pub fn len(&self) -> u64 {
        if self.start > self.end {
            0
        } else {
            self.end.saturating_sub(self.start).saturating_add(1)
        }
    }

    /// Whether the gap is empty (never true for constructed gaps).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Longest gap-free prefix of `[start, end]` present in `succeeded`.
///
/// Used by the contiguous-commit path: only the prefix advances the cursor,
/// the remainder is retried (or enqueued for repair).
#[must_use]
pub fn contiguous_prefix(start: u64, end: u64, succeeded: &BTreeSet<u64>) -> Option<u64> {
    let mut cursor = start;
    while cursor <= end {
        if !succeeded.contains(&cursor) {
            return if cursor == start {
                None
            } else {
                Some(cursor.saturating_sub(1))
            };
        }
        if cursor == end {
            return Some(end);
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

/// Island-group a sorted height list into inclusive [`HeightGap`]s.
///
/// `present` must be sorted ascending; duplicates are ignored.
#[must_use]
pub fn gaps_in(floor: u64, tip: u64, present: &[u64]) -> Vec<HeightGap> {
    let mut gaps = Vec::new();
    let mut cursor = floor;
    for height in present.iter().copied() {
        if height < cursor {
            continue;
        }
        if height > cursor {
            if let Some(gap) = HeightGap::new(cursor, height.saturating_sub(1)) {
                gaps.push(gap);
            }
        }
        cursor = height.saturating_add(1);
        if cursor > tip {
            break;
        }
    }
    if cursor <= tip {
        if let Some(gap) = HeightGap::new(cursor, tip) {
            gaps.push(gap);
        }
    }
    // Newest-first for the backfiller.
    gaps.sort_by_key(|gap| std::cmp::Reverse(gap.start));
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn contiguous_prefix_returns_longest_gap_free_prefix() {
        let ok: BTreeSet<u64> = [10, 11, 12, 14].into_iter().collect();
        assert_eq!(contiguous_prefix(10, 15, &ok), Some(12));
        assert_eq!(contiguous_prefix(14, 15, &ok), Some(14));
        assert_eq!(contiguous_prefix(13, 15, &ok), None);
    }

    #[test]
    fn gaps_in_detects_holes_newest_first() {
        let gaps = gaps_in(10, 15, &[10, 11, 14]);
        assert_eq!(
            gaps,
            vec![
                HeightGap { start: 15, end: 15 },
                HeightGap { start: 12, end: 13 },
            ]
        );
    }

    #[test]
    fn gaps_in_empty_present_means_full_gap() {
        assert_eq!(gaps_in(5, 7, &[]), vec![HeightGap { start: 5, end: 7 }]);
    }

    #[test]
    fn watermarks_report_lag_and_catch_up() {
        let state = SyncState {
            synced_num: 90,
            tip_num: 100,
            backfill_num: 0,
        };
        assert_eq!(state.lag(), 10);
        assert!(!state.is_caught_up());
        let caught = SyncState {
            synced_num: 100,
            tip_num: 100,
            backfill_num: 0,
        };
        assert_eq!(caught.lag(), 0);
        assert!(caught.is_caught_up());
        let ahead = SyncState {
            synced_num: 101,
            tip_num: 100,
            backfill_num: 0,
        };
        assert_eq!(ahead.lag(), 0);
        assert!(ahead.is_caught_up());
    }

    #[test]
    fn gap_len_and_emptiness() {
        let gap = HeightGap::new(5, 9).expect("gap");
        assert_eq!(gap.len(), 5);
        assert!(!gap.is_empty());
        assert!(HeightGap::new(9, 5).is_none());
        let single = HeightGap::new(5, 5).expect("gap");
        assert_eq!(single.len(), 1);
        assert!(!single.is_empty());
        // Manually built inverted gaps cover nothing.
        let inverted = HeightGap { start: 5, end: 4 };
        assert_eq!(inverted.len(), 0);
        assert!(inverted.is_empty());
    }

    #[test]
    fn duplicates_at_the_floor_do_not_emit_phantom_gaps() {
        assert!(gaps_in(0, 3, &[0, 1, 2, 3]).is_empty());
        assert_eq!(
            gaps_in(0, 3, &[0, 0, 2]),
            vec![
                HeightGap { start: 3, end: 3 },
                HeightGap { start: 1, end: 1 }
            ]
        );
    }

    proptest! {
        #[test]
        fn contiguous_prefix_never_exceeds_end(
            start in 0u64..1000,
            len in 0u64..50,
            mask in proptest::collection::vec(proptest::bool::ANY, 0..50),
        ) {
            let end = start.saturating_add(len);
            let succeeded: BTreeSet<u64> = (start..=end)
                .zip(mask.iter().chain(std::iter::repeat(&true)))
                .filter(|(_, keep)| **keep)
                .map(|(h, _)| h)
                .collect();
            if let Some(prefix) = contiguous_prefix(start, end, &succeeded) {
                prop_assert!(prefix <= end);
                prop_assert!((start..=prefix).all(|h| succeeded.contains(&h)));
            } else {
                prop_assert!(!succeeded.contains(&start));
            }
        }
    }
}
