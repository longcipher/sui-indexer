use eyre::Result;
use serde::Deserialize;
use sui_indexer_config::QueryConfig;
use sui_indexer_storage::StorageManager;

pub use sui_indexer_storage::GatewayResult;

/// Read-only SQL gateway over the canonical tables.
pub struct QueryGateway {
    config: QueryConfig,
}

/// Gateway query request.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayQuery {
    /// SELECT statement.
    pub sql: String,
    /// Optional Move event selector `package::module::Name` exposed as a CTE.
    pub event: Option<String>,
    /// Max rows (clamped to gateway max).
    pub limit: Option<u64>,
}

/// Queryable canonical tables.
const ALLOWED_TABLES: &[&str] = &[
    "checkpoints",
    "transactions",
    "events_v2",
    "objects",
    "coin_flows",
    "balance_snapshots",
    "coin_metadata",
];

impl QueryGateway {
    /// Create a gateway from query config.
    pub fn new(config: QueryConfig) -> Self {
        Self { config }
    }

    /// Validate that a statement is a read-only SELECT over allowed tables.
    pub fn validate(&self, sql: &str) -> Result<String> {
        validate_select(sql)
    }

    /// Execute a validated query with row/byte caps.
    pub async fn execute(
        &self,
        storage: &StorageManager,
        query: GatewayQuery,
    ) -> Result<GatewayResult> {
        let mut sql = self.validate(&query.sql)?;
        if let Some(event) = &query.event {
            sql = apply_event_cte(&sql, event)?;
        }
        let limit = query
            .limit
            .unwrap_or(self.config.max_rows)
            .clamp(1, self.config.max_rows.max(1));
        sql = append_limit(&sql, limit);
        storage.sql_query(&sql, limit, self.config.max_bytes).await
    }
}

/// Validate SELECT-only access to allowed tables.
pub fn validate_select(sql: &str) -> Result<String> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err(eyre::eyre!("empty query"));
    }
    if trimmed.len() > 64 * 1024 {
        return Err(eyre::eyre!("query exceeds 64KB"));
    }
    let upper = trimmed.to_ascii_uppercase();
    for forbidden in [
        "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "CREATE", "TRUNCATE", "GRANT", "REVOKE",
        "COPY", "CALL", "VACUUM", "ANALYZE",
    ] {
        if upper.contains(forbidden) {
            return Err(eyre::eyre!("only SELECT queries are allowed"));
        }
    }
    if !upper.starts_with("SELECT") && !upper.starts_with("WITH") {
        return Err(eyre::eyre!("only SELECT queries are allowed"));
    }
    if trimmed.contains(';') {
        return Err(eyre::eyre!("multi-statement queries are not allowed"));
    }
    let lower = trimmed.to_ascii_lowercase();
    if !ALLOWED_TABLES.iter().any(|table| lower.contains(table)) {
        return Err(eyre::eyre!("query must reference a canonical table"));
    }
    Ok(trimmed.to_string())
}

/// Expose a Move event selector as a `decoded_events` CTE over events_v2.
pub fn apply_event_cte(sql: &str, event: &str) -> Result<String> {
    let parts: Vec<&str> = event.split("::").collect();
    if parts.len() != 3 {
        return Err(eyre::eyre!("event must be package::module::Name"));
    }
    let (package, module, name) = (parts[0], parts[1], parts[2]);
    if package.is_empty() || module.is_empty() || name.is_empty() {
        return Err(eyre::eyre!("event must be package::module::Name"));
    }
    let package_escaped = package.replace('\'', "''");
    let module_escaped = module.replace('\'', "''");
    let name_escaped = name.replace('\'', "''");
    let cte = format!(
        "decoded_events AS (SELECT checkpoint_sequence, transaction_digest, event_index, \
         sender, timestamp_ms, bcs, fields FROM events_v2 WHERE package_id LIKE '%{package_escaped}%' \
         AND module_name = '{module_escaped}' AND event_type = '{name_escaped}')"
    );
    let trimmed = sql.trim();
    if trimmed.to_ascii_uppercase().starts_with("WITH") {
        let rest = &trimmed[4..];
        Ok(format!("WITH {cte},{rest}"))
    } else {
        Ok(format!("WITH {cte} {trimmed}"))
    }
}

/// Append LIMIT when the statement has none.
pub fn append_limit(sql: &str, limit: u64) -> String {
    if sql.to_ascii_uppercase().contains("LIMIT") {
        sql.to_string()
    } else {
        format!("{sql} LIMIT {limit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_select_statements() {
        assert!(validate_select("DELETE FROM checkpoints").is_err());
        assert!(validate_select("SELECT 1; SELECT 2").is_err());
        assert!(validate_select("SELECT * FROM secrets").is_err());
        assert!(validate_select("SELECT * FROM checkpoints").is_ok());
    }

    #[test]
    fn event_cte_wraps_select() {
        let sql = apply_event_cte("SELECT * FROM decoded_events", "0x2::coin::CoinEvent").unwrap();
        assert!(sql.starts_with("WITH decoded_events AS"));
        assert!(apply_event_cte("SELECT 1", "bad").is_err());
    }

    #[test]
    fn limit_appended_only_when_missing() {
        assert_eq!(
            append_limit("SELECT * FROM checkpoints", 10),
            "SELECT * FROM checkpoints LIMIT 10"
        );
        assert_eq!(
            append_limit("SELECT * FROM checkpoints LIMIT 5", 10),
            "SELECT * FROM checkpoints LIMIT 5"
        );
    }
}
