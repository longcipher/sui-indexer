//! Per-job observability: gauges every dashboard needs.
//!
//! All metrics carry `chain`, `job` and `version` labels — dashboards that
//! aggregate without them are wrong in a multichain deployment.

use std::sync::atomic::{AtomicU64, Ordering};

use chain_core::MetricLabels;

/// In-memory gauges for one job version.
#[derive(Debug, Default)]
pub struct JobMetrics {
    /// Labels attached to every exported sample.
    pub labels: Option<MetricLabels>,
    scan_height: AtomicU64,
    lag: AtomicU64,
    rows_written: AtomicU64,
    failures: AtomicU64,
    quarantined: AtomicU64,
}

impl JobMetrics {
    /// Create labelled gauges for one job version.
    #[must_use]
    pub fn labelled(chain: &str, job: &str, version: u32) -> Self {
        Self {
            labels: Some(MetricLabels {
                chain: chain.to_owned(),
                job: job.to_owned(),
                version,
            }),
            ..Self::default()
        }
    }

    /// Record scan progress.
    pub fn set_scan_height(&self, height: u64) {
        self.scan_height.store(height, Ordering::Relaxed);
    }

    /// Record tip lag.
    pub fn set_lag(&self, lag: u64) {
        self.lag.store(lag, Ordering::Relaxed);
    }

    /// Accumulate rows written.
    pub fn add_rows(&self, rows: u64) {
        self.rows_written.fetch_add(rows, Ordering::Relaxed);
    }

    /// Record a failure; `quarantine = true` marks the version quarantined.
    pub fn add_failure(&self, quarantine: bool) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        if quarantine {
            self.quarantined.store(1, Ordering::Relaxed);
        }
    }

    /// Render Prometheus exposition lines.
    #[must_use]
    pub fn render(&self) -> String {
        let suffix = self
            .labels
            .as_ref()
            .map_or_else(String::new, |l| l.prometheus_suffix());
        let get = |v: &AtomicU64| v.load(Ordering::Relaxed);
        format!(
            "indexer_job_scan_height{suffix} {}\n\
             indexer_job_lag{suffix} {}\n\
             indexer_job_rows_written_total{suffix} {}\n\
             indexer_job_failures_total{suffix} {}\n\
             indexer_job_quarantined{suffix} {}\n",
            get(&self.scan_height),
            get(&self.lag),
            get(&self.rows_written),
            get(&self.failures),
            get(&self.quarantined),
        )
    }
}

/// Shared registry of per-version gauges: the runner registers on start
/// and unregisters on stop; `/metrics` renders everything live.
#[derive(Debug, Default)]
pub struct JobMetricsRegistry {
    inner: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<JobMetrics>>>,
}

impl JobMetricsRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one version's gauges (overwrites any previous entry).
    pub fn register(&self, key: String, metrics: std::sync::Arc<JobMetrics>) {
        self.inner.lock().expect("mutex").insert(key, metrics);
    }

    /// Forget one version's gauges.
    pub fn unregister(&self, key: &str) {
        self.inner.lock().expect("mutex").remove(key);
    }

    /// Render every registered version in Prometheus exposition format.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut keys: Vec<String> = self.inner.lock().expect("mutex").keys().cloned().collect();
        keys.sort();
        for key in keys {
            if let Some(metrics) = self.inner.lock().expect("mutex").get(&key) {
                out.push_str(&metrics.render());
            }
        }
        out
    }

    /// Number of registered versions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("mutex").len()
    }

    /// Whether no versions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().expect("mutex").is_empty()
    }
}

/// Registry key for one job version: `chain/job/version`.
#[must_use]
pub fn registry_key(chain: &str, job: &str, version: u32) -> String {
    format!("{chain}/{job}/{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauges_render_with_labels() {
        let metrics = JobMetrics::labelled("solana", "sandwich", 3);
        metrics.set_scan_height(100);
        metrics.set_lag(5);
        metrics.add_rows(40);
        metrics.add_rows(2);
        metrics.add_failure(false);
        let text = metrics.render();
        assert!(text.contains(
            "indexer_job_scan_height{chain=\"solana\",job=\"sandwich\",version=\"3\"} 100"
        ));
        assert!(
            text.contains("indexer_job_lag{chain=\"solana\",job=\"sandwich\",version=\"3\"} 5")
        );
        assert!(text.contains(
            "indexer_job_rows_written_total{chain=\"solana\",job=\"sandwich\",version=\"3\"} 42"
        ));
        assert!(text.contains(
            "indexer_job_failures_total{chain=\"solana\",job=\"sandwich\",version=\"3\"} 1"
        ));
        assert!(text.contains(
            "indexer_job_quarantined{chain=\"solana\",job=\"sandwich\",version=\"3\"} 0"
        ));
    }

    #[test]
    fn registry_tracks_versions() {
        let registry = JobMetricsRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry_key("c", "j", 1), "c/j/1");
        registry.register(
            registry_key("c", "j", 1),
            std::sync::Arc::new(JobMetrics::labelled("c", "j", 1)),
        );
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
        let text = registry.render();
        assert!(text.contains("indexer_job_scan_height"));
        registry.unregister("c/j/1");
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        assert!(registry.render().is_empty());
    }

    #[test]
    fn quarantine_flag_is_sticky() {
        let metrics = JobMetrics::labelled("c", "j", 1);
        metrics.add_failure(true);
        assert!(metrics.render().contains("indexer_job_quarantined"));
    }
}
