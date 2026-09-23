-- Fix naive timestamps on the processed tables.
-- Migration: 20250826000008_tz_timestamps
--
-- `processed_events.timestamp` and `processed_transactions.timestamp` were
-- created as TIMESTAMP WITHOUT TIME ZONE, but every reader decodes them as
-- TIMESTAMPTZ. The canonical `transactions` table was already converted in
-- 00004; this finishes the job the same way.

ALTER TABLE processed_events
    ALTER COLUMN timestamp SET DATA TYPE TIMESTAMP WITH TIME ZONE
    USING timestamp AT TIME ZONE 'UTC';
ALTER TABLE processed_transactions
    ALTER COLUMN timestamp SET DATA TYPE TIMESTAMP WITH TIME ZONE
    USING timestamp AT TIME ZONE 'UTC';
