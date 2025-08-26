use chrono::Utc;
use eyre::Result;
use serde_json::{Map, Value};
use sui_json_rpc_types::SuiEvent;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{EventMetadata, ProcessedEvent};

/// Event transformation logic for processing and enriching events
pub struct EventTransformer {
    include_raw_event: bool,
    extract_custom_fields: bool,
}

impl EventTransformer {
    /// Create a new event transformer with configuration
    pub fn new() -> Self {
        Self {
            include_raw_event: true,
            extract_custom_fields: true,
        }
    }

    /// Create a transformer with custom settings
    pub fn with_config(include_raw_event: bool, extract_custom_fields: bool) -> Self {
        Self {
            include_raw_event,
            extract_custom_fields,
        }
    }

    /// Transform a SuiEvent into a ProcessedEvent with additional metadata
    pub async fn transform_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        let start_time = Utc::now();

        // Extract and enhance event fields
        let fields = self.extract_event_fields(&event)?;

        // Create processed event with metadata
        let processed_event = ProcessedEvent {
            id: Uuid::new_v4(),
            event: event.clone(),
            transaction_digest: event.id.tx_digest,
            checkpoint_sequence: 0, // This would need to be provided from context
            timestamp: event
                .timestamp_ms
                .map(|ts| chrono::DateTime::from_timestamp_millis(ts as i64).unwrap_or(Utc::now()))
                .unwrap_or(Utc::now()),
            package_id: event.package_id,
            module_name: event.type_.module.to_string(),
            event_type: event.type_.name.to_string(),
            sender: event.sender.to_string(),
            fields,
            metadata: EventMetadata {
                processed_at: Utc::now(),
                processing_duration_ms: (Utc::now() - start_time).num_milliseconds() as u64,
                event_index: event.id.event_seq as usize,
                matched_filters: vec![], // This would be populated by the filter processor
                tags: self.extract_event_tags(&event),
            },
        };

        debug!(
            event_id = %processed_event.id,
            event_type = %processed_event.event_type,
            module = %processed_event.module_name,
            "Event transformed successfully"
        );

