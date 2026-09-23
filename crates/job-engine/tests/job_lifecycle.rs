//! End-to-end job lifecycle without a database: apply → plan → DDL → scan.

use job_engine::{ApplyDecision, decide_apply, plan_chunks, plan_job};
use sui_indexer_config::{JobSpec, OutputConfig};

fn sandwich_spec() -> JobSpec {
    JobSpec {
        name: "sandwich".to_owned(),
        version: 3,
        source: "events".to_owned(),
        output: OutputConfig {
            table: "job_sandwich".to_owned(),
            order_by: vec!["slot".to_owned(), "ix_idx".to_owned()],
            ..OutputConfig::default()
        },
        sql: "SELECT slot AS _height, 3 AS _rule_version, 1 AS _commitment \
              FROM chain_events WHERE height >= {lo} AND height < {hi}"
            .to_owned(),
        ..JobSpec::default()
    }
}

#[test]
fn fresh_apply_plans_versioned_table_and_full_scan() {
    let spec = sandwich_spec();
    let decision = decide_apply(&spec, None).expect("decide");
    assert_eq!(decision, ApplyDecision::Create { version: 3 });
    let plan = plan_job(&spec, decision, 1_000_000).expect("plan");
    assert_eq!(plan.version, 3);
    assert!(plan.ddl[0].contains("job_sandwich__v3"));
    assert_eq!(plan.estimated_heights, Some(1_000_001));

    // The planned range tiles exactly into executable chunks.
    let chunks = plan_chunks(spec.scan.from, 1_000_000, spec.scan.chunk);
    assert!(!chunks.is_empty());
    assert_eq!(chunks.first().map(|c| c.hi), Some(1_000_001));
    assert_eq!(chunks.last().map(|c| c.lo), Some(0));
    let covered: u64 = chunks.iter().map(|c| c.hi - c.lo).sum();
    assert_eq!(covered, 1_000_001);
}

#[test]
fn logic_change_creates_a_new_version_while_desired_change_does_not() {
    let spec = sandwich_spec();
    let hash = spec.spec_hash();
    assert_eq!(
        decide_apply(&spec, Some((3, &hash, "active"))).expect("decide"),
        ApplyDecision::NoChange { version: 3 }
    );
    assert!(matches!(
        decide_apply(&spec, Some((3, "stale", "active"))).expect("decide"),
        ApplyDecision::NewVersion { .. }
    ));
    let mut paused = spec.clone();
    paused.desired = "paused".to_owned();
    let paused_hash = paused.spec_hash();
    assert!(matches!(
        decide_apply(&paused, Some((3, &paused_hash, "active"))).expect("decide"),
        ApplyDecision::DesiredChange { .. }
    ));
}

#[test]
fn alias_swap_points_readers_at_the_new_version() {
    let physical = job_engine::physical_table("job_sandwich", 3).expect("table");
    let swap = job_engine::alias_swap_ddl("job_sandwich", 3).expect("swap");
    assert_eq!(physical, "job_sandwich__v3");
    assert!(swap.contains("SELECT * FROM job_sandwich__v3"));
}
