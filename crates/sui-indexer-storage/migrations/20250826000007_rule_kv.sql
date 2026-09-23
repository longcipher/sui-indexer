-- Durable height-scoped KV for stateful rules (pool reserves, …).
-- Migration: 20250826000007_rule_kv
--
-- Every write is tagged with a height, so rollback is a range delete.
-- The in-memory MemoryKv mirrors this layout for offline runs.

CREATE TABLE IF NOT EXISTS rule_kv (
    chain_id   TEXT NOT NULL,
    rule       TEXT NOT NULL,
    key        TEXT NOT NULL,
    height     BIGINT NOT NULL,
    value      BYTEA NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, rule, key, height)
);

CREATE INDEX IF NOT EXISTS idx_rule_kv_lookup
ON rule_kv (chain_id, rule, key, height DESC);
