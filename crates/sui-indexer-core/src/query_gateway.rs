use std::time::Duration;

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

/// Queryable canonical tables: skeleton (`blocks`, `txs`, `chain_events`)
/// plus the Sui-native tables. Job outputs enter through the dynamic catalog
/// ([`QueryGateway::validate_with_catalog`]).
const ALLOWED_TABLES: &[&str] = &[
    "blocks",
    "txs",
    "chain_events",
    "checkpoints",
    "transactions",
    "events_v2",
    "objects",
    "coin_flows",
    "balance_snapshots",
    "coin_metadata",
];

/// Quote a catalog table name after allow-listing it as an identifier.
fn validated_catalog_name(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > 64 {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

impl QueryGateway {
    /// Create a gateway from query config.
    pub fn new(config: QueryConfig) -> Self {
        Self { config }
    }

    /// Validate that a statement is a read-only SELECT over allowed tables.
    pub fn validate(&self, sql: &str) -> Result<String> {
        validate_select(sql)
    }

    /// Validate against core tables plus dynamic catalog tables
    /// (`catalog_objects WHERE public`).
    pub fn validate_with_catalog(&self, sql: &str, catalog: &[String]) -> Result<String> {
        validate_select_catalog(sql, catalog)
    }

    /// Execute a validated query with row/byte caps.
    pub async fn execute(
        &self,
        storage: &StorageManager,
        query: GatewayQuery,
    ) -> Result<GatewayResult> {
        self.execute_with_catalog(storage, query, &[]).await
    }

    /// Execute with dynamic catalog visibility: allow-list = core tables ∪
    /// `catalog_objects WHERE public`.
    pub async fn execute_with_catalog(
        &self,
        storage: &StorageManager,
        query: GatewayQuery,
        catalog: &[String],
    ) -> Result<GatewayResult> {
        let mut sql = self.validate_with_catalog(&query.sql, catalog)?;
        if let Some(event) = &query.event {
            sql = apply_event_cte(&sql, event)?;
        }
        let limit = query
            .limit
            .unwrap_or(self.config.max_rows)
            .clamp(1, self.config.max_rows.max(1));
        // Server-side bound the user statement cannot escape: the outer
        // LIMIT wins even when the inner statement carries its own LIMIT.
        let sql = format!("SELECT * FROM ({sql}) AS _gateway_q LIMIT {limit}");
        let timeout = Duration::from_millis(self.config.timeout_ms.max(1));
        tokio::time::timeout(
            timeout,
            storage.sql_query(&sql, limit, self.config.max_bytes),
        )
        .await
        .map_err(|_| eyre::eyre!("query timed out after {}ms", self.config.timeout_ms))?
    }

    /// Load public catalog table names for `chain_id` (empty on error: the
    /// gateway stays safe by falling back to the static allow-list).
    pub async fn public_catalog_tables(storage: &StorageManager, chain_id: &str) -> Vec<String> {
        use sui_indexer_storage::JobControlPlane as _;
        match storage.postgres().list_public_catalog(chain_id).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| validated_catalog_name(&row.name))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Validate SELECT-only access to allowed tables.
pub fn validate_select(sql: &str) -> Result<String> {
    validate_select_catalog(sql, &[])
}

/// Validate against core tables plus `extra` catalog tables.
pub fn validate_select_catalog(sql: &str, extra: &[String]) -> Result<String> {
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
    let catalog_hit = extra
        .iter()
        .filter_map(|name| validated_catalog_name(name))
        .any(|name| lower.contains(&name));
    if !ALLOWED_TABLES.iter().any(|table| lower.contains(table)) && !catalog_hit {
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
        // Validators return the statement itself, not a canned value.
        assert_eq!(
            validate_select("SELECT * FROM checkpoints").expect("valid"),
            "SELECT * FROM checkpoints"
        );
        let gateway = QueryGateway::new(sui_indexer_config::QueryConfig {
            max_rows: 10,
            timeout_ms: 5000,
            max_bytes: 1024,
        });
        assert_eq!(
            gateway
                .validate("SELECT * FROM checkpoints")
                .expect("valid"),
            "SELECT * FROM checkpoints"
        );
        assert_eq!(
            gateway
                .validate_with_catalog("SELECT * FROM job_sandwich", &["job_sandwich".to_owned()])
                .expect("valid"),
            "SELECT * FROM job_sandwich"
        );
        assert_eq!(
            validate_select_catalog("SELECT * FROM job_sandwich", &["job_sandwich".to_owned()])
                .expect("valid"),
            "SELECT * FROM job_sandwich"
        );
    }

    #[test]
    fn event_cte_wraps_select() {
        let sql = apply_event_cte("SELECT * FROM decoded_events", "0x2::coin::CoinEvent").unwrap();
        assert!(sql.starts_with("WITH decoded_events AS"));
        assert!(apply_event_cte("SELECT 1", "bad").is_err());
    }

    #[test]
    fn catalog_tables_extend_the_allow_list() {
        assert!(validate_select("SELECT * FROM job_sandwich").is_err());
        assert!(
            validate_select_catalog("SELECT * FROM job_sandwich", &["job_sandwich".to_owned()])
                .is_ok()
        );
        // Injection-shaped catalog names never widen the allow-list.
        assert!(
            validate_select_catalog(
                "SELECT * FROM job_sandwich",
                &["job_sandwich; DROP TABLE x --".to_owned()]
            )
            .is_err()
        );
        assert!(validate_select("SELECT * FROM chain_events").is_ok());
        assert!(validate_select("SELECT * FROM blocks").is_ok());
        // WITH queries read through the same allow-list.
        assert!(
            validate_select_catalog(
                "WITH x AS (SELECT height FROM job_sandwich) SELECT * FROM x",
                &["job_sandwich".to_owned()]
            )
            .is_ok()
        );
    }

    #[test]
    fn catalog_name_length_boundary_is_64() {
        let ok = "a".repeat(64);
        assert!(validate_select_catalog(&format!("SELECT * FROM {ok}"), &[ok]).is_ok());
        let too_long = "a".repeat(65);
        assert!(
            validate_select_catalog(&format!("SELECT * FROM {too_long}"), &[too_long]).is_err()
        );
        // Leading digits and dashes never validate.
        assert!(validate_select_catalog("SELECT * FROM 1t", &["1t".to_owned()]).is_err());
        assert!(validate_select_catalog("SELECT * FROM t", &["a-b".to_owned()]).is_err());
        assert!(validate_select_catalog("SELECT * FROM t", &["".to_owned()]).is_err());
    }

    #[test]
    fn select_size_boundary_is_64kib() {
        let at_cap = format!("SELECT {} FROM checkpoints", "x".repeat(64 * 1024 - 24));
        assert_eq!(at_cap.len(), 64 * 1024);
        assert!(validate_select(&at_cap).is_ok());
        let huge = format!("SELECT '{}' FROM checkpoints", "x".repeat(64 * 1024));
        assert!(validate_select(&huge).is_err());
    }

    #[test]
    fn event_cte_rejects_partial_selectors() {
        assert!(apply_event_cte("SELECT 1", "0x2::coin").is_err());
        assert!(apply_event_cte("SELECT 1", "0x2::::").is_err());
        assert!(apply_event_cte("SELECT 1", "::coin::Name").is_err());
        assert!(apply_event_cte("SELECT 1", "0x2::coin::").is_err());
        assert!(apply_event_cte("SELECT 1", "0x2::coin::Name").is_ok());
    }

    async fn live_storage() -> Option<StorageManager> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let db = sui_indexer_config::DatabaseConfig {
            url,
            max_connections: 2,
            min_connections: 1,
            connect_timeout: 10,
            idle_timeout: None,
            auto_migrate: false,
        };
        let storage = StorageManager::new_postgres(db).await.ok()?;
        storage.initialize().await.ok()?;
        Some(storage)
    }

    fn gateway() -> QueryGateway {
        QueryGateway::new(sui_indexer_config::QueryConfig {
            max_rows: 10,
            timeout_ms: 5000,
            max_bytes: 1024 * 1024,
        })
    }

    #[tokio::test]
    async fn execute_returns_rows_and_columns() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let result = gateway()
            .execute(
                &storage,
                GatewayQuery {
                    sql: "SELECT COUNT(*) AS n FROM checkpoints".to_string(),
                    event: None,
                    limit: None,
                },
            )
            .await
            .expect("execute");
        assert_eq!(result.columns, vec!["n".to_string()]);
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0]["n"].is_number());
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn catalog_tables_flow_into_execution() {
        use sui_indexer_storage::JobControlPlane as _;
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = format!("qg-{}-{}", std::process::id(), rand_suffix());
        let table = format!("qg_{}_{}", std::process::id(), rand_suffix());
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE {table} (height BIGINT)"
        )))
        .execute(storage.postgres().pool())
        .await
        .expect("ddl");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} VALUES (7)"
        )))
        .execute(storage.postgres().pool())
        .await
        .expect("rows");
        storage
            .postgres()
            .upsert_catalog_object(&sui_indexer_storage::CatalogObjectRow {
                chain_id: chain.clone(),
                name: table.clone(),
                kind: "table".to_owned(),
                ddl: format!("CREATE TABLE {table} (height BIGINT)"),
                select_sql: None,
                checksum: "abc".to_owned(),
                public: true,
                block_column: Some("height".to_owned()),
                reorg_mode: "block_scoped".to_owned(),
                owner_job: None,
                backfill: "none".to_owned(),
            })
            .await
            .expect("catalog");
        let tables = QueryGateway::public_catalog_tables(&storage, &chain).await;
        assert_eq!(tables, vec![table.clone()]);
        let result = gateway()
            .execute_with_catalog(
                &storage,
                GatewayQuery {
                    sql: format!("SELECT * FROM {table}"),
                    event: None,
                    limit: None,
                },
                &tables,
            )
            .await
            .expect("execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["height"], serde_json::json!(7));
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(storage.postgres().pool())
            .await
            .expect("drop");
    }

    #[tokio::test]
    async fn embedded_limit_cannot_escape_the_cap() {
        let Some(storage) = live_storage().await else {
            return;
        };
        // Inner LIMIT 100 must still come back bounded by the gateway cap.
        let result = gateway()
            .execute(
                &storage,
                GatewayQuery {
                    sql: "SELECT * FROM checkpoints LIMIT 100".to_string(),
                    event: None,
                    limit: None,
                },
            )
            .await
            .expect("execute");
        assert!(result.row_count <= 10);
    }

    #[tokio::test]
    async fn slow_queries_hit_the_timeout() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let impatient = QueryGateway::new(sui_indexer_config::QueryConfig {
            max_rows: 10,
            timeout_ms: 50,
            max_bytes: 1024 * 1024,
        });
        let result = impatient
            .execute(
                &storage,
                GatewayQuery {
                    sql: "SELECT pg_sleep(2) FROM checkpoints".to_string(),
                    event: None,
                    limit: None,
                },
            )
            .await;
        assert!(result.is_err());
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
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
