use std::collections::HashMap;
use std::sync::Arc;

use eyre::Result;
use sui_indexer_config::ProtocolPreset;
use sui_json_rpc_types::SuiEvent;
use tracing::info;

/// A named protocol with package allowlist and tags.
#[derive(Debug, Clone)]
pub struct Protocol {
    /// Protocol name.
    pub name: String,
    /// Package IDs belonging to the protocol.
    pub packages: Vec<String>,
    /// Tags attached to matched events.
    pub tags: Vec<String>,
}

impl From<ProtocolPreset> for Protocol {
    fn from(preset: ProtocolPreset) -> Self {
        Self {
            name: preset.name,
            packages: preset.packages,
            tags: preset.tags,
        }
    }
}

/// Registry mapping package IDs to protocols, replacing hardcoded
/// single-protocol branches with data-driven presets.
#[derive(Debug, Clone, Default)]
pub struct ProtocolRegistry {
    protocols: Vec<Protocol>,
    package_index: HashMap<String, usize>,
}

impl ProtocolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with built-in DeFi presets.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Protocol {
            name: "navi".to_string(),
            packages: vec![
                "0x81c408448d0d57b3e371ea94de1d40bf852784d3e225de1e74acab3e8395c18f".to_string(),
                "0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5".to_string(),
            ],
            tags: vec!["defi".to_string(), "lending".to_string()],
        });
        registry.register(Protocol {
            name: "sui_system".to_string(),
            packages: vec!["0x3".to_string()],
            tags: vec!["system".to_string()],
        });
        registry.register(Protocol {
            name: "coin".to_string(),
            packages: vec!["0x2".to_string()],
            tags: vec!["coin".to_string()],
        });
        registry
    }

    /// Create a registry from TOML protocol presets.
    pub fn from_presets(presets: Vec<ProtocolPreset>) -> Self {
        let mut registry = Self::with_defaults();
        for preset in presets {
            registry.register(Protocol::from(preset));
        }
        registry
    }

    /// Register a protocol and index its packages.
    pub fn register(&mut self, protocol: Protocol) {
        let index = self.protocols.len();
        for package in &protocol.packages {
            self.package_index.insert(normalize_package(package), index);
        }
        self.protocols.push(protocol);
    }

    /// Look up the protocol for a package ID.
    pub fn lookup(&self, package_id: &str) -> Option<&Protocol> {
        self.package_index
            .get(&normalize_package(package_id))
            .and_then(|index| self.protocols.get(*index))
    }

    /// Tags for an event: protocol tags plus action tags.
    pub fn tags_for_event(&self, event: &SuiEvent) -> Vec<String> {
        let mut tags = Vec::new();
        if let Some(protocol) = self.lookup(&event.package_id.to_string()) {
            tags.push(protocol.name.clone());
            tags.extend(protocol.tags.clone());
        }
        match event.type_.name.as_str() {
            "DepositEvent" => tags.push("deposit".to_string()),
            "WithdrawEvent" => tags.push("withdraw".to_string()),
            "BorrowEvent" => tags.push("borrow".to_string()),
            "RepayEvent" => tags.push("repay".to_string()),
            _ => {}
        }
        tags.push(event.type_.module.to_string());
        tags
    }
}

/// Normalize package IDs for comparison (`0x2` vs full 32-byte form).
fn normalize_package(package: &str) -> String {
    package
        .trim_start_matches("0x")
        .trim_start_matches('0')
        .to_string()
}

/// Trait for protocol-specific event handlers.
#[async_trait::async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol name.
    fn name(&self) -> &str;
    /// Handle a matched event.
    async fn handle(&self, event: &SuiEvent) -> Result<()>;
}

/// Logging handler used as the default protocol implementation.
pub struct LoggingProtocolHandler {
    name: String,
}

impl LoggingProtocolHandler {
    /// Create a logging handler for a protocol.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait::async_trait]
impl ProtocolHandler for LoggingProtocolHandler {
    fn name(&self) -> &str {
        &self.name
    }

