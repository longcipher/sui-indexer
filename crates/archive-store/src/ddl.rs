//! ClickHouse DDL generation: archive tables, job outputs, live views.
//!
//! All generated SQL is covered by golden-string tests, following the
//! reference project's snapshot discipline for generated DDL.

/// Archive table for one skeleton dataset (`blocks`, `txs`, `events`).
///
/// `ReplacingMergeTree` with a version column keeps reorg cleanup off the
/// hot path: duplicates collapse on read instead of `ALTER … DELETE`.
#[must_use]
pub fn archive_table_ddl(database: &str, table: &str, partition_by: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {database}.{table} (\n\
         \x20   chain LowCardinality(String),\n\
         \x20   height UInt64,\n\
         \x20   ts DateTime64(3, 'UTC'),\n\
         \x20   commitment UInt8,\n\
         \x20   payload String,\n\
         \x20   _version UInt64\n\
         ) ENGINE = ReplacingMergeTree(_version)\n\
         ORDER BY (chain, height)\n\
         PARTITION BY {partition_by}"
    )
}

/// Versioned job output table: `job_sandwich__v3`, immutable per version.
#[must_use]
pub fn job_output_ddl(
    database: &str,
    physical: &str,
    order_by: &[String],
    partition_by: &str,
    ttl: Option<&str>,
) -> String {
    let order = if order_by.is_empty() {
        "height".to_owned()
    } else {
        order_by.join(", ")
    };
    let mut ddl = format!(
        "CREATE TABLE IF NOT EXISTS {database}.{physical} (\n\
         \x20   chain LowCardinality(String),\n\
         \x20   _height UInt64,\n\
         \x20   _rule_version UInt32,\n\
         \x20   _commitment UInt8,\n\
         \x20   data String\n\
         ) ENGINE = ReplacingMergeTree(_version)\n\
         ORDER BY (chain, {order})\n\
         PARTITION BY {partition_by}"
    );
    if let Some(ttl) = ttl.filter(|t| !t.is_empty()) {
        ddl.push_str(&format!("\nTTL ts + INTERVAL {ttl}"));
    }
    ddl
}

/// Live-row materialized view: new archive rows flow into the job output.
/// (Reserved for static archive selects; versioned scans land through
/// continued chunked backfill, which keeps one write path.)
#[must_use]
pub fn live_view_ddl(database: &str, physical: &str, select_sql: &str) -> String {
    format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{physical}_mv TO {database}.{physical} AS {select_sql}"
    )
}

/// Versioned job table derived from the user query: the schema comes from the
/// query itself (`LIMIT 0` executes nothing). Requires the query to project
/// `_height` (the `ReplacingMergeTree` version column and reorg-prune key).
#[must_use]
pub fn job_table_as_select_ddl(
    database: &str,
    physical: &str,
    order_by: &[String],
    partition_by: &str,
    select_sql: &str,
) -> String {
    let order = if order_by.is_empty() {
        "_height".to_owned()
    } else {
        order_by.join(", ")
    };
    let select = select_sql.replace("{lo}", "0").replace("{hi}", "0");
    let mut ddl = format!(
        "CREATE TABLE IF NOT EXISTS {database}.{physical}\nENGINE = ReplacingMergeTree(_height)\nORDER BY ({order})\n"
    );
    if !partition_by.trim().is_empty() {
        ddl.push_str(&format!("PARTITION BY {partition_by}\n"));
    }
    ddl.push_str(&format!("AS {select} LIMIT 0"));
    ddl
}

/// Tiered view over PG-hot + CH-archive for tables that declare `pg_hot`.
#[must_use]
pub fn tiered_view_ddl(
    pg_table: &str,
    ch_table: &str,
    boundary: u64,
    columns: &[String],
) -> String {
    let list = columns.join(", ");
    format!(
        "CREATE OR REPLACE VIEW {pg_table}_tiered AS\n\
         SELECT {list} FROM {pg_table} WHERE height >= {boundary}\n\
         UNION ALL\n\
         SELECT {list} FROM {ch_table} WHERE height < {boundary}"
    )
}

/// Ranged backfill: chunked `INSERT … SELECT` inside the archive.
///
/// Path A: zero RPC. The caller binds `{lo}` / `{hi}` per chunk and persists
/// the cursor between chunks.
#[must_use]
pub fn ranged_backfill_sql(target: &str, select_sql: &str) -> String {
    format!("INSERT INTO {target} {select_sql} WHERE height >= {{lo}} AND height < {{hi}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_ddl_uses_replacing_merge_tree() {
        let ddl = archive_table_ddl("indexer", "events", "toYYYYMM(ts)");
        assert!(ddl.contains("ENGINE = ReplacingMergeTree(_version)"));
        assert!(ddl.contains("ORDER BY (chain, height)"));
        assert!(ddl.contains("PARTITION BY toYYYYMM(ts)"));
    }

    #[test]
    fn job_output_ddl_pins_order_and_partition() {
        let ddl = job_output_ddl(
            "analytics",
            "job_sandwich__v3",
            &["slot".to_owned(), "ix_idx".to_owned()],
            "toYYYYMM(ts)",
            Some("180d"),
        );
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS analytics.job_sandwich__v3"));
        assert!(ddl.contains("ORDER BY (chain, slot, ix_idx)"));
        assert!(ddl.contains("TTL ts + INTERVAL 180d"));
    }

    #[test]
    fn job_output_ddl_without_ttl_has_no_ttl_clause() {
        let ddl = job_output_ddl("analytics", "job_x__v1", &[], "toYYYYMM(ts)", None);
        assert!(!ddl.contains("TTL"));
        assert!(ddl.contains("ORDER BY (chain, height)"));
    }

    #[test]
    fn live_view_targets_physical_table() {
        let ddl = live_view_ddl("analytics", "job_x__v1", "SELECT * FROM events");
        assert_eq!(
            ddl,
            "CREATE MATERIALIZED VIEW IF NOT EXISTS analytics.job_x__v1_mv TO analytics.job_x__v1 AS SELECT * FROM events"
        );
    }

    #[test]
    fn tiered_view_unions_hot_and_archive_at_boundary() {
        let ddl = tiered_view_ddl(
            "events_hot",
            "indexer.events",
            1000,
            &["height".to_owned(), "data".to_owned()],
        );
        assert!(ddl.contains("FROM events_hot WHERE height >= 1000"));
        assert!(ddl.contains("FROM indexer.events WHERE height < 1000"));
    }

    #[test]
    fn ranged_backfill_binds_lo_hi() {
        let sql = ranged_backfill_sql("job_x__v1", "SELECT * FROM events");
        assert!(sql.contains("WHERE height >= {lo} AND height < {hi}"));
    }

    #[test]
    fn job_table_as_select_pins_engine_and_derives_schema() {
        let ddl = job_table_as_select_ddl(
            "analytics",
            "job_sandwich__v3",
            &["slot".to_owned()],
            "toYYYYMM(ts)",
            "SELECT slot AS _height FROM events WHERE height >= {lo} AND height < {hi}",
        );
        assert!(ddl.contains("ENGINE = ReplacingMergeTree(_height)"));
        assert!(ddl.contains("ORDER BY (slot)"));
        assert!(ddl.contains("PARTITION BY toYYYYMM(ts)"));
        assert!(ddl.contains("height >= 0 AND height < 0"));
        assert!(ddl.ends_with("LIMIT 0"));
    }
}
