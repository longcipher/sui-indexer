-- Initial schema for Sui Indexer
-- Migration: 20250826000001_initial_schema

-- Checkpoint progress tracking
CREATE TABLE IF NOT EXISTS checkpoint_progress (
    id SERIAL PRIMARY KEY,
    checkpoint_sequence BIGINT NOT NULL UNIQUE,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Transactions table
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY DEFAULT GEN_RANDOM_UUID(),
    digest TEXT NOT NULL UNIQUE,
    checkpoint_sequence BIGINT NOT NULL,
    timestamp TIMESTAMP WITH TIME ZONE NOT NULL,
    gas_used BIGINT,
    success BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Events table
CREATE TABLE IF NOT EXISTS events (
    id UUID PRIMARY KEY DEFAULT GEN_RANDOM_UUID(),
    checkpoint_sequence BIGINT NOT NULL,
    transaction_digest TEXT NOT NULL,
    event_type TEXT NOT NULL,
    package_id TEXT NOT NULL,
    module_name TEXT NOT NULL,
    sender TEXT NOT NULL,
    fields JSONB NOT NULL,
    timestamp TIMESTAMP WITH TIME ZONE NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Performance indexes
CREATE INDEX IF NOT EXISTS idx_events_checkpoint
ON events (checkpoint_sequence);
CREATE INDEX IF NOT EXISTS idx_events_type
ON events (event_type);
CREATE INDEX IF NOT EXISTS idx_events_package
ON events (package_id);
CREATE INDEX IF NOT EXISTS idx_events_timestamp
ON events (timestamp);
CREATE INDEX IF NOT EXISTS idx_transactions_checkpoint
ON transactions (checkpoint_sequence);
CREATE INDEX IF NOT EXISTS idx_transactions_timestamp
ON transactions (timestamp);

-- Checkpoint progress index
CREATE INDEX IF NOT EXISTS idx_checkpoint_progress_sequence
ON checkpoint_progress (checkpoint_sequence);
