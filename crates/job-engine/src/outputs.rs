//! Versioned outputs: immutable physical tables plus an atomic alias.
//!
//! A logic change creates a new table (`job_sandwich__v3`); the old one is
//! retired, never mutated in place. Readers switch atomically with
//! `CREATE OR REPLACE VIEW`, so there is no empty window.

/// Physical table for `(alias, version)`: `job_sandwich__v3`.
pub fn physical_table(alias: &str, version: u32) -> Result<String, crate::JobError> {
    crate::validate_identifier(alias)?;
    Ok(format!("{alias}__v{version}"))
}

/// Atomic alias swap: readers move to the new version with no empty window.
pub fn alias_swap_ddl(alias: &str, version: u32) -> Result<String, crate::JobError> {
    let physical = physical_table(alias, version)?;
    Ok(format!(
        "CREATE OR REPLACE VIEW {alias} AS SELECT * FROM {physical}"
    ))
}

/// Stable content checksum (FNV-1a hex, 16 chars) for DDL drift detection.
#[must_use]
pub fn checksum(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_names_carry_alias_and_version() {
        assert_eq!(
            physical_table("job_sandwich", 3).expect("name"),
            "job_sandwich__v3"
        );
    }

    #[test]
    fn alias_swap_selects_the_new_version() {
        let ddl = alias_swap_ddl("job_sandwich", 3).expect("ddl");
        assert_eq!(
            ddl,
            "CREATE OR REPLACE VIEW job_sandwich AS SELECT * FROM job_sandwich__v3"
        );
    }

    #[test]
    fn bad_alias_is_rejected_before_interpolation() {
        assert!(physical_table("a-b", 1).is_err());
        assert!(alias_swap_ddl("a;b", 1).is_err());
    }

    #[test]
    fn checksum_is_pinned_and_sensitive() {
        // Golden value: any operator or constant change breaks this.
        assert_eq!(checksum("abc"), "e71fa2190541574b");
        assert_ne!(checksum("a"), checksum("b"));
        assert_ne!(checksum("abc"), checksum("xyz"));
        assert_ne!(checksum(""), checksum("a"));
    }
}
