use std::collections::HashSet;

use sui_indexer_config::EventFilter;
use sui_json_rpc_types::SuiEvent;
use sui_types::{
    Identifier,
    base_types::{ObjectID, SuiAddress},
};
use tracing::debug;

/// Event filtering logic for processing incoming events
pub struct EventFilterProcessor {
    filters: Vec<EventFilter>,
    package_filters: HashSet<ObjectID>,
    module_filters: HashSet<(ObjectID, Identifier)>,
    event_type_filters: HashSet<String>,
    sender_filters: HashSet<SuiAddress>,
}

impl EventFilterProcessor {
    /// Create a new event filter processor with the given filters
    pub fn new(filters: Vec<EventFilter>) -> Self {
        let mut processor = Self {
            filters: filters.clone(),
            package_filters: HashSet::new(),
            module_filters: HashSet::new(),
            event_type_filters: HashSet::new(),
            sender_filters: HashSet::new(),
        };

        // Pre-process filters for efficient matching
        processor.preprocess_filters(&filters);
        processor
    }

    /// Pre-process filters for efficient matching
    fn preprocess_filters(&mut self, filters: &[EventFilter]) {
        for filter in filters {
            if let Some(package) = &filter.package {
                if let Ok(package_id) = package.parse::<ObjectID>() {
                    self.package_filters.insert(package_id);
                }
            }

            if let (Some(package), Some(module)) = (&filter.package, &filter.module) {
                if let (Ok(package_id), Ok(module_name)) =
                    (package.parse::<ObjectID>(), module.parse::<Identifier>())
                {
                    self.module_filters.insert((package_id, module_name));
                }
            }

            if let Some(event_type) = &filter.event_type {
                self.event_type_filters.insert(event_type.clone());
            }

            if let Some(sender) = &filter.sender {
                if let Ok(sender_addr) = sender.parse::<SuiAddress>() {
                    self.sender_filters.insert(sender_addr);
                }
            }
        }
    }

    /// Check if an event should be processed based on configured filters
    pub fn should_process_event(&self, event: &SuiEvent) -> bool {
        // If no filters are configured, process all events
        if self.filters.is_empty() {
            return true;
        }

        // Check each filter - event must match at least one filter to be processed
        for filter in &self.filters {
            if self.event_matches_filter(event, filter) {
                debug!(
                    event_type = %event.type_,
                    package_id = %event.package_id,
                    filter = ?filter,
                    "Event matched filter"
                );
                return true;
            }
        }

        false
    }

