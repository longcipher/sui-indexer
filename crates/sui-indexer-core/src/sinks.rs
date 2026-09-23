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

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    async fn mock_sink(status: u16) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_server = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/hook", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let seen_server = seen_server.clone();
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(&mut socket);
                    let mut line = String::new();
                    let mut auth = String::new();
                    let mut length = 0usize;
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).await.is_err() || line == "\r\n" {
                            break;
                        }
                        let lower = line.to_ascii_lowercase();
                        if let Some(value) = lower.strip_prefix("authorization:") {
                            auth = value.trim().to_owned();
                        }
                        if let Some(value) = lower.strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; length];
                    if length > 0 {
                        let _ = reader.read_exact(&mut body).await;
                    }
                    seen_server
                        .lock()
                        .expect("mutex")
                        .push(format!("{auth}|{}", String::from_utf8_lossy(&body)));
                    let reason = if status == 200 { "OK" } else { "ERROR" };
                    let reply = format!(
                        "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    );
                    let _ = reader.into_inner().write_all(reply.as_bytes()).await;
                });
            }
        });
        (url, seen)
    }

    fn sink(name: &str, url: String, packages: Vec<String>, bearer: Option<String>) -> WebhookSink {
        WebhookSink {
            name: name.to_string(),
            url,
            packages,
            bearer_token: bearer,
        }
    }

    fn event(package: &str) -> ProcessedEvent {
        use sui_indexer_events::EventMetadata;
        ProcessedEvent {
            id: uuid::Uuid::new_v4(),
            event: sample_sui_event(package),
            transaction_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
            checkpoint_sequence: 1,
            timestamp: chrono::Utc::now(),
            package_id: package.parse().expect("package"),
            module_name: "coin".to_owned(),
            event_type: "Transfer".to_owned(),
            sender: "0x1".to_owned(),
            fields: serde_json::json!({}),
            metadata: EventMetadata {
                processed_at: chrono::Utc::now(),
                processing_duration_ms: 0,
                event_index: 0,
                matched_filters: vec![],
                tags: vec![],
            },
        }
    }

    fn sample_sui_event(package: &str) -> sui_json_rpc_types::SuiEvent {
        sui_json_rpc_types::SuiEvent {
            id: sui_types::event::EventID {
                tx_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
                event_seq: 0,
            },
            package_id: package.parse().expect("package"),
            transaction_module: "coin".parse().expect("module"),
            sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .expect("sender"),
            type_: format!("{package}::coin::Transfer").parse().expect("type"),
            parsed_json: serde_json::json!({}),
            bcs: sui_json_rpc_types::BcsEvent::new(vec![]),
            timestamp_ms: Some(0),
        }
    }

    #[tokio::test]
    async fn webhooks_filter_deliver_and_count() {
        let (ok_url, ok_seen) = mock_sink(200).await;
        let (fail_url, fail_seen) = mock_sink(500).await;
        let client = reqwest::Client::new();
        let events = vec![event(
            "0x0000000000000000000000000000000000000000000000000000000000000002",
        )];
        let delivered = dispatch_webhooks(
            &[
                sink(
                    "match",
                    ok_url.clone(),
                    vec![
                        "0x0000000000000000000000000000000000000000000000000000000000000002"
                            .to_string(),
                    ],
                    Some("secret".to_string()),
                ),
                sink(
                    "other-package",
                    ok_url.clone(),
                    vec![
                        "0x0000000000000000000000000000000000000000000000000000000000000003"
                            .to_string(),
                    ],
                    None,
                ),
                sink("failing", fail_url, vec![], None),
            ],
            &events,
            &client,
        )
        .await
        .expect("dispatch");
        // One matching sink at 200 (1 event), one skipped package, one 500.
        assert_eq!(delivered, 1);
        assert_eq!(ok_seen.lock().expect("mutex").len(), 1);
        assert!(ok_seen.lock().expect("mutex")[0].starts_with("bearer secret|"));
        assert_eq!(fail_seen.lock().expect("mutex").len(), 1);
    }

    #[test]
    fn alerts_trigger_on_thresholds() {
        let rules = vec![
            sui_indexer_config::AlertRule {
                name: "all".to_string(),
                package: String::new(),
                min_events_per_checkpoint: 2,
            },
            sui_indexer_config::AlertRule {
                name: "coin-only".to_string(),
                package: "0x0000000000000000000000000000000000000000000000000000000000000002"
                    .to_string(),
                min_events_per_checkpoint: 1,
            },
        ];
        let events = vec![
            event("0x0000000000000000000000000000000000000000000000000000000000000002"),
            event("0x0000000000000000000000000000000000000000000000000000000000000002"),
            event("0x0000000000000000000000000000000000000000000000000000000000000003"),
        ];
        let triggered = evaluate_alerts(&rules, 9, &events);
        assert_eq!(triggered.len(), 2);
        assert!(triggered[0].contains("all"));
        assert!(triggered[0].contains("checkpoint 9"));
        assert!(triggered[1].contains("coin-only"));

        // Below threshold stays quiet.
        let quiet = evaluate_alerts(&rules[..1], 9, &events[..1]);
        assert!(quiet.is_empty());
        // Package-scoped rule ignores other packages.
        let scoped = evaluate_alerts(
            &[sui_indexer_config::AlertRule {
                name: "sys".to_string(),
                package: "0x0000000000000000000000000000000000000000000000000000000000000009"
                    .to_string(),
                min_events_per_checkpoint: 1,
            }],
            9,
            &events,
        );
        assert!(scoped.is_empty());
    }
}
