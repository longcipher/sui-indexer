-- Multichain job-engine control plane and skeleton tables.
-- Migration: 20250826000006_job_engine
--
-- 1. Skeleton tables (chain-independent): blocks / txs / chain_events.
--    Chain-specific columns never land here; they live in native tables
--    or the chain_meta / extra JSONB columns.
-- 2. Control plane: jobs / job_versions / job_cursors / catalog_objects /
--    feeds / work_queue. A job is data, not a process.

-- 1a. Skeleton blocks: height is the universal cursor (block number, slot,
-- checkpoint sequence). Parent link covers EVM parent_hash, SVM
-- parent_slot + previous_blockhash, Sui prev_digest.
CREATE TABLE IF NOT EXISTS blocks (
    chain_id    TEXT NOT NULL,
    height      BIGINT NOT NULL,
    hash        BYTEA NOT NULL,
    parent_hash BYTEA NOT NULL DEFAULT '\x',
    parent_ref  JSONB NOT NULL DEFAULT '{}',
    ts          TIMESTAMP WITH TIME ZONE NOT NULL,
    commitment  SMALLINT NOT NULL DEFAULT 2,
    skipped     BOOLEAN NOT NULL DEFAULT FALSE,
    chain_meta  JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, height)
) PARTITION BY LIST (chain_id);
-- Default partition keeps single-chain deployments working with no extra DDL.
CREATE TABLE IF NOT EXISTS blocks_default PARTITION OF blocks DEFAULT;

CREATE INDEX IF NOT EXISTS idx_blocks_chain_ts
ON blocks (chain_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_blocks_chain_commitment
ON blocks (chain_id, commitment);

-- 1b. Skeleton transactions.
CREATE TABLE IF NOT EXISTS txs (
    chain_id   TEXT NOT NULL,
    height     BIGINT NOT NULL,
    block_ts   TIMESTAMP WITH TIME ZONE NOT NULL,
    tx_index   INTEGER NOT NULL,
    tx_hash    BYTEA NOT NULL,
    sender     TEXT NOT NULL DEFAULT '',
    success    BOOLEAN NOT NULL DEFAULT TRUE,
    -- Per-transaction fee. Real fees fit comfortably; the adapter clamps
    -- the i128 accounting value into range on write.
    fee        BIGINT NOT NULL DEFAULT 0,
    chain_meta JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, height, tx_index)
) PARTITION BY LIST (chain_id);
CREATE TABLE IF NOT EXISTS txs_default PARTITION OF txs DEFAULT;

CREATE INDEX IF NOT EXISTS idx_txs_chain_sender
ON txs (chain_id, sender, height DESC);

-- 1c. Universal event projection: every rule reads this table.
CREATE TABLE IF NOT EXISTS chain_events (
    chain_id     TEXT NOT NULL,
    height       BIGINT NOT NULL,
    block_ts     TIMESTAMP WITH TIME ZONE NOT NULL,
    tx_index     INTEGER NOT NULL,
    ev_index     INTEGER NOT NULL,
    inner_ix     INTEGER NOT NULL DEFAULT 0,
    stack_height INTEGER NOT NULL DEFAULT 0,
    emitter      TEXT NOT NULL,
    topics       TEXT[] NOT NULL DEFAULT '{}',
    payload      BYTEA NOT NULL DEFAULT '\x',
    tx_hash      BYTEA NOT NULL DEFAULT '\x',
    sender       TEXT NOT NULL DEFAULT '',
    extra        JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, height, tx_index, ev_index, inner_ix)
) PARTITION BY LIST (chain_id);
CREATE TABLE IF NOT EXISTS chain_events_default PARTITION OF chain_events DEFAULT;

CREATE INDEX IF NOT EXISTS idx_chain_events_emitter
ON chain_events (chain_id, emitter, height DESC);
CREATE INDEX IF NOT EXISTS idx_chain_events_height
ON chain_events (chain_id, height DESC);

-- 2a. Job registry: desired state per (chain, job).
CREATE TABLE IF NOT EXISTS jobs (
    chain_id   TEXT NOT NULL,
    name       TEXT NOT NULL,
    spec       JSONB NOT NULL,
    spec_hash  TEXT NOT NULL,
    desired    TEXT NOT NULL DEFAULT 'active',
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, name)
);

-- 2b. Job versions: immutable per version; a logic change creates a new row
-- and the scheduler builds it, then swaps the alias on catch-up.
CREATE TABLE IF NOT EXISTS job_versions (
    chain_id     TEXT NOT NULL,
    name         TEXT NOT NULL,
    version      INTEGER NOT NULL,
    spec_hash    TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'draft',
    scan_from    BIGINT NOT NULL,
    scan_to      BIGINT,
    scan_cursor  BIGINT,
    rows_written BIGINT NOT NULL DEFAULT 0,
    last_error   TEXT,
    started_at   TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    finished_at  TIMESTAMP WITH TIME ZONE,
    updated_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, name, version)
);

-- 2c. Job cursors: per-version scan progress (the archive cursor means a
-- retry resumes rather than restarts).
CREATE TABLE IF NOT EXISTS job_cursors (
    chain_id  TEXT NOT NULL,
    name      TEXT NOT NULL,
    version   INTEGER NOT NULL,
    cursor    BIGINT NOT NULL DEFAULT 0,
    tip       BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, name, version)
);

-- 2d. Dynamic catalog: replaces the compile-time catalog. Every consumer
-- (reorg pruning, public query visibility, backfill policy) reads this table.
CREATE TABLE IF NOT EXISTS catalog_objects (
    chain_id     TEXT NOT NULL,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    ddl          TEXT NOT NULL,
    select_sql   TEXT,
    checksum     TEXT NOT NULL,
    public       BOOLEAN NOT NULL DEFAULT FALSE,
    block_column TEXT,
    reorg_mode   TEXT NOT NULL DEFAULT 'block_scoped',
    owner_job    TEXT,
    backfill     TEXT NOT NULL DEFAULT 'none',
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, name)
);
CREATE INDEX IF NOT EXISTS idx_catalog_public
ON catalog_objects (chain_id) WHERE public;

-- 2e. External feeds (CEX prices, …) with window/cursor semantics.
CREATE TABLE IF NOT EXISTS feeds (
    chain_id   TEXT NOT NULL,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    settings   JSONB NOT NULL DEFAULT '{}',
    cursor     BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (chain_id, name)
);

-- 2f. Generalised work queue: ranged units of work claimed with SKIP LOCKED.
-- Repair rows reference the legacy repair_queue; job scan ranges live here.
CREATE TABLE IF NOT EXISTS work_queue (
    id          BIGSERIAL PRIMARY KEY,
    chain_id    TEXT NOT NULL,
    job_name    TEXT NOT NULL,
    job_version INTEGER NOT NULL DEFAULT 0,
    range_lo    BIGINT NOT NULL,
    range_hi    BIGINT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    last_error  TEXT,
    claimed_at  TIMESTAMP WITH TIME ZONE,
    done        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_work_queue_claim
ON work_queue (chain_id, job_name, done, next_retry_at)
WHERE done = FALSE;
