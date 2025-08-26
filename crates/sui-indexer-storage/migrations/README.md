# Database Migrations

This directory contains database migration files for the Sui Indexer project.

## Overview

We use SQLx's built-in migration functionality instead of embedding SQL directly in Rust code. This provides several benefits:

- Version control for database schema changes
- Rollback capabilities
- Better separation of concerns
- Standard SQLx tooling support

## Migration Files

Each migration file follows the naming convention: `YYYYMMDDHHMMSS_description.sql`

### Current Migrations

1. `20250826000001_initial_schema.sql` - Creates basic tables for checkpoints, transactions, and events
2. `20250826000002_processed_tables.sql` - Creates additional processed events and transactions tables

## Usage

### Running Migrations

Migrations are automatically run when the application starts up through the `run_migrations()` function in `crates/sui-indexer-storage/src/migrations.rs`.

### Creating New Migrations

1. Create a new file in this directory with the appropriate timestamp and description
2. Add your SQL schema changes
3. The migration will be picked up automatically by the application

### Manual Migration Management

You can also use the `sqlx-cli` tool to manage migrations manually:

```bash
# Install sqlx-cli
cargo install sqlx-cli

# Run migrations manually (requires DATABASE_URL environment variable)
sqlx migrate run

# Revert the last migration
sqlx migrate revert
```

## Best Practices

1. Always use `IF NOT EXISTS` clauses for tables and indexes to ensure idempotency
2. Use descriptive migration names
3. Test migrations on a copy of production data before applying
4. Keep migrations small and focused on specific changes

## Database Schema

The current schema includes:

- `checkpoint_progress` - Tracks checkpoint synchronization progress
- `transactions` - Stores transaction data
- `events` - Stores event data
- `indexer_state` - Application state tracking
- `processed_events` - Processed events with metadata
- `processed_transactions` - Processed transactions with metadata

All tables include appropriate indexes for performance optimization.
