//! Tiered-routing composition: route → boundary → re-bake.

use archive_store::{BoundaryCheck, Tier, TierRouter, tiered_view_ddl};

#[test]
fn hot_archive_split_and_guarded_boundary_move() {
    let router = TierRouter::new(1000, ["job_sandwich".to_owned()].into_iter().collect());
    assert_eq!(router.route("chain_events", 999), Tier::Archive);
    assert_eq!(router.route("chain_events", 1000), Tier::Hot);
    assert_eq!(router.route("job_sandwich", 10), Tier::Hot);

    let covered = |_: u64, _: u64| None;
    assert_eq!(
        router.check_boundary_move(2000, &covered),
        BoundaryCheck::Ready { to: 2000 }
    );

    // After the move, the tiered view is re-baked atomically at 2000.
    let ddl = tiered_view_ddl(
        "chain_events_hot",
        "indexer.chain_events",
        2000,
        &["height".to_owned(), "emitter".to_owned()],
    );
    assert!(ddl.contains("WHERE height >= 2000"));
    assert!(ddl.contains("WHERE height < 2000"));
}

#[test]
fn archive_job_ddl_is_replacing_merge_tree() {
    let ddl = archive_store::job_output_ddl(
        "analytics",
        "job_sandwich__v3",
        &["slot".to_owned()],
        "toYYYYMM(ts)",
        Some("180d"),
    );
    assert!(ddl.contains("ReplacingMergeTree"));
    assert!(ddl.contains("ORDER BY (chain, slot)"));

    let backfill =
        archive_store::ranged_backfill_sql("analytics.job_sandwich__v3", "SELECT * FROM events");
    assert!(backfill.contains("{lo}"));
    assert!(backfill.contains("{hi}"));
}