    /// Check if an event matches a specific filter
    fn event_matches_filter(&self, event: &SuiEvent, filter: &EventFilter) -> bool {
        // Package filter
        if let Some(expected_package) = &filter.package {
            if let Ok(expected_id) = expected_package.parse::<ObjectID>() {
                if event.package_id != expected_id {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Module filter
        if let Some(expected_module) = &filter.module {
            if event.transaction_module.as_str() != expected_module {
                return false;
            }
        }

        // Event type filter: the bare Move name (`DepositEvent`) or any
        // textual struct path, compared structurally so short (`0x2::…`)
        // and canonical (`0x000…02::…`) forms both match.
        if let Some(expected_type) = &filter.event_type {
            if !event_type_matches(
                &event.package_id,
                event.transaction_module.as_str(),
                event.type_.name.as_str(),
                expected_type,
            ) {
                return false;
            }
        }

        // Sender filter
        if let Some(expected_sender) = &filter.sender {
            if let Ok(expected_addr) = expected_sender.parse::<SuiAddress>() {
                if event.sender != expected_addr {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }

    /// Get the configured filters
    pub fn filters(&self) -> &[EventFilter] {
        &self.filters
    }

    /// Check if the processor has any filters configured
    pub fn has_filters(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Get statistics about the filters
    pub fn filter_stats(&self) -> FilterStats {
        FilterStats {
            total_filters: self.filters.len(),
            package_filters: self.package_filters.len(),
            module_filters: self.module_filters.len(),
            event_type_filters: self.event_type_filters.len(),
            sender_filters: self.sender_filters.len(),
        }
    }
}

impl Default for EventFilterProcessor {
    fn default() -> Self {
        Self::new(vec![])
    }
}

/// Match an event type against a bare name or any textual struct path.
/// Both sides are compared structurally (address, module, name), so
/// `0x2::coin::Transfer` and its canonical `0x000…02::coin::Transfer` form
/// match the same event.
fn event_type_matches(
    package_id: &sui_types::base_types::ObjectID,
    module: &str,
    name: &str,
    expected: &str,
) -> bool {
    if name == expected {
        return true;
    }
    let Ok(sui_types::TypeTag::Struct(tag)) = expected.parse::<sui_types::TypeTag>() else {
        return false;
    };
    // Addresses compare normalized: `Display` pads differently across
    // `ObjectID` and `AccountAddress`, so `0x2` must equal `0x000…02`.
    norm_hex(&tag.address.to_string()) == norm_hex(&package_id.to_string())
        && tag.module.as_str() == module
        && tag.name.as_str() == name
}

/// Normalize a hex address for comparison (`0x` prefix and leading zeros off).
fn norm_hex(value: &str) -> String {
    value
        .trim_start_matches("0x")
        .trim_start_matches('0')
        .to_owned()
}

/// Statistics about configured filters
#[derive(Debug, Clone)]
pub struct FilterStats {
    pub total_filters: usize,
    pub package_filters: usize,
    pub module_filters: usize,
    pub event_type_filters: usize,
    pub sender_filters: usize,
}

/// Helper functions for creating common filters
pub mod common_filters {
    use super::*;

    /// Custom error type for filter creation
    #[derive(Debug, thiserror::Error)]
    pub enum FilterError {
        #[error("Invalid package ID: {0}")]
        InvalidPackageId(String),
        #[error("Invalid module name: {0}")]
        InvalidModuleName(String),
        #[error("Invalid sender address: {0}")]
        InvalidSenderAddress(String),
    }

    /// Create a filter for all events from a specific package
    pub fn package_events(package_id: &str) -> Result<EventFilter, FilterError> {
        // Validate the package ID can be parsed
        package_id
            .parse::<ObjectID>()
            .map_err(|_| FilterError::InvalidPackageId(package_id.to_string()))?;
        Ok(EventFilter {
            package: Some(package_id.to_string()),
            module: None,
            event_type: None,
            sender: None,
        })
    }

    /// Create a filter for events from a specific module
    pub fn module_events(package_id: &str, module_name: &str) -> Result<EventFilter, FilterError> {
        // Validate inputs
        package_id
            .parse::<ObjectID>()
            .map_err(|_| FilterError::InvalidPackageId(package_id.to_string()))?;
        module_name
            .parse::<Identifier>()
            .map_err(|_| FilterError::InvalidModuleName(module_name.to_string()))?;

        Ok(EventFilter {
            package: Some(package_id.to_string()),
            module: Some(module_name.to_string()),
            event_type: None,
            sender: None,
        })
    }

    /// Create a filter for events of a specific type
    pub fn event_type_filter(
        package_id: &str,
        module_name: &str,
        event_name: &str,
    ) -> Result<EventFilter, FilterError> {
        let event_type = format!("{package_id}::{module_name}::{event_name}");

        Ok(EventFilter {
            package: Some(package_id.to_string()),
            module: Some(module_name.to_string()),
            event_type: Some(event_type),
            sender: None,
        })
    }

    /// Create a filter for events from a specific sender
    pub fn sender_events(sender: &str) -> Result<EventFilter, FilterError> {
        // Validate the sender address can be parsed
        sender
            .parse::<SuiAddress>()
            .map_err(|_| FilterError::InvalidSenderAddress(sender.to_string()))?;

        Ok(EventFilter {
            package: None,
            module: None,
            event_type: None,
            sender: Some(sender.to_string()),
        })
    }

    /// Create filter for Navi Protocol lending events
    pub fn navi_lending_events(package_id: &str) -> Result<Vec<EventFilter>, FilterError> {
        Ok(vec![
            event_type_filter(package_id, "lending", "DepositEvent")?,
            event_type_filter(package_id, "lending", "WithdrawEvent")?,
            event_type_filter(package_id, "lending", "BorrowEvent")?,
            event_type_filter(package_id, "lending", "RepayEvent")?,
        ])
    }

    /// Create filter for Navi Protocol deposit events specifically
    pub fn navi_deposit_events(package_id: &str) -> Result<EventFilter, FilterError> {
        event_type_filter(package_id, "lending", "DepositEvent")
    }

    /// Create filter for Navi Protocol withdraw events specifically  
    pub fn navi_withdraw_events(package_id: &str) -> Result<EventFilter, FilterError> {
        event_type_filter(package_id, "lending", "WithdrawEvent")
    }
}

#[cfg(test)]
mod tests {
    use super::{common_filters::*, *};

    #[test]
    fn test_filter_processor_creation() {
        let processor = EventFilterProcessor::new(vec![]);
        assert_eq!(processor.filters().len(), 0);
        assert!(!processor.has_filters());
    }

    #[test]
    fn test_filter_processor_with_filters() {
        let filters = vec![EventFilter {
            package: Some("0x2".to_string()),
            module: None,
            event_type: None,
            sender: None,
        }];

        let processor = EventFilterProcessor::new(filters);
        assert_eq!(processor.filters().len(), 1);
        assert!(processor.has_filters());
    }

    #[test]
    fn test_common_filters() {
        let filter = package_events("0x2").unwrap();
        assert_eq!(filter.package, Some("0x2".to_string()));
        assert_eq!(filter.module, None);

        let filter = module_events("0x2", "coin").unwrap();
        assert_eq!(filter.package, Some("0x2".to_string()));
        assert_eq!(filter.module, Some("coin".to_string()));
    }

    #[test]
    fn test_navi_filters() {
        let filters = navi_lending_events("0xabc123").unwrap();
        assert_eq!(filters.len(), 4);

        let deposit_filter = navi_deposit_events("0xabc123").unwrap();
        assert!(deposit_filter.event_type.unwrap().contains("DepositEvent"));
    }

    fn sample_event() -> SuiEvent {
        SuiEvent {
            id: sui_types::event::EventID {
                tx_digest: sui_types::base_types::TransactionDigest::new([1; 32]),
                event_seq: 0,
            },
            package_id: "0x0000000000000000000000000000000000000000000000000000000000000002"
                .parse()
                .expect("package"),
            transaction_module: "coin".parse().expect("module"),
            sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .expect("sender"),
            type_: "0x2::coin::Transfer".parse().expect("type"),
            parsed_json: serde_json::json!({}),
            bcs: sui_json_rpc_types::BcsEvent::new(vec![1, 2, 3]),
            timestamp_ms: Some(1000),
        }
    }

    #[test]
    fn event_type_matches_bare_and_qualified_names() {
        let event = sample_event();
        let bare = EventFilterProcessor::new(vec![EventFilter {
            package: None,
            module: None,
            event_type: Some("Transfer".to_string()),
            sender: None,
        }]);
        assert!(bare.should_process_event(&event));
        let qualified = EventFilterProcessor::new(vec![EventFilter {
            package: None,
            module: None,
            event_type: Some(format!(
                "{}::{}::{}",
                event.package_id, event.transaction_module, event.type_.name
            )),
            sender: None,
        }]);
        assert!(qualified.should_process_event(&event));
        // Short package form matches the same event (structural compare).
        let short = EventFilterProcessor::new(vec![EventFilter {
            package: None,
            module: None,
            event_type: Some("0x2::coin::Transfer".to_string()),
            sender: None,
        }]);
        assert!(short.should_process_event(&event));
        // Each path component gates independently.
        for bad in [
            "0x3::coin::Transfer",
            "0x2::vault::Transfer",
            "0x2::coin::Other",
        ] {
            let rejected = EventFilterProcessor::new(vec![EventFilter {
                package: None,
                module: None,
                event_type: Some(bad.to_string()),
                sender: None,
            }]);
            assert!(!rejected.should_process_event(&event), "{bad}");
        }
        let other = EventFilterProcessor::new(vec![EventFilter {
            package: None,
            module: None,
            event_type: Some("DepositEvent".to_string()),
            sender: None,
        }]);
        assert!(!other.should_process_event(&event));
    }
}

#[cfg(test)]
mod mismatch_tests {
    use super::*;
    use crate::test_support::sample_event;

    fn processor_with(filter: EventFilter) -> EventFilterProcessor {
        EventFilterProcessor::new(vec![filter])
    }

    #[test]
    fn package_mismatch_rejects() {
        let event = sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({}));
        let same = processor_with(EventFilter {
            package: Some("0x2".to_string()),
            module: None,
            event_type: None,
            sender: None,
        });
        assert!(same.should_process_event(&event));
        let other = processor_with(EventFilter {
            package: Some("0x3".to_string()),
            module: None,
            event_type: None,
            sender: None,
        });
        assert!(!other.should_process_event(&event));
        let invalid = processor_with(EventFilter {
            package: Some("not-an-id".to_string()),
            module: None,
            event_type: None,
            sender: None,
        });
        assert!(!invalid.should_process_event(&event));
    }

    #[test]
    fn module_mismatch_rejects() {
        let event = sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({}));
        let same = processor_with(EventFilter {
            package: None,
            module: Some("coin".to_string()),
            event_type: None,
            sender: None,
        });
        assert!(same.should_process_event(&event));
        let other = processor_with(EventFilter {
            package: None,
            module: Some("vault".to_string()),
            event_type: None,
            sender: None,
        });
        assert!(!other.should_process_event(&event));
    }

    #[test]
    fn sender_mismatch_rejects() {
        let event = sample_event(1, "0x2", "coin", "Transfer", serde_json::json!({}));
        let same = processor_with(EventFilter {
            package: None,
            module: None,
            event_type: None,
            sender: Some(
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            ),
        });
        assert!(same.should_process_event(&event));
        let other = processor_with(EventFilter {
            package: None,
            module: None,
            event_type: None,
            sender: Some(
                "0x0000000000000000000000000000000000000000000000000000000000000002".to_string(),
            ),
        });
        assert!(!other.should_process_event(&event));
        let invalid = processor_with(EventFilter {
            package: None,
            module: None,
            event_type: None,
            sender: Some("bogus".to_string()),
        });
        assert!(!invalid.should_process_event(&event));
    }

    #[test]
    fn preprocessed_stats_reflect_configured_filters() {
        let processor = EventFilterProcessor::new(vec![EventFilter {
            package: Some("0x2".to_string()),
            module: Some("coin".to_string()),
            event_type: Some("Transfer".to_string()),
            sender: Some(
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            ),
        }]);
        let stats = processor.filter_stats();
        assert_eq!(stats.total_filters, 1);
        assert_eq!(stats.package_filters, 1);
        assert_eq!(stats.module_filters, 1);
        assert_eq!(stats.event_type_filters, 1);
        assert_eq!(stats.sender_filters, 1);
    }
}
