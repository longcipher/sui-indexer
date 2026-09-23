//! Dynamic catalog planning: checksum drift detection.
//!
//! The catalog replaces the compile-time object list. `plan_catalog_apply`
//! diffs the desired object against the stored row: same checksum means no
//! work, a drift means re-apply the DDL.

use sui_indexer_storage::CatalogObjectRow;

use crate::checksum;

/// What applying one catalog object requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogPlan {
    /// Object is new: execute the DDL and register the row.
    Create {
        /// Object name.
        name: String,
        /// DDL to execute.
        ddl: String,
    },
    /// Stored checksum differs: re-execute the DDL and refresh the row.
    Reapply {
        /// Object name.
        name: String,
        /// DDL to execute.
        ddl: String,
        /// Previously stored checksum.
        previous_checksum: String,
    },
    /// Stored checksum matches: nothing to do.
    NoChange {
        /// Object name.
        name: String,
    },
}

/// Diff one desired catalog object against its stored row.
#[must_use]
pub fn plan_catalog_apply(
    desired: &CatalogObjectRow,
    stored: Option<&CatalogObjectRow>,
) -> CatalogPlan {
    match stored {
        None => CatalogPlan::Create {
            name: desired.name.clone(),
            ddl: desired.ddl.clone(),
        },
        Some(row) if row.checksum == desired.checksum || row.checksum == checksum(&desired.ddl) => {
            CatalogPlan::NoChange {
                name: desired.name.clone(),
            }
        }
        Some(row) => CatalogPlan::Reapply {
            name: desired.name.clone(),
            ddl: desired.ddl.clone(),
            previous_checksum: row.checksum.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(checksum: &str) -> CatalogObjectRow {
        CatalogObjectRow {
            chain_id: "test".to_owned(),
            name: "job_sandwich".to_owned(),
            kind: "view".to_owned(),
            ddl: "CREATE VIEW job_sandwich AS SELECT 1".to_owned(),
            select_sql: None,
            checksum: checksum.to_owned(),
            public: true,
            block_column: Some("_height".to_owned()),
            reorg_mode: "block_scoped".to_owned(),
            owner_job: Some("sandwich".to_owned()),
            backfill: "ranged".to_owned(),
        }
    }

    #[test]
    fn missing_object_is_created() {
        let desired = object("abc");
        assert!(matches!(
            plan_catalog_apply(&desired, None),
            CatalogPlan::Create { .. }
        ));
    }

    #[test]
    fn matching_checksum_is_no_change() {
        let desired = object("abc");
        let stored = object("abc");
        assert!(matches!(
            plan_catalog_apply(&desired, Some(&stored)),
            CatalogPlan::NoChange { .. }
        ));
    }

    #[test]
    fn drifted_checksum_reapplies() {
        let desired = object("new");
        let stored = object("old");
        let plan = plan_catalog_apply(&desired, Some(&stored));
        assert!(matches!(plan, CatalogPlan::Reapply { .. }));
        if let CatalogPlan::Reapply {
            previous_checksum, ..
        } = plan
        {
            assert_eq!(previous_checksum, "old");
        }
    }
}
