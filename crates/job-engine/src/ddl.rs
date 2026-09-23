//! Runtime DDL: identifier guards, read-only SQL checks, table builders.
//!
//! Job output names come from user specs, so every identifier is allow-listed
//! before interpolation. User SQL is restricted to read-only `SELECT`.

/// Validate one SQL identifier (`table`, `column`, alias).
///
/// Allow-list: ASCII letters, digits, underscore; must not start with a digit.
pub fn validate_identifier(name: &str) -> Result<(), crate::JobError> {
    if name.is_empty() || name.len() > 64 {
        return Err(crate::JobError::BadIdentifier(name.to_owned()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(crate::JobError::BadIdentifier(name.to_owned()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(crate::JobError::BadIdentifier(name.to_owned()));
    }
    Ok(())
}

/// Validate a dotted `db.table` or bare `table` path.
pub fn validate_table_path(path: &str) -> Result<(), crate::JobError> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() || parts.len() > 2 {
        return Err(crate::JobError::BadIdentifier(path.to_owned()));
    }
    for part in parts {
        validate_identifier(part)?;
    }
    Ok(())
}

/// Reject anything that is not a read-only `SELECT`/`WITH … SELECT`.
pub fn validate_select_sql(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err("empty SQL".to_owned());
    }
    if trimmed.len() > 64 * 1024 {
        return Err("SQL exceeds 64KiB".to_owned());
    }
    let upper = trimmed.to_ascii_uppercase();
    for banned in [
        "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "CREATE", "TRUNCATE", "GRANT", "REVOKE",
        "COPY", "CALL", "VACUUM", "ANALYZE",
    ] {
        if upper.contains(banned) {
            return Err(format!("banned keyword {banned}"));
        }
    }
    if !(upper.starts_with("SELECT") || upper.starts_with("WITH")) {
        return Err("SQL must start with SELECT or WITH".to_owned());
    }
    if trimmed.contains(';') {
        return Err("multiple statements are not allowed".to_owned());
    }
    Ok(())
}

/// Bind `{lo}` / `{hi}` range placeholders in user SQL.
#[must_use]
pub fn bind_range(select_sql: &str, lo: u64, hi: u64) -> String {
    select_sql
        .replace("{lo}", &lo.to_string())
        .replace("{hi}", &hi.to_string())
}

/// One DDL statement with its checksum for drift detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlStatement {
    /// Statement text.
    pub sql: String,
    /// Checksum of the statement.
    pub checksum: String,
}

/// `CREATE TABLE <target> AS <select> LIMIT 0`: derive the output schema from
/// the user query without executing it.
pub fn build_create_table_as(
    target: &str,
    select_sql: &str,
) -> Result<DdlStatement, crate::JobError> {
    validate_table_path(target)?;
    validate_select_sql(select_sql).map_err(crate::JobError::BadSql)?;
    let sql = format!("CREATE TABLE IF NOT EXISTS {target} AS {select_sql} LIMIT 0");
    let checksum = crate::checksum(&sql);
    Ok(DdlStatement { sql, checksum })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_allow_list_blocks_injection() {
        assert!(validate_identifier("job_sandwich__v3").is_ok());
        assert!(validate_identifier("_private").is_ok());
        assert!(validate_identifier("1abc").is_err());
        assert!(validate_identifier("a-b").is_err());
        assert!(validate_identifier("a b").is_err());
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("x\"; DROP TABLE t; --").is_err());
    }

    #[test]
    fn table_paths_support_schema_qualification() {
        assert!(validate_table_path("job_sandwich__v3").is_ok());
        assert!(validate_table_path("analytics.job_sandwich").is_ok());
        assert!(validate_table_path("a.b.c").is_err());
    }

    #[test]
    fn select_check_rejects_writes_and_multi_statements() {
        assert!(validate_select_sql("SELECT height FROM chain_events").is_ok());
        assert!(validate_select_sql("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
        assert!(validate_select_sql("INSERT INTO t SELECT 1").is_err());
        assert!(validate_select_sql("SELECT 1; DROP TABLE t").is_err());
        assert!(validate_select_sql("DELETE FROM t").is_err());
        assert!(validate_select_sql("").is_err());
    }

    #[test]
    fn identifier_length_boundary_is_64() {
        let ok = "a".repeat(64);
        assert!(validate_identifier(&ok).is_ok());
        let too_long = "a".repeat(65);
        assert!(validate_identifier(&too_long).is_err());
    }

    #[test]
    fn select_size_boundary_is_64kib() {
        let mut big = String::from("SELECT ");
        while big.len() <= 1088 {
            big.push_str("height, ");
        }
        // Above the old 1088-byte tripwire but below the 64KiB cap: valid.
        assert!(big.len() > 1088);
        assert!(validate_select_sql(&big).is_ok());
        // Exactly at the cap is valid; one byte over is not.
        let at_cap = format!("SELECT {}", "x".repeat(64 * 1024 - 7));
        assert_eq!(at_cap.len(), 64 * 1024);
        assert!(validate_select_sql(&at_cap).is_ok());
        let huge = format!("SELECT '{}'", "x".repeat(64 * 1024));
        assert!(validate_select_sql(&huge).is_err());
    }

    #[test]
    fn create_table_as_derives_schema_without_running_query() {
        let stmt = build_create_table_as("job_sandwich__v1", "SELECT height FROM chain_events")
            .expect("ddl");
        assert!(
            stmt.sql
                .starts_with("CREATE TABLE IF NOT EXISTS job_sandwich__v1 AS")
        );
        assert!(stmt.sql.ends_with("LIMIT 0"));
        assert_eq!(stmt.checksum.len(), 16);
    }

    #[test]
    fn range_placeholders_bind_heights() {
        let bound = bind_range("height >= {lo} AND height < {hi}", 10, 20);
        assert_eq!(bound, "height >= 10 AND height < 20");
        assert!(!bind_range("SELECT 1", 0, 0).contains('{'));
    }
}
