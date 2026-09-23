//! Tier router and boundary keeper.
//!
//! Wide analytical queries hit the archive, point lookups hit PG, and the
//! hot/cold boundary only moves after the archive provably covers the range.

use std::collections::HashSet;

/// Which tier serves a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Hot window in PostgreSQL.
    Hot,
    /// Archive of record in ClickHouse.
    Archive,
}

/// Boundary decision for a proposed move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryCheck {
    /// Archive covers everything below `to`: safe to move.
    Ready {
        /// New boundary.
        to: u64,
    },
    /// Archive has gaps below `to`: report the first missing range.
    Missing {
        /// First missing height.
        lo: u64,
        /// Past-the-end missing height.
        hi: u64,
    },
}

/// Routes reads and guards the hot/cold boundary.
#[derive(Debug, Clone)]
pub struct TierRouter {
    /// Heights below this are served from the archive.
    pub pruned_below: u64,
    /// Tables mirrored hot in PG (`pg_hot`).
    pub pg_hot_tables: HashSet<String>,
}

impl TierRouter {
    /// Build with the current boundary and the hot-mirror set.
    #[must_use]
    pub fn new(pruned_below: u64, pg_hot_tables: HashSet<String>) -> Self {
        Self {
            pruned_below,
            pg_hot_tables,
        }
    }

    /// Route one table read at `height`: PG-hot tables stay hot, everything
    /// below the boundary is archive-only.
    #[must_use]
    pub fn route(&self, table: &str, height: u64) -> Tier {
        if self.pg_hot_tables.contains(table) {
            return Tier::Hot;
        }
        if height < self.pruned_below {
            Tier::Archive
        } else {
            Tier::Hot
        }
    }

    /// Check whether the boundary may move to `to`.
    ///
    /// `archive_covered(lo, hi)` reports whether the archive holds every
    /// height in `[lo, hi)`. The boundary moves only on full coverage, then
    /// the tiered views are re-baked atomically by the caller.
    #[must_use]
    pub fn check_boundary_move(
        &self,
        to: u64,
        archive_covered: &dyn Fn(u64, u64) -> Option<(u64, u64)>,
    ) -> BoundaryCheck {
        if to <= self.pruned_below {
            return BoundaryCheck::Ready {
                to: self.pruned_below,
            };
        }
        match archive_covered(self.pruned_below, to) {
            None => BoundaryCheck::Ready { to },
            Some((lo, hi)) => BoundaryCheck::Missing { lo, hi },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> TierRouter {
        TierRouter::new(1000, ["job_sandwich".to_owned()].into_iter().collect())
    }

    #[test]
    fn pg_hot_tables_always_route_hot() {
        let router = router();
        assert_eq!(router.route("job_sandwich", 10), Tier::Hot);
        assert_eq!(router.route("job_sandwich", 10_000), Tier::Hot);
    }

    #[test]
    fn boundary_splits_hot_and_archive() {
        let router = router();
        assert_eq!(router.route("chain_events", 999), Tier::Archive);
        assert_eq!(router.route("chain_events", 1000), Tier::Hot);
    }

    #[test]
    fn boundary_moves_only_on_full_coverage() {
        let router = router();
        let covered = |_: u64, _: u64| None;
        assert_eq!(
            router.check_boundary_move(2000, &covered),
            BoundaryCheck::Ready { to: 2000 }
        );
        let gapped = |_: u64, _: u64| Some((1500, 1600));
        assert_eq!(
            router.check_boundary_move(2000, &gapped),
            BoundaryCheck::Missing { lo: 1500, hi: 1600 }
        );
        // Never moves backwards.
        assert_eq!(
            router.check_boundary_move(500, &covered),
            BoundaryCheck::Ready { to: 1000 }
        );
    }
}
