//! Height-cursor composition: gaps → tick → contiguous commit.

use std::collections::BTreeSet;

use chain_core::{HeightGap, SyncEngine, SyncPlannerConfig, contiguous_prefix, gaps_in};

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
fn gap_detection_drives_backfill_and_commit_advances_the_cursor() {
    // Archive holds 100..=109 and 120..=129; the tip is at 129.
    let present: Vec<u64> = (100..=109).chain(120..=129).collect();
    let gaps = gaps_in(100, 129, &present);
    assert_eq!(
        gaps,
        vec![HeightGap {
            start: 110,
            end: 119
        }]
    );

    // The tick covers the frontier; the gap heals newest-first.
    let tick = engine().plan_tick(130, 150, 150);
    let tracker = tick.tracker_range.expect("tracker");
    assert_eq!((tracker.start, tracker.end), (130, 139));
    assert!(tick.backfill_ranges.first().map(|r| r.end) == Some(150));

    // Only the gap-free prefix commits; the rest retries.
    let succeeded: BTreeSet<u64> = (110..=115).collect();
    assert_eq!(contiguous_prefix(110, 119, &succeeded), Some(115));
    assert_eq!(contiguous_prefix(116, 119, &succeeded), None);
}

#[test]
fn realtime_yields_when_lag_exceeds_threshold() {
    assert!(engine().should_yield_to_tracker(11));
    assert!(!engine().should_yield_to_tracker(10));
}