    async fn handle(&self, event: &SuiEvent) -> Result<()> {
        info!(
            "Protocol {} event {} from {}",
            self.name, event.type_.name, event.sender
        );
        Ok(())
    }
}

/// Router dispatching events to protocol handlers by package ID.
#[derive(Default)]
pub struct ProtocolRouter {
    handlers: HashMap<String, Arc<dyn ProtocolHandler>>,
}

impl ProtocolRouter {
    /// Create an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for a package ID.
    pub fn register(&mut self, package: impl Into<String>, handler: Arc<dyn ProtocolHandler>) {
        self.handlers
            .insert(normalize_package(&package.into()), handler);
    }

    /// Dispatch an event to its protocol handler.
    pub async fn dispatch(&self, event: &SuiEvent) -> Result<bool> {
        let key = normalize_package(&event.package_id.to_string());
        if let Some(handler) = self.handlers.get(&key) {
            handler.handle(event).await?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_lookup() {
        let registry = ProtocolRegistry::with_defaults();
        assert!(registry.lookup("0x2").is_some());
        assert!(registry.lookup("0x3").is_some());
        assert!(registry.lookup("0xdead").is_none());
    }

    #[test]
    fn test_registry_tags() {
        use sui_json_rpc_types::BcsEvent;

        let event = SuiEvent {
            id: sui_types::event::EventID {
                tx_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
                event_seq: 1,
            },
            package_id: "0x0000000000000000000000000000000000000000000000000000000000000002"
                .parse()
                .expect("package"),
            transaction_module: "coin".parse().expect("module"),
            sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .expect("sender"),
            type_: "0x2::coin::CoinEvent".parse().expect("type"),
            parsed_json: serde_json::json!({}),
            bcs: BcsEvent::new(vec![1, 2, 3]),
            timestamp_ms: Some(1000),
        };
        let registry = ProtocolRegistry::with_defaults();
        let tags = registry.tags_for_event(&event);
        assert!(tags.contains(&"coin".to_string()));
    }
}

#[cfg(test)]
mod routing_tests {
    use super::super::test_support::{navi_deposit_event, sample_event};
    use super::*;

    #[test]
    fn presets_extend_the_defaults() {
        let registry = ProtocolRegistry::from_presets(vec![ProtocolPreset {
            name: "custom".to_string(),
            packages: vec!["0xbeef".to_string()],
            tags: vec!["t".to_string()],
        }]);
        let found = registry.lookup("0xbeef").expect("custom");
        assert_eq!(found.name, "custom");
        assert!(registry.lookup("0x2").is_some());
    }

    #[test]
    fn action_tags_cover_all_lending_actions() {
        let registry = ProtocolRegistry::with_defaults();
        for (name, action) in [
            ("DepositEvent", "deposit"),
            ("WithdrawEvent", "withdraw"),
            ("BorrowEvent", "borrow"),
            ("RepayEvent", "repay"),
        ] {
            let event = sample_event(1, "0x2", "coin", name, serde_json::json!({}));
            let tags = registry.tags_for_event(&event);
            assert!(tags.contains(&action.to_string()), "{name}");
            assert!(tags.contains(&"coin".to_string()));
        }
    }

    #[test]
    fn unknown_actions_carry_no_action_tag() {
        let registry = ProtocolRegistry::with_defaults();
        let event = sample_event(1, "0x2", "coin", "Mint", serde_json::json!({}));
        let tags = registry.tags_for_event(&event);
        assert!(!tags.contains(&"deposit".to_string()));
        assert!(tags.contains(&"coin".to_string()));
    }

    #[test]
    fn navi_deposit_carries_protocol_tags() {
        let registry = ProtocolRegistry::with_defaults();
        let tags = registry.tags_for_event(&navi_deposit_event());
        assert!(tags.contains(&"navi".to_string()));
        assert!(tags.contains(&"defi".to_string()));
        assert!(tags.contains(&"deposit".to_string()));
    }

    #[tokio::test]
    async fn router_dispatches_by_package() {
        let mut router = ProtocolRouter::new();
        router.register("0x2", Arc::new(LoggingProtocolHandler::new("coin-handler")));
        let event = sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({}));
        assert!(router.dispatch(&event).await.expect("dispatch"));
        let other = sample_event(1, "0x9", "m", "E", serde_json::json!({}));
        assert!(!router.dispatch(&other).await.expect("dispatch"));
    }

    #[test]
    fn handler_reports_its_name() {
        let handler = LoggingProtocolHandler::new("coin-handler");
        assert_eq!(handler.name(), "coin-handler");
    }
}
