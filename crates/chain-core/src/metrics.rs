//! Metric names and label helpers.
//!
//! Per-chain and per-job labels are mandatory: dashboards that aggregate
//! without `chain` / `job` labels are wrong in a multichain deployment.

/// Label set attached to every job-scoped metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricLabels {
    /// Chain key, e.g. `sui-mainnet`.
    pub chain: String,
    /// Job name, e.g. `sandwich`.
    pub job: String,
    /// Job version.
    pub version: u32,
}

impl MetricLabels {
    /// Render as Prometheus label suffix: `{chain="…",job="…",version="…"}`.
    #[must_use]
    pub fn prometheus_suffix(&self) -> String {
        format!(
            "{{chain=\"{}\",job=\"{}\",version=\"{}\"}}",
            self.chain, self.job, self.version
        )
    }
}

/// Metric names owned by the job engine.
#[must_use]
pub fn metric_name(kind: &str) -> String {
    format!("indexer_{kind}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_render_prometheus_suffix() {
        let labels = MetricLabels {
            chain: "solana".to_owned(),
            job: "sandwich".to_owned(),
            version: 3,
        };
        assert_eq!(
            labels.prometheus_suffix(),
            r#"{chain="solana",job="sandwich",version="3"}"#
        );
    }

    #[test]
    fn metric_names_are_namespaced() {
        assert_eq!(metric_name("job_lag"), "indexer_job_lag");
    }
}
