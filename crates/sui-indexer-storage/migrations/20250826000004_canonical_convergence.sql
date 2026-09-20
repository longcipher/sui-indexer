-- Converged canonical schema for Sui Indexer
-- Migration: 20250826000004_canonical_convergence
--
-- Fixes the P0 correctness gaps:
-- 1. checkpoints gains prev_digest/validator_sig/end_of_epoch + digest change
--    detection support (contiguous-commit reads digest per sequence).
-- 2. transactions gains sender/gas_price/error_message on the canonical table
--    so point lookups no longer depend on processed_transactions.
-- 3. events becomes the canonical decoded-event table with BCS bytes stored
--    as BYTEA (not Debug strings) plus tx/event ordering columns.
-- 4. indexer_progress replaces the ad-hoc indexer_state/checkpoint_progress
--    rows with a single triple-watermark row (continuous/floor + archive
--    interval + hot boundary).
-- 5. repair_queue is the durable queue for checkpoints that failed ingest.
-- 6. Invalid legacy unique index on processed_events(metadata JSONB) is
--    dropped and replaced with a (transaction_digest, checkpoint_sequence)
--    dedup key.

-- 1. Checkpoints: chain-continuity columns.
ALTER TABLE checkpoints
    ADD COLUMN IF NOT EXISTS prev_digest TEXT,
    ADD COLUMN IF NOT EXISTS validator_signature TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS end_of_epoch_data JSONB,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW();

-- 2. Transactions: real sender / gas / error on the canonical table.
ALTER TABLE transactions
    ADD COLUMN IF NOT EXISTS sender TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS gas_price BIGINT,
    ADD COLUMN IF NOT EXISTS error_message TEXT,
    ADD COLUMN IF NOT EXISTS effects JSONB;
ALTER TABLE transactions
    ALTER COLUMN timestamp SET DATA TYPE TIMESTAMP WITH TIME ZONE
    USING timestamp AT TIME ZONE 'UTC';

-- 3. Canonical decoded events table.
CREATE TABLE IF NOT EXISTS events_v2 (
    id UUID PRIMARY KEY DEFAULT GEN_RANDOM_UUID(),
    checkpoint_sequence BIGINT NOT NULL,
    transaction_digest TEXT NOT NULL,
    event_index BIGINT NOT NULL DEFAULT 0,
    package_id TEXT NOT NULL,
    module_name TEXT NOT NULL,
    event_type TEXT NOT NULL,
    sender TEXT NOT NULL,
    timestamp_ms BIGINT NOT NULL DEFAULT 0,
    bcs BYTEA,
    fields JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    UNIQUE (transaction_digest, event_index)
);

CREATE INDEX IF NOT EXISTS idx_events_v2_checkpoint
ON events_v2 (checkpoint_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_events_v2_package_module
ON events_v2 (package_id, module_name);
CREATE INDEX IF NOT EXISTS idx_events_v2_type
ON events_v2 (event_type);
CREATE INDEX IF NOT EXISTS idx_events_v2_sender
ON events_v2 (sender);
CREATE INDEX IF NOT EXISTS idx_events_v2_tx
ON events_v2 (transaction_digest);

-- 4. Converged progress row: continuous/floor + archive interval + hot boundary.
CREATE TABLE IF NOT EXISTS indexer_progress (
    pipeline TEXT PRIMARY KEY,
    continuous_checkpoint BIGINT NOT NULL DEFAULT 0,
    floor_checkpoint BIGINT NOT NULL DEFAULT 0,
    archive_lo BIGINT,
    archive_hi BIGINT,
    hot_boundary BIGINT,
    hot_boundary_ts TIMESTAMP WITH TIME ZONE,
    digest TEXT,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- 5. Durable repair queue with exponential backoff.
CREATE TABLE IF NOT EXISTS repair_queue (
    checkpoint_sequence BIGINT PRIMARY KEY,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    last_error TEXT,
    parked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_repair_queue_due
ON repair_queue (next_retry_at) WHERE parked = FALSE;

-- 6. Replace the invalid JSONB unique index with a scalar dedup key.
DROP INDEX IF EXISTS uq_processed_events_digest_seq;
CREATE UNIQUE INDEX IF NOT EXISTS uq_processed_events_digest_seq_v2
ON processed_events (transaction_digest, checkpoint_sequence);

-- Checkpoint digest lookup for contiguous-commit verification.
CREATE INDEX IF NOT EXISTS idx_checkpoints_sequence_digest
ON checkpoints (sequence_number, digest);

-- Canonical transaction point lookups.
CREATE INDEX IF NOT EXISTS idx_transactions_digest
ON transactions (digest);
CREATE INDEX IF NOT EXISTS idx_transactions_sender
ON transactions (sender, checkpoint_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_objects_tx
ON objects (transaction_digest);
