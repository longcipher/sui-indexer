# Sui Indexer Framework Examples

This directory contains examples showing how to use the Sui Indexer Framework programmatically.

## Examples

### 1. Simple Indexer (`simple_indexer.rs`)

A basic example that shows how to:
- Create a custom event processor
- Configure the indexer programmatically
- Monitor basic Sui events (coin events)

**Usage:**
```bash
cargo run --example simple_indexer -p sui-indexer-core
```

### 2. Custom DeFi Indexer (`custom_defi_indexer.rs`)

A comprehensive example demonstrating:
- Advanced custom event processing
- DeFi protocol monitoring (using Navi Protocol as example)
- Custom business logic implementation
- Event data extraction and analysis
- Multi-protocol support patterns

**Features:**
- ✅ Deposit event monitoring
- ✅ Borrow event tracking
- ✅ Withdrawal processing
- ✅ Loan repayment handling
- ✅ Liquidation alerts
- ✅ Portfolio tracking (placeholder)
- ✅ TVL calculations (placeholder)
- ✅ Risk monitoring (placeholder)

**Usage:**
```bash
cargo run --example custom_defi_indexer -p sui-indexer-core
```

## Running Examples

### Prerequisites

1. **Database Setup:**
   ```bash
   # Start PostgreSQL
   docker run --name postgres-sui \
     -e POSTGRES_PASSWORD=password \
     -e POSTGRES_DB=sui_indexer \
     -p 5433:5432 \
     -d postgres:15
   ```

2. **Environment Setup:**
   ```bash
   # Set database URL
   export SUI_INDEXER_DATABASE_URL="postgresql://postgres:password@localhost:5433/sui_indexer"
   ```

### Run Simple Example

```bash
cargo run --example simple_indexer -p sui-indexer-core
```

### Run DeFi Example

```bash
cargo run --example custom_defi_indexer -p sui-indexer-core
```

## Customization Guide

### Creating Your Own Event Processor

1. **Implement the EventProcessor trait:**

```rust
use sui_indexer_events::{EventProcessor, ProcessedEvent};
use sui_json_rpc_types::SuiEvent;
use async_trait::async_trait;
use eyre::Result;

pub struct MyCustomProcessor;

#[async_trait]
impl EventProcessor for MyCustomProcessor {
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        // Your custom logic here
        
        // Return processed event
        Ok(ProcessedEvent::from_sui_event(event))
    }
}
```

2. **Configure event filters:**

```rust
use sui_indexer_config::{IndexerConfig, EventFilter};

let mut config = IndexerConfig::default();
config.events.filters = vec![
    EventFilter {
        package: Some("your_package_id".to_string()),
        module: Some("your_module".to_string()),
        event_type: Some("YourEventType".to_string()),
        sender: None,
    }
];
```

3. **Start the indexer:**

```rust
let processor = Arc::new(MyCustomProcessor);
let indexer = IndexerCore::with_event_processor(config, processor).await?;
indexer.initialize().await?;
indexer.start().await?;
```

### Event Data Extraction

The examples show how to extract data from events:

```rust
// Extract string fields
if let Some(amount) = event.parsed_json.get("amount")
    .and_then(|v| v.as_str()) {
    println!("Amount: {}", amount);
}

// Extract numeric fields
if let Some(value) = event.parsed_json.get("value")
    .and_then(|v| v.as_number()) {
    println!("Value: {}", value);
}

// Extract nested objects
if let Some(user_data) = event.parsed_json.get("user")
    .and_then(|v| v.as_object()) {
    // Process user data
}
```

### Adding Custom Business Logic

The DeFi example shows patterns for:

- **Portfolio tracking**: Update user balances and positions
- **TVL calculations**: Calculate total value locked
- **Risk monitoring**: Track liquidation risks
- **Alert systems**: Send notifications for important events
- **Analytics**: Generate metrics and insights

### Protocol-Specific Processing

Structure your code to handle multiple protocols:

```rust
match package_id.as_str() {
    "0xprotocol1..." => handle_protocol1_event(&event).await?,
    "0xprotocol2..." => handle_protocol2_event(&event).await?,
    _ => handle_generic_event(&event).await?,
}
```

## Integration Patterns

### 1. Microservice Architecture

Use the framework as part of a larger system:

```rust
// Your main service
pub struct DeFiAnalyticsService {
    indexer: IndexerCore,
    database: Database,
    cache: Cache,
    api_server: ApiServer,
}

impl DeFiAnalyticsService {
    pub async fn start(&self) -> Result<()> {
        // Start indexer in background
        tokio::spawn(async move {
            self.indexer.start().await
        });
        
        // Start API server
        self.api_server.start().await
    }
}
```

### 2. Event-Driven Architecture

Forward processed events to other systems:

```rust
#[async_trait]
impl EventProcessor for EventForwarder {
    async fn process_event(&self, event: SuiEvent) -> Result<ProcessedEvent> {
        let processed = ProcessedEvent::from_sui_event(event);
        
        // Forward to event bus
        self.event_bus.publish(&processed).await?;
        
        // Forward to analytics pipeline
        self.analytics.process(&processed).await?;
        
        Ok(processed)
    }
}
```

### 3. Real-time Dashboards

Stream events to frontend applications:

```rust
// WebSocket handler
async fn handle_websocket(
    websocket: WebSocket,
    event_receiver: Receiver<ProcessedEvent>
) {
    while let Ok(event) = event_receiver.recv().await {
        // Send to frontend
        websocket.send(serde_json::to_string(&event)?).await?;
    }
}
```

## Best Practices

1. **Error Handling**: Always handle errors gracefully in your processors
2. **Performance**: Use async/await properly for I/O operations
3. **Logging**: Add comprehensive logging for debugging
4. **Testing**: Write unit tests for your event processors
5. **Configuration**: Make your processors configurable
6. **Monitoring**: Add metrics and health checks

## Support

For questions about these examples:
- Check the main README.md for general framework documentation
- Open an issue for bugs or feature requests
- Join discussions for implementation questions
