-- Unified pipeline watermarks plus canonical checkpoint/object tables
-- Migration: 20250826000003_watermarks_canonical

-- Pipeline watermarks aligned with sui-indexer-alt-framework semantics.
CREATE TABLE IF NOT EXISTS pipeline_watermarks (
    pipeline TEXT PRIMARY KEY,
    epoch_hi_inclusive BIGINT NOT NULL DEFAULT 0,
    checkpoint_hi_inclusive BIGINT NOT NULL DEFAULT 0,
    tx_hi BIGINT NOT NULL DEFAULT 0,
    timestamp_ms_hi_inclusive BIGINT NOT NULL DEFAULT 0,
    reader_lo BIGINT NOT NULL DEFAULT 0,
    pruner_hi BIGINT NOT NULL DEFAULT 0,
    pruner_timestamp TIMESTAMP WITH TIME ZONE,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Canonical checkpoints table (one row per checkpoint).
CREATE TABLE IF NOT EXISTS checkpoints (
    sequence_number BIGINT PRIMARY KEY,
    digest TEXT NOT NULL,
    epoch BIGINT NOT NULL DEFAULT 0,
    timestamp_ms BIGINT NOT NULL DEFAULT 0,
    transaction_count BIGINT NOT NULL DEFAULT 0,
    network_total_transactions BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Canonical objects table (object changes per checkpoint).
CREATE TABLE IF NOT EXISTS objects (
    id UUID PRIMARY KEY,
    object_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    digest TEXT NOT NULL,
    checkpoint_sequence BIGINT NOT NULL,
    transaction_digest TEXT NOT NULL,
    sender TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    UNIQUE (object_id, version)
);

-- Deduplicate event and transaction writes for safe replays.
CREATE UNIQUE INDEX IF NOT EXISTS uq_processed_events_digest_seq
ON processed_events (transaction_digest, metadata);
CREATE UNIQUE INDEX IF NOT EXISTS uq_checkpoints_sequence
ON checkpoints (sequence_number);

-- Query-oriented indexes.
CREATE INDEX IF NOT EXISTS idx_checkpoints_epoch
ON checkpoints (epoch);
CREATE INDEX IF NOT EXISTS idx_checkpoints_timestamp
ON checkpoints (timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_objects_checkpoint
ON objects (checkpoint_sequence);
CREATE INDEX IF NOT EXISTS idx_objects_object_id
ON objects (object_id);
CREATE INDEX IF NOT EXISTS idx_objects_sender
ON objects (sender);
CREATE INDEX IF NOT EXISTS idx_processed_events_sender
ON processed_events (sender);
CREATE INDEX IF NOT EXISTS idx_processed_transactions_status
ON processed_transactions (status);
