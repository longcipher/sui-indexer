//! Job apply/plan: pure decision logic over specs and stored versions.
//!
//! `apply` = validate → hash → compare with the latest stored version →
//! either reuse, bump, or update desired state. `plan` is the dry run: DDL
//! plus estimated scan size, with no writes.

use sui_indexer_config::JobSpec;

use crate::{JobError, physical_table};

/// What `apply` decided for one spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyDecision {
    /// No stored version: create `version` and start a full scan.
    Create {
        /// Version to create.
        version: u32,
    },
    /// Stored version has the same hash: only desired state may change.
    NoChange {
        /// Existing version.
        version: u32,
    },
    /// Hash differs: build a new version and re-scan.
    NewVersion {
        /// Previous version.
        previous: u32,
        /// Version to build.
        next: u32,
    },
    /// Desired state changed (pause / resume / retire).
    DesiredChange {
        /// Existing version.
        version: u32,
        /// New desired state.
        desired: String,
    },
}

impl ApplyDecision {
    /// The version to build for this decision. Centralizes version selection
    /// so callers cannot silently reuse a stale version when the logic
    /// changed: a hash change always bumps, never overwrites.
    #[must_use]
    pub fn target_version(&self) -> u32 {
        match self {
            Self::Create { version }
            | Self::NoChange { version }
            | Self::DesiredChange { version, .. } => *version,
            Self::NewVersion { next, .. } => *next,
        }
    }
}

/// Dry-run plan for `job plan`: DDL plus estimated scan size.
#[derive(Debug, Clone)]
pub struct JobPlan {
    /// Job name.
    pub name: String,
    /// Version the plan targets.
    pub version: u32,
    /// Decision behind the plan.
    pub decision: ApplyDecision,
    /// Statements the apply would execute, in order.
    pub ddl: Vec<String>,
    /// Estimated heights to scan (None = unbounded `head`).
    pub estimated_heights: Option<u64>,
}

/// Decide what applying `spec` means given the latest stored version.
///
/// `latest` is `(version, spec_hash, desired)` of the newest stored version,
/// if any.
pub fn decide_apply(
    spec: &JobSpec,
    latest: Option<(u32, &str, &str)>,
) -> Result<ApplyDecision, JobError> {
    spec.validate().map_err(|reason| JobError::InvalidSpec {
        job: spec.name.clone(),
        reason,
    })?;
    let hash = spec.spec_hash();
    match latest {
        None => Ok(ApplyDecision::Create {
            version: spec.version,
        }),
        Some((version, stored_hash, stored_desired)) => {
            if stored_hash != hash {
                Ok(ApplyDecision::NewVersion {
                    previous: version,
                    next: version.saturating_add(1).max(spec.version),
                })
            } else if stored_desired != spec.desired {
                Ok(ApplyDecision::DesiredChange {
                    version,
                    desired: spec.desired.clone(),
                })
            } else {
                Ok(ApplyDecision::NoChange { version })
            }
        }
    }
}

/// Build the dry-run plan: versioned table DDL plus scan estimate.
///
/// `tip` is the current sync tip used to bound `to = "head"` scans.
pub fn plan_job(spec: &JobSpec, decision: ApplyDecision, tip: u64) -> Result<JobPlan, JobError> {
    spec.validate().map_err(|reason| JobError::InvalidSpec {
        job: spec.name.clone(),
        reason,
    })?;
    let version = match &decision {
        ApplyDecision::Create { version }
        | ApplyDecision::NoChange { version }
        | ApplyDecision::DesiredChange { version, .. } => *version,
        ApplyDecision::NewVersion { next, .. } => *next,
    };
    let physical = physical_table(&spec.output.table, version)?;
    let ddl = vec![format!(
        "CREATE TABLE IF NOT EXISTS {physical} AS {} LIMIT 0",
        inline_sql(spec)?,
    )];
    let estimated_heights = crate::parse_scan_to(&spec.scan.to, tip)
        .map(|to| to.saturating_sub(spec.scan.from).saturating_add(1));
    Ok(JobPlan {
        name: spec.name.clone(),
        version,
        decision,
        ddl,
        estimated_heights,
    })
}

fn inline_sql(spec: &JobSpec) -> Result<String, JobError> {
    let sql = spec.sql.trim();
    if sql.is_empty() {
        return Ok("SELECT 1 WHERE FALSE".to_owned());
    }
    crate::ddl::validate_select_sql(sql).map_err(JobError::BadSql)?;
    Ok(sql.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sui_indexer_config::{JobTier, OutputConfig};

    fn sql_spec(name: &str) -> JobSpec {
        JobSpec {
            name: name.to_owned(),
            output: OutputConfig {
                table: format!("job_{name}"),
                ..OutputConfig::default()
            },
            sql: "SELECT height FROM chain_events".to_owned(),
            ..JobSpec::default()
        }
    }

    #[test]
    fn first_apply_creates_version() {
        let spec = sql_spec("sandwich");
        let decision = decide_apply(&spec, None).expect("decide");
        assert_eq!(decision, ApplyDecision::Create { version: 1 });
        assert_eq!(decision.target_version(), 1);
    }

    #[test]
    fn identical_reapply_is_no_change() {
        let spec = sql_spec("sandwich");
        let hash = spec.spec_hash();
        assert_eq!(
            decide_apply(&spec, Some((1, &hash, "active"))).expect("decide"),
            ApplyDecision::NoChange { version: 1 }
        );
    }

    #[test]
    fn logic_change_bumps_version() {
        let spec = sql_spec("sandwich");
        let decision = decide_apply(&spec, Some((1, "deadbeef", "active"))).expect("decide");
        assert_eq!(
            decision,
            ApplyDecision::NewVersion {
                previous: 1,
                next: 2
            }
        );
        assert_eq!(decision.target_version(), 2);
    }

    #[test]
    fn target_version_follows_every_decision() {
        assert_eq!(ApplyDecision::NoChange { version: 3 }.target_version(), 3);
        assert_eq!(
            ApplyDecision::DesiredChange {
                version: 4,
                desired: "paused".to_owned(),
            }
            .target_version(),
            4
        );
    }

    #[test]
    fn desired_only_change_does_not_rescan() {
        let mut spec = sql_spec("sandwich");
        spec.desired = "paused".to_owned();
        let hash = spec.spec_hash();
        assert_eq!(
            decide_apply(&spec, Some((1, &hash, "active"))).expect("decide"),
            ApplyDecision::DesiredChange {
                version: 1,
                desired: "paused".to_owned()
            }
        );
    }

    #[test]
    fn invalid_spec_is_rejected() {
        let spec = JobSpec::default();
        assert!(decide_apply(&spec, None).is_err());
    }

    #[test]
    fn plan_binds_versioned_table_and_estimates_scan() {
        let spec = sql_spec("sandwich");
        let plan = plan_job(&spec, ApplyDecision::Create { version: 1 }, 999).expect("plan");
        assert!(plan.ddl[0].contains("job_sandwich__v1"));
        // The DDL carries the query body, not just the table name.
        assert!(plan.ddl[0].contains("FROM chain_events"));
        assert_eq!(plan.estimated_heights, Some(1000));
    }

    #[test]
    fn wasm_spec_without_sql_body_still_plans() {
        let mut spec = sql_spec("sandwich");
        spec.tier = JobTier::Wasm;
        spec.module = "sandwich.wasm".to_owned();
        spec.sql.clear();
        let plan = plan_job(&spec, ApplyDecision::Create { version: 1 }, 10).expect("plan");
        assert_eq!(plan.version, 1);
    }
}
