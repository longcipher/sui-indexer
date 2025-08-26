-- Additional tables for Sui Indexer
-- Migration: 20250826000002_processed_tables

-- Indexer state table for checkpoint tracking
CREATE TABLE IF NOT EXISTS indexer_state (
    id SERIAL PRIMARY KEY,
    checkpoint_sequence BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP DEFAULT NOW()
);

-- Processed events table
CREATE TABLE IF NOT EXISTS processed_events (
    id UUID PRIMARY KEY,
    event_data JSONB NOT NULL,
    transaction_digest TEXT NOT NULL,
    checkpoint_sequence BIGINT NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    package_id TEXT NOT NULL,
    module_name TEXT NOT NULL,
    event_type TEXT NOT NULL,
    sender TEXT NOT NULL,
    fields JSONB NOT NULL,
    metadata JSONB NOT NULL,
    processed_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

-- Processed transactions table
CREATE TABLE IF NOT EXISTS processed_transactions (
    id UUID PRIMARY KEY,
    transaction_data JSONB NOT NULL,
    digest TEXT NOT NULL UNIQUE,
    checkpoint_sequence BIGINT NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    sender TEXT NOT NULL,
    gas_used BIGINT NOT NULL,
    status TEXT NOT NULL,
    effects JSONB NOT NULL,
    metadata JSONB NOT NULL,
    processed_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

-- Additional indexes for processed tables
CREATE INDEX IF NOT EXISTS idx_events_checkpoint_seq
ON processed_events (checkpoint_sequence);

CREATE INDEX IF NOT EXISTS idx_events_event_type
ON processed_events (event_type);

CREATE INDEX IF NOT EXISTS idx_events_package_module
ON processed_events (package_id, module_name);

CREATE INDEX IF NOT EXISTS idx_transactions_checkpoint_seq
ON processed_transactions (checkpoint_sequence);

CREATE INDEX IF NOT EXISTS idx_transactions_sender
ON processed_transactions (sender);
