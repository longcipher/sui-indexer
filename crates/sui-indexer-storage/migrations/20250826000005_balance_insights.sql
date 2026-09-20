-- Balance insight tables derived from coin object changes
-- Migration: 20250826000005_balance_insights
--
-- coin_flows: one row per owned-coin object version observed (credit at the
-- checkpoint that created the version). Balances roll up from it; snapshots
-- are periodic pre-aggregations for hot holders/tokens.

CREATE TABLE IF NOT EXISTS coin_flows (
    checkpoint_sequence BIGINT NOT NULL,
    timestamp_ms BIGINT NOT NULL DEFAULT 0,
    transaction_digest TEXT NOT NULL,
    coin_type TEXT NOT NULL,
    holder TEXT NOT NULL,
    object_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    balance NUMERIC(78, 0) NOT NULL DEFAULT 0,
    PRIMARY KEY (object_id, version)
);
CREATE INDEX IF NOT EXISTS idx_coin_flows_holder
ON coin_flows (holder, checkpoint_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_coin_flows_coin
ON coin_flows (coin_type, checkpoint_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_coin_flows_checkpoint
ON coin_flows (checkpoint_sequence DESC);

CREATE TABLE IF NOT EXISTS balance_snapshots (
    holder TEXT NOT NULL,
    coin_type TEXT NOT NULL,
    balance NUMERIC(78, 0) NOT NULL DEFAULT 0,
    checkpoint_sequence BIGINT NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (holder, coin_type)
);
CREATE INDEX IF NOT EXISTS idx_balance_snapshots_coin
ON balance_snapshots (coin_type, balance DESC);

CREATE TABLE IF NOT EXISTS coin_metadata (
    coin_type TEXT PRIMARY KEY,
    first_seen_checkpoint BIGINT NOT NULL DEFAULT 0,
    last_seen_checkpoint BIGINT NOT NULL DEFAULT 0,
    flow_count BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
