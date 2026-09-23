//! Concurrency quota + rate limiting for sync and job scans.
//!
//! Freshness beats backfill: realtime sync has priority; job scans run on a
//! separate concurrency quota and share the same rate limiter as the
//! gap-filler.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;

/// Bounded-concurrency pool with a minimum interval between acquisitions.
///
/// The semaphore caps in-flight work; `min_interval` spaces out the start of
/// each unit of work so RPC endpoints are never hammered.
#[derive(Debug, Clone)]
pub struct ThrottledPool {
    semaphore: Arc<Semaphore>,
    min_interval: Duration,
}

impl ThrottledPool {
    /// Build a pool with `permits` concurrent slots and `min_interval` pacing.
    #[must_use]
    pub fn new(permits: usize, min_interval: Duration) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permits.max(1))),
            min_interval,
        }
    }

    /// Acquire one permit, pacing the acquisition by `min_interval`.
    ///
    /// Returns `None` when the pool is closed (shutdown).
    pub async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        sleep(self.min_interval).await;
        self.semaphore.clone().acquire_owned().await.ok()
    }

    /// Number of permits currently available.
    #[must_use]
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn pool_caps_concurrent_holders() {
        let pool = ThrottledPool::new(2, Duration::from_millis(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..6 {
            let pool = pool.clone();
            let in_flight = in_flight.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                let _permit = pool.acquire().await.expect("pool open");
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);
                sleep(Duration::from_millis(5)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.expect("task");
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn pool_reports_available_permits() {
        let pool = ThrottledPool::new(4, Duration::from_millis(0));
        assert_eq!(pool.available(), 4);
    }
}
