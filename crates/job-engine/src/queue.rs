//! Scan planning: chunked ranged scans with persisted cursors.
//!
//! Re-scans replay the archive in `chunk`-sized ranges, newest-first for
//! backfill priority, each chunk cancellable and resumable from its cursor.

/// One scan chunk: `[lo, hi)` heights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanChunk {
    /// First height (inclusive).
    pub lo: u64,
    /// Past-the-end height (exclusive).
    pub hi: u64,
}

/// Parse `scan.to`: `"head"` follows `tip`, otherwise a bounded height.
#[must_use]
pub fn parse_scan_to(to: &str, tip: u64) -> Option<u64> {
    let trimmed = to.trim();
    if trimmed.eq_ignore_ascii_case("head") || trimmed.is_empty() {
        Some(tip)
    } else {
        trimmed.parse::<u64>().ok()
    }
}

/// Split `[from, to]` (inclusive) into `chunk`-sized `[lo, hi)` ranges.
///
/// Ranges come back newest-first so recent data is queryable during a long
/// backfill; the caller persists the cursor per completed chunk.
#[must_use]
pub fn plan_chunks(from: u64, to: u64, chunk: u64) -> Vec<ScanChunk> {
    if from > to || chunk == 0 {
        return Vec::new();
    }
    let span = chunk.max(1);
    let mut ranges = Vec::new();
    let mut hi = to.saturating_add(1);
    // `from <= to` holds here, so `hi == from` is unreachable on entry and
    // `!=` is the exact "slices remain" test (`hi` only lands on `from`
    // right after the final slice is pushed).
    while hi != from {
        let lo = hi.saturating_sub(span).max(from);
        ranges.push(ScanChunk { lo, hi });
        if lo == from {
            break;
        }
        hi = lo;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn chunks_cover_the_range_newest_first_without_gaps() {
        let chunks = plan_chunks(0, 999, 400);
        assert_eq!(
            chunks,
            vec![
                ScanChunk { lo: 600, hi: 1000 },
                ScanChunk { lo: 200, hi: 600 },
                ScanChunk { lo: 0, hi: 200 },
            ]
        );
        // Contiguous and complete.
        assert_eq!(chunks.first().map(|c| c.hi), Some(1000));
        assert_eq!(chunks.last().map(|c| c.lo), Some(0));
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].lo, pair[1].hi);
        }
    }

    #[test]
    fn degenerate_inputs_yield_no_chunks() {
        assert!(plan_chunks(10, 5, 100).is_empty());
        assert!(plan_chunks(0, 100, 0).is_empty());
    }

    #[test]
    fn single_height_range_is_one_chunk() {
        assert_eq!(plan_chunks(5, 5, 10), vec![ScanChunk { lo: 5, hi: 6 }]);
    }

    #[test]
    fn scan_to_parses_head_and_bounds() {
        assert_eq!(parse_scan_to("head", 500), Some(500));
        assert_eq!(parse_scan_to("HEAD", 500), Some(500));
        assert_eq!(parse_scan_to("123", 500), Some(123));
        assert_eq!(parse_scan_to("bogus", 500), None);
    }

    proptest! {
        #[test]
        fn chunks_always_tile(from in 0u64..10_000, len in 0u64..5_000, chunk in 1u64..10_000) {
            let to = from.saturating_add(len);
            let chunks = plan_chunks(from, to, chunk);
            if from > to {
                prop_assert!(chunks.is_empty());
            } else {
                prop_assert!(!chunks.is_empty());
                prop_assert_eq!(chunks.last().map(|c| c.lo), Some(from));
                prop_assert_eq!(chunks.first().map(|c| c.hi), Some(to.saturating_add(1)));
                let covered: u64 = chunks.iter().map(|c| c.hi - c.lo).sum();
                prop_assert_eq!(covered, len.saturating_add(1));
            }
        }
    }
}
