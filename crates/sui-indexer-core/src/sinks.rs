use eyre::Result;
use sui_indexer_config::WebhookSink;
use sui_indexer_events::ProcessedEvent;
use tracing::{info, warn};

/// Dispatch processed events to configured webhook sinks.
pub async fn dispatch_webhooks(
    sinks: &[WebhookSink],
    events: &[ProcessedEvent],
    client: &reqwest::Client,
) -> Result<usize> {
    let mut delivered = 0_usize;
    for sink in sinks {
        let matching: Vec<&ProcessedEvent> = events
            .iter()
            .filter(|event| {
                sink.packages.is_empty()
                    || sink
                        .packages
                        .iter()
                        .any(|package| event.package_id.to_string() == *package)
            })
            .collect();
        if matching.is_empty() {
            continue;
        }
        let payload = serde_json::json!({
            "sink": sink.name,
            "events": matching,
        });
        let mut request = client.post(&sink.url).json(&payload);
        if let Some(token) = &sink.bearer_token {
            request = request.bearer_auth(token);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                delivered = delivered.saturating_add(matching.len());
                info!("Delivered {} events to sink {}", matching.len(), sink.name);
            }
            Ok(response) => {
                warn!("Sink {} returned {}", sink.name, response.status());
            }
            Err(e) => {
                warn!("Failed to deliver to sink {}: {e}", sink.name);
            }
        }
    }
    Ok(delivered)
}

/// Evaluate threshold alert rules for one checkpoint.
pub fn evaluate_alerts(
    rules: &[sui_indexer_config::AlertRule],
    checkpoint: u64,
    events: &[ProcessedEvent],
) -> Vec<String> {
    let mut triggered = Vec::new();
    for rule in rules {
        let count = events
            .iter()
            .filter(|event| rule.package.is_empty() || event.package_id.to_string() == rule.package)
            .count() as u64;
        if count >= rule.min_events_per_checkpoint.max(1) {
            triggered.push(format!(
                "Alert {} at checkpoint {checkpoint}: {count} events",
                rule.name
            ));
        }
    }
    triggered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_alerts() {
        let rules = vec![sui_indexer_config::AlertRule {
            name: "burst".to_string(),
            package: String::new(),
            min_events_per_checkpoint: 2,
        }];
        assert!(evaluate_alerts(&rules, 1, &[]).is_empty());
    }
}