        Ok(processed_event)
    }

    /// Extract and structure event fields from the SuiEvent
    fn extract_event_fields(&self, event: &SuiEvent) -> Result<Value> {
        let mut fields = Map::new();

        // Include the parsed JSON if available
        if let Value::Object(parsed_map) = &event.parsed_json {
            for (key, value) in parsed_map {
                fields.insert(key.clone(), value.clone());
            }
        }

        // Add event metadata
        fields.insert(
            "package_id".to_string(),
            Value::String(event.package_id.to_string()),
        );
        fields.insert(
            "module".to_string(),
            Value::String(event.type_.module.to_string()),
        );
        fields.insert(
            "event_type".to_string(),
            Value::String(event.type_.name.to_string()),
        );
        fields.insert(
            "sender".to_string(),
            Value::String(event.sender.to_string()),
        );
        fields.insert(
            "transaction_digest".to_string(),
            Value::String(event.id.tx_digest.to_string()),
        );
        fields.insert(
            "event_sequence".to_string(),
            Value::Number(event.id.event_seq.into()),
        );

        // Add timestamp if available
        if let Some(timestamp_ms) = event.timestamp_ms {
            fields.insert(
                "timestamp_ms".to_string(),
                Value::Number(timestamp_ms.into()),
            );
        }

        // Include raw BCS data if configured and available
        if self.include_raw_event {
            // BcsEvent is likely a wrapper type, try to get the bytes
            let bcs_string = format!("{:?}", event.bcs);
            if !bcs_string.is_empty() && bcs_string != "Event([])" {
                fields.insert("bcs_data".to_string(), Value::String(bcs_string));
            }
        }

        // Extract protocol-specific fields if configured
        if self.extract_custom_fields {
            self.extract_protocol_specific_fields(event, &mut fields);
        }

        Ok(Value::Object(fields))
    }

    /// Extract protocol-specific fields for known protocols
    fn extract_protocol_specific_fields(&self, event: &SuiEvent, fields: &mut Map<String, Value>) {
        // Handle Navi Protocol events
        if self.is_navi_protocol_event(event) {
            self.extract_navi_fields(event, fields);
        }

        // Add more protocol handlers here as needed
        // if self.is_other_protocol_event(event) {
        //     self.extract_other_protocol_fields(event, fields);
        // }
    }

    /// Check if this is a Navi Protocol event
    fn is_navi_protocol_event(&self, event: &SuiEvent) -> bool {
        // This would need to be updated with actual Navi package IDs
        let navi_package_ids = [
            "0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5",
            // Add more Navi package IDs as needed
        ];

        navi_package_ids
            .iter()
            .any(|&package_id| event.package_id.to_string().starts_with(package_id))
    }

    /// Extract Navi Protocol specific fields
    fn extract_navi_fields(&self, event: &SuiEvent, fields: &mut Map<String, Value>) {
        fields.insert("protocol".to_string(), Value::String("navi".to_string()));

        // Handle different Navi event types
        match event.type_.name.as_str() {
            "DepositEvent" => {
                fields.insert("action".to_string(), Value::String("deposit".to_string()));
                self.extract_navi_deposit_fields(event, fields);
            }
            "WithdrawEvent" => {
                fields.insert("action".to_string(), Value::String("withdraw".to_string()));
                self.extract_navi_withdraw_fields(event, fields);
            }
            "BorrowEvent" => {
                fields.insert("action".to_string(), Value::String("borrow".to_string()));
                self.extract_navi_borrow_fields(event, fields);
            }
            "RepayEvent" => {
                fields.insert("action".to_string(), Value::String("repay".to_string()));
                self.extract_navi_repay_fields(event, fields);
            }
            _ => {
                // Generic Navi event
                fields.insert("action".to_string(), Value::String("unknown".to_string()));
            }
        }
    }

    /// Extract Navi deposit event specific fields
    fn extract_navi_deposit_fields(&self, event: &SuiEvent, fields: &mut Map<String, Value>) {
        if let Value::Object(parsed) = &event.parsed_json {
            // Extract common deposit fields based on Navi protocol structure
            if let Some(amount) = parsed.get("amount") {
                fields.insert("deposit_amount".to_string(), amount.clone());
            }

            if let Some(asset) = parsed.get("asset_id") {
                fields.insert("asset_id".to_string(), asset.clone());
            }

            if let Some(user) = parsed.get("user") {
                fields.insert("user_address".to_string(), user.clone());
            }

            if let Some(pool) = parsed.get("pool_id") {
                fields.insert("pool_id".to_string(), pool.clone());
            }

            // Add success indicator for deposit events
            fields.insert("success".to_string(), Value::Bool(true));
        }
    }

    /// Extract Navi withdraw event specific fields
    fn extract_navi_withdraw_fields(&self, _event: &SuiEvent, _fields: &mut Map<String, Value>) {
        // Similar to deposit fields but for withdrawals
        // Implementation would depend on actual Navi withdraw event structure
    }

    /// Extract Navi borrow event specific fields
    fn extract_navi_borrow_fields(&self, _event: &SuiEvent, _fields: &mut Map<String, Value>) {
        // Implementation would depend on actual Navi borrow event structure
    }

    /// Extract Navi repay event specific fields
    fn extract_navi_repay_fields(&self, _event: &SuiEvent, _fields: &mut Map<String, Value>) {
        // Implementation would depend on actual Navi repay event structure
    }

    /// Extract tags for categorizing events
    fn extract_event_tags(&self, event: &SuiEvent) -> Vec<String> {
        let mut tags = Vec::new();

        // Add protocol tag
        if self.is_navi_protocol_event(event) {
            tags.push("navi".to_string());
            tags.push("lending".to_string());
        }

        // Add event type tags
        match event.type_.name.as_str() {
            "DepositEvent" => tags.push("deposit".to_string()),
            "WithdrawEvent" => tags.push("withdraw".to_string()),
            "BorrowEvent" => tags.push("borrow".to_string()),
            "RepayEvent" => tags.push("repay".to_string()),
            _ => {}
        }

        // Add module-based tags
        tags.push(event.type_.module.to_string());

        tags
    }

    /// Batch transform multiple events
    pub async fn transform_events(&self, events: Vec<SuiEvent>) -> Result<Vec<ProcessedEvent>> {
        let original_count = events.len();
        let mut processed_events = Vec::with_capacity(events.len());

        for event in &events {
            match self.transform_event(event.clone()).await {
                Ok(processed) => processed_events.push(processed),
                Err(err) => {
                    warn!(error = %err, "Failed to transform event, skipping");
                    // Continue processing other events
                }
            }
        }

        debug!(
            original_count = original_count,
            processed_count = processed_events.len(),
            "Batch event transformation completed"
        );

        Ok(processed_events)
    }
}

impl Default for EventTransformer {
    fn default() -> Self {
        Self::new()
    }
}

/// Specialized transformers for specific protocols
pub mod protocol_transformers {
    use super::*;

    /// Transformer specifically for Navi Protocol events
    pub struct NaviEventTransformer {
        base_transformer: EventTransformer,
    }

    impl NaviEventTransformer {
        pub fn new() -> Self {
            Self {
                base_transformer: EventTransformer::with_config(true, true),
            }
        }

        /// Transform a Navi event with protocol-specific logic
        pub async fn transform_navi_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
            let mut processed = self.base_transformer.transform_event(event).await?;

            // Add Navi-specific metadata
            processed.metadata.tags.push("defi".to_string());
            processed.metadata.tags.push("navi-protocol".to_string());

            Ok(processed)
        }
    }

    impl Default for NaviEventTransformer {
        fn default() -> Self {
            Self::new()
        }
    }
}

// Tests will be implemented once the SuiEvent API is stable
