//! Canonical storage round trips against a live PostgreSQL.
//!
//! Runs when `DATABASE_URL` points at a scratch database (vacuous pass
//! otherwise so ordinary `cargo test` needs no DB). The schema is reset once
//! per test process, so the `run_migrations`-noop mutant is observable.
//! Names are unique per test to stay race-free under parallel execution.

use std::sync::atomic::{AtomicU64, Ordering};

use sui_indexer_events::{
    EventMetadata, ProcessedEvent, ProcessedTransaction, TransactionMetadata,
};
use sui_indexer_storage::{
    CanonicalEventModel, CanonicalEventModelConfig, CheckpointModel, CheckpointModelConfig,
    CoinFlowModel, CoinFlowModelConfig, EventQueryFilter, ObjectModel, ObjectModelConfig,
    StorageManager, TransactionModel, TransactionModelConfig, TransactionQueryFilter,
    WatermarkModel, WatermarkModelConfig,
};

static INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static SEQ: AtomicU64 = AtomicU64::new(0);

fn ns(prefix: &str) -> String {
    format!(
        "{}-{}-{}",
        prefix,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

async fn manager() -> Option<StorageManager> {
    let url = std::env::var("DATABASE_URL").ok()?;
    // Reset the schema once per process; migrations run exclusively through
    // `initialize()` below, so the `run_migrations`-noop mutant is observable.
    INIT.get_or_init(|| async {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::query("DROP SCHEMA public CASCADE")
            .execute(&pool)
            .await
            .expect("drop");
        sqlx::query("CREATE SCHEMA public")
            .execute(&pool)
            .await
            .expect("create");
    })
    .await;
    let config = sui_indexer_config::DatabaseConfig {
        url,
        max_connections: 4,
        min_connections: 1,
        connect_timeout: 10,
        idle_timeout: None,
        auto_migrate: false,
    };
    let manager = StorageManager::new_postgres(config).await.ok()?;
    manager.initialize().await.expect("migrate");
    Some(manager)
}

fn sample_sui_event(seq: u64) -> sui_json_rpc_types::SuiEvent {
    sui_json_rpc_types::SuiEvent {
        id: sui_types::event::EventID {
            tx_digest: sui_types::base_types::TransactionDigest::new([seq as u8; 32]),
            event_seq: seq,
        },
        package_id: "0x0000000000000000000000000000000000000000000000000000000000000002"
            .parse()
            .expect("package"),
        transaction_module: "coin".parse().expect("module"),
        sender: "0x0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .expect("sender"),
        type_: "0x2::coin::Transfer".parse().expect("type"),
        parsed_json: serde_json::json!({ "seq": seq }),
        bcs: sui_json_rpc_types::BcsEvent::new(vec![1, 2, 3]),
        timestamp_ms: Some(1_000),
    }
}

fn sample_event(seq: u64, checkpoint: u64, package: &str) -> ProcessedEvent {
    ProcessedEvent {
        id: uuid::Uuid::new_v4(),
        event: sample_sui_event(seq),
        transaction_digest: sui_types::base_types::TransactionDigest::new([seq as u8; 32]),
        checkpoint_sequence: checkpoint,
        timestamp: chrono::Utc::now(),
        package_id: package.parse().expect("package"),
        module_name: "coin".to_owned(),
        event_type: "Transfer".to_owned(),
        sender: "0x1".to_owned(),
        fields: serde_json::json!({}),
        metadata: EventMetadata {
            processed_at: chrono::Utc::now(),
            processing_duration_ms: 1,
            event_index: seq as usize,
            matched_filters: vec![],
            tags: vec![],
        },
    }
}

fn sample_transaction(seed: u8, checkpoint: u64) -> ProcessedTransaction {
    let mut response = sui_json_rpc_types::SuiTransactionBlockResponse::default();
    response.digest = sui_types::base_types::TransactionDigest::new([seed; 32]);
    ProcessedTransaction {
        id: uuid::Uuid::new_v4(),
        transaction: response,
        checkpoint_sequence: checkpoint,
        timestamp: chrono::Utc::now(),
        events: vec![],
        metadata: TransactionMetadata {
            processed_at: chrono::Utc::now(),
            processing_duration_ms: 1,
            event_count: 0,
            gas_used: Some(100),
            success: true,
        },
    }
}

#[tokio::test]
async fn health_and_processed_event_round_trip() {
    let Some(manager) = manager().await else {
        return;
    };
    assert!(manager.health_check().await.expect("health"));
    let checkpoint = 1_000_000 + SEQ.fetch_add(1, Ordering::Relaxed);
    let package = "0x0000000000000000000000000000000000000000000000000000000000000002";
    manager
        .store_events(vec![
            sample_event(1, checkpoint, package),
            sample_event(2, checkpoint, package),
        ])
        .await
        .expect("store");
    manager
        .store_event(&sample_event(3, checkpoint + 1, package))
        .await
        .expect("store one");
    let got = manager
        .get_events_by_checkpoint_range(checkpoint, checkpoint)
        .await
        .expect("range");
    assert_eq!(got.len(), 2);
    let filtered = manager
        .query_events(EventQueryFilter {
            package: Some(package),
            from_checkpoint: Some(checkpoint),
            to_checkpoint: Some(checkpoint + 1),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(filtered.len(), 3);
    let empty = manager
        .query_events(EventQueryFilter {
            package: Some("0xdead"),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("query");
    assert!(empty.is_empty());
}

#[tokio::test]
async fn processed_transaction_round_trip() {
    let Some(manager) = manager().await else {
        return;
    };
    // Seed range 100..199 stays disjoint from the prune test's seeds 5,6
    // (digests are globally unique).
    let seed = 100 + (SEQ.fetch_add(1, Ordering::Relaxed) % 100) as u8;
    let checkpoint = 2_000_000 + u64::from(seed);
    let expected_digest = sui_types::base_types::TransactionDigest::new([seed; 32]).to_string();
    manager
        .store_transaction(&sample_transaction(seed, checkpoint))
        .await
        .expect("store");
    let got = manager
        .query_transactions(TransactionQueryFilter {
            sender: Some("0x0"),
            from_checkpoint: Some(checkpoint),
            to_checkpoint: Some(checkpoint),
            limit: 10,
        })
        .await
        .expect("query");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].transaction.digest.to_string(), expected_digest);
    // The plural path shares the implementation (singular delegates to it).
    manager
        .store_transactions(vec![sample_transaction(
            seed.wrapping_add(1),
            checkpoint + 1,
        )])
        .await
        .expect("store plural");
    let both = manager
        .query_transactions(TransactionQueryFilter {
            sender: Some("0x0"),
            from_checkpoint: Some(checkpoint),
            to_checkpoint: Some(checkpoint + 1),
            limit: 10,
        })
        .await
        .expect("query");
    assert_eq!(both.len(), 2);
    let missing = manager
        .query_transactions(TransactionQueryFilter {
            sender: Some("0xNobodyHere"),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("query");
    assert!(missing.is_empty());
}

#[tokio::test]
async fn canonical_models_and_counts() {
    let Some(manager) = manager().await else {
        return;
    };
    let tag = ns("canon");
    let seq = 3_000_000 + SEQ.fetch_add(1, Ordering::Relaxed);
    manager
        .store_checkpoint_model(CheckpointModel::new(CheckpointModelConfig {
            sequence_number: seq as i64,
            digest: format!("digest-{tag}"),
            prev_digest: Some("prev".to_owned()),
            epoch: 1,
            timestamp_ms: 1_000,
            transaction_count: 1,
            network_total_transactions: 1,
            validator_signature: "sig".to_owned(),
            end_of_epoch_data: None,
        }))
        .await
        .expect("checkpoint");
    manager
        .store_transaction_models(vec![TransactionModel::new(TransactionModelConfig {
            digest: format!("tx-{tag}"),
            checkpoint_sequence: seq as i64,
            timestamp_ms: 1_000,
            sender: "0x1".to_owned(),
            gas_used: Some(10),
            gas_price: Some(1),
            success: true,
            error_message: None,
        })])
        .await
        .expect("tx models");
    manager
        .store_object_models(vec![ObjectModel::new(ObjectModelConfig {
            object_id: format!("obj-{tag}"),
            version: 1,
            digest: "d".to_owned(),
            checkpoint_sequence: seq as i64,
            transaction_digest: format!("tx-{tag}"),
            sender: "0x1".to_owned(),
        })])
        .await
        .expect("objects");
    manager
        .store_canonical_events(vec![
            CanonicalEventModel::new(CanonicalEventModelConfig {
                checkpoint_sequence: seq as i64,
                transaction_digest: format!("tx-{tag}"),
                event_index: 0,
                package_id: "0x2".to_owned(),
                module_name: "coin".to_owned(),
                event_type: "Transfer".to_owned(),
                sender: "0x1".to_owned(),
                timestamp_ms: 1_000,
                bcs: Some(vec![9]),
                fields: serde_json::json!({}),
            }),
            CanonicalEventModel::new(CanonicalEventModelConfig {
                checkpoint_sequence: seq as i64,
                transaction_digest: format!("tx-{tag}"),
                event_index: 1,
                package_id: "0x2".to_owned(),
                module_name: "coin".to_owned(),
                event_type: "Transfer".to_owned(),
                sender: "0x1".to_owned(),
                timestamp_ms: 1_000,
                bcs: None,
                fields: serde_json::json!({}),
            }),
        ])
        .await
        .expect("canonical");
    let after = manager.table_counts().await.expect("counts");
    // Absolute minimums (this test inserts at least these; rows only ever
    // accumulate, so the bounds are race-proof).
    assert!(after.checkpoints >= 1);
    assert!(after.transactions >= 1);
    assert!(after.objects >= 1);
    assert!(after.events >= 2);
    // Targeted counts pin this test's own rows.
    for (table, column, value, want) in [
        ("checkpoints", "sequence_number", seq.to_string(), 1),
        ("transactions", "digest", format!("tx-{tag}"), 1),
        ("objects", "object_id", format!("obj-{tag}"), 1),
        ("events_v2", "transaction_digest", format!("tx-{tag}"), 2),
    ] {
        let counted = manager
            .sql_query(
                &format!("SELECT COUNT(*) AS n FROM {table} WHERE {column} = '{value}'"),
                10,
                1024,
            )
            .await
            .expect("count");
        assert_eq!(counted.rows[0]["n"], serde_json::json!(want));
    }
}

#[tokio::test]
async fn coin_flows_roll_up_into_snapshots() {
    let Some(manager) = manager().await else {
        return;
    };
    let tag = ns("coin");
    let seq = 4_000_000 + SEQ.fetch_add(1, Ordering::Relaxed);
    manager
        .store_coin_flows(vec![
            CoinFlowModel::new(CoinFlowModelConfig {
                checkpoint_sequence: seq as i64,
                timestamp_ms: 1_000,
                transaction_digest: "t".to_owned(),
                coin_type: format!("coin-{tag}"),
                holder: format!("holder-{tag}"),
                object_id: format!("o1-{tag}"),
                version: 1,
                balance: serde_json::json!(100),
            }),
            CoinFlowModel::new(CoinFlowModelConfig {
                checkpoint_sequence: seq as i64,
                timestamp_ms: 1_000,
                transaction_digest: "t".to_owned(),
                coin_type: format!("coin-{tag}"),
                holder: format!("holder-{tag}"),
                object_id: format!("o2-{tag}"),
                version: 1,
                balance: serde_json::json!("200"),
            }),
            CoinFlowModel::new(CoinFlowModelConfig {
                checkpoint_sequence: seq as i64,
                timestamp_ms: 1_000,
                transaction_digest: "t".to_owned(),
                coin_type: format!("coin-{tag}"),
                holder: format!("holder-{tag}"),
                object_id: format!("o3-{tag}"),
                version: 1,
                balance: serde_json::json!(true),
            }),
        ])
        .await
        .expect("flows");
    // Number, string and boolean balances all land; the snapshot sums them.
    let result = manager
        .sql_query(
            &format!("SELECT balance FROM balance_snapshots WHERE holder = 'holder-{tag}'"),
            10,
            1024 * 1024,
        )
        .await
        .expect("snapshot");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["balance"], serde_json::json!("300"));
    // Repair rebuilds derived rows it finds missing (`tag` is alphanumeric).
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM balance_snapshots WHERE holder = 'holder-{tag}'"
    )))
    .execute(manager.postgres().pool())
    .await
    .expect("delete");
    manager.repair_derived_insights().await.expect("repair");
    let restored = manager
        .sql_query(
            &format!("SELECT balance FROM balance_snapshots WHERE holder = 'holder-{tag}'"),
            10,
            1024 * 1024,
        )
        .await
        .expect("snapshot");
    assert_eq!(restored.rows.len(), 1);
}

#[tokio::test]
async fn moved_coins_credit_only_the_current_holder() {
    let Some(manager) = manager().await else {
        return;
    };
    let tag = ns("move");
    let coin = format!("coin-{tag}");
    let alice = format!("alice-{tag}");
    let bob = format!("bob-{tag}");
    let object = format!("obj-{tag}");
    for (version, holder, balance, seq) in [
        (1i64, alice.clone(), serde_json::json!(100), 100i64),
        (2i64, bob.clone(), serde_json::json!(150), 101i64),
    ] {
        manager
            .store_coin_flows(vec![CoinFlowModel::new(CoinFlowModelConfig {
                checkpoint_sequence: 4_000_000 + seq,
                timestamp_ms: 1_000,
                transaction_digest: "t".to_owned(),
                coin_type: coin.clone(),
                holder,
                object_id: object.clone(),
                version,
                balance,
            })])
            .await
            .expect("flows");
    }
    // Metadata accumulates both flows across the two scoped refreshes.
    let meta = manager
        .sql_query(
            &format!("SELECT flow_count FROM coin_metadata WHERE coin_type = '{coin}'"),
            10,
            1024,
        )
        .await
        .expect("metadata");
    assert_eq!(meta.rows.len(), 1);
    assert_eq!(meta.rows[0]["flow_count"], serde_json::json!(2));
    // One object, two versions: only v2 (bob, 150) counts, alice is zeroed.
    for (holder, want) in [(&alice, "0"), (&bob, "150")] {
        let result = manager
            .sql_query(
                &format!(
                    "SELECT balance FROM balance_snapshots WHERE holder = '{holder}' AND coin_type = '{coin}'"
                ),
                10,
                1024 * 1024,
            )
            .await
            .expect("snapshot");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["balance"], serde_json::json!(want));
    }
}

#[tokio::test]
async fn watermarks_advance_and_rewind() {
    let Some(manager) = manager().await else {
        return;
    };
    let pipe = ns("pipe");
    assert!(manager.get_watermark(&pipe).await.expect("get").is_none());
    let model = WatermarkModel::new(WatermarkModelConfig {
        pipeline: pipe.clone(),
        epoch_hi_inclusive: 1,
        checkpoint_hi_inclusive: 10,
        tx_hi: 5,
        timestamp_ms_hi_inclusive: 1_000,
        reader_lo: 10,
        pruner_hi: 0,
    });
    assert!(manager.set_watermark(model).await.expect("set"));
    let got = manager
        .get_watermark(&pipe)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.checkpoint_hi_inclusive, 10);
    manager.rewind_watermark(&pipe, 3).await.expect("rewind");
    let rewound = manager
        .get_watermark(&pipe)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(rewound.checkpoint_hi_inclusive, 3);
}

#[tokio::test]
async fn checkpoint_progress_tracks_the_tip() {
    let Some(manager) = manager().await else {
        return;
    };
    manager
        .update_checkpoint_progress(5_000_001)
        .await
        .expect("progress");
    manager
        .update_last_processed_checkpoint(5_000_002)
        .await
        .expect("alias");
    // No other test writes the `default` pipeline: the alias lands exactly.
    assert_eq!(
        manager.get_latest_checkpoint().await.expect("tip"),
        Some(5_000_002)
    );
    assert_eq!(
        manager.get_last_processed_checkpoint().await.expect("last"),
        5_000_002
    );
    manager
        .advance_continuous("pipe-adv", 20, 10, None)
        .await
        .expect("advance");
    let progress = manager
        .get_progress("pipe-adv")
        .await
        .expect("progress")
        .expect("present");
    assert_eq!(progress.continuous_checkpoint, 20);
    manager
        .record_archive_window("pipe-adv", Some(1), Some(20), Some(10))
        .await
        .expect("archive");
    let windowed = manager
        .get_progress("pipe-adv")
        .await
        .expect("progress")
        .expect("present");
    assert_eq!(windowed.archive_lo, Some(1));
    assert_eq!(windowed.archive_hi, Some(20));
}

#[tokio::test]
async fn continuous_rewind_moves_backward() {
    let Some(manager) = manager().await else {
        return;
    };
    let pipe = ns("rewind");
    manager
        .advance_continuous(&pipe, 100, 90, None)
        .await
        .expect("advance");
    // Advance never regresses...
    manager
        .advance_continuous(&pipe, 50, 40, None)
        .await
        .expect("advance");
    let held = manager
        .get_progress(&pipe)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(held.continuous_checkpoint, 100);
    // ...but the rewind path sets unconditionally.
    manager.rewind_continuous(&pipe, 30).await.expect("rewind");
    let rewound = manager
        .get_progress(&pipe)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(rewound.continuous_checkpoint, 30);
    assert_eq!(rewound.floor_checkpoint, 30);
}

#[tokio::test]
async fn gaps_and_digests() {
    let Some(manager) = manager().await else {
        return;
    };
    let base = 6_000_000 + SEQ.fetch_add(1, Ordering::Relaxed) % 1000;
    for seq in [base, base + 1, base + 2] {
        manager
            .store_checkpoint_model(CheckpointModel::new(CheckpointModelConfig {
                sequence_number: seq as i64,
                digest: format!("digest-{seq}"),
                prev_digest: None,
                epoch: 1,
                timestamp_ms: 1_000,
                transaction_count: 0,
                network_total_transactions: 0,
                validator_signature: String::new(),
                end_of_epoch_data: None,
            }))
            .await
            .expect("checkpoint");
    }
    assert_eq!(
        manager.detect_gaps(base, base + 2).await.expect("gaps"),
        vec![]
    );
    assert_eq!(
        manager.detect_gaps(base, base + 4).await.expect("gaps"),
        vec![(base + 3, base + 4)]
    );
    // Degenerate single-point range over a missing height is a one-gap.
    assert_eq!(
        manager.detect_gaps(base + 9, base + 9).await.expect("gaps"),
        vec![(base + 9, base + 9)]
    );
    let digests = manager
        .checkpoint_digests(base, base + 2)
        .await
        .expect("digests");
    assert_eq!(digests.len(), 3);
    assert_eq!(digests[0], (base, format!("digest-{base}")));
}

#[tokio::test]
async fn tip_none_and_zero_on_fresh_database() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else {
        return;
    };
    let dbname = format!("fresh_{}", std::process::id());
    let admin_url = url.replace("sui_indexer_test", "postgres");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .expect("admin");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {dbname}"
    )))
    .execute(&admin)
    .await
    .expect("drop");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {dbname}")))
        .execute(&admin)
        .await
        .expect("create");
    let config = sui_indexer_config::DatabaseConfig {
        url: url.replace("sui_indexer_test", &dbname),
        max_connections: 2,
        min_connections: 1,
        connect_timeout: 10,
        idle_timeout: None,
        auto_migrate: false,
    };
    let manager = StorageManager::new_postgres(config).await.expect("manager");
    manager.initialize().await.expect("migrate");
    // No watermark yet: no tip.
    assert!(
        manager
            .get_latest_checkpoint()
            .await
            .expect("tip")
            .is_none()
    );
    // A zero watermark still means "no tip" (the `> 0` guard).
    manager
        .update_checkpoint_progress(0)
        .await
        .expect("progress");
    assert!(
        manager
            .get_latest_checkpoint()
            .await
            .expect("tip")
            .is_none()
    );
    manager
        .update_checkpoint_progress(5)
        .await
        .expect("progress");
    assert_eq!(manager.get_latest_checkpoint().await.expect("tip"), Some(5));
    drop(manager);
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {dbname}")))
        .execute(&admin)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn repair_queue_round_trip() {
    let Some(manager) = manager().await else {
        return;
    };
    let seq = 7_000_000 + SEQ.fetch_add(1, Ordering::Relaxed) % 1000;
    manager.enqueue_repair(seq, "boom").await.expect("enqueue");
    let claimed = manager.claim_repair_entries(10).await.expect("claim");
    assert!(
        claimed
            .iter()
            .any(|entry| entry.checkpoint_sequence == seq as i64)
    );
    manager
        .complete_repair(seq, true, None, 3, 1)
        .await
        .expect("complete");
    let rest = manager.claim_repair_entries(100).await.expect("claim");
    assert!(
        !rest
            .iter()
            .any(|entry| entry.checkpoint_sequence == seq as i64)
    );
    let parked = seq + 1;
    manager
        .enqueue_repair(parked, "boom")
        .await
        .expect("enqueue");
    manager
        .complete_repair(parked, false, Some("boom"), 1, 0)
        .await
        .expect("fail");
    let after = manager.claim_repair_entries(1000).await.expect("claim");
    assert!(
        !after
            .iter()
            .any(|entry| entry.checkpoint_sequence == parked as i64)
    );
}

#[tokio::test]
async fn prune_removes_below_cutoff() {
    let Some(manager) = manager().await else {
        return;
    };
    let tag = ns("prune");
    for seq in [5u64, 6] {
        manager
            .store_events(vec![sample_event(seq, seq, "0x2")])
            .await
            .expect("event");
        manager
            .store_transaction_models(vec![TransactionModel::new(TransactionModelConfig {
                digest: format!("prune-{tag}-{seq}"),
                checkpoint_sequence: seq as i64,
                timestamp_ms: 1_000,
                sender: "0x1".to_owned(),
                gas_used: None,
                gas_price: None,
                success: true,
                error_message: None,
            })])
            .await
            .expect("tx");
        manager
            .store_object_models(vec![ObjectModel::new(ObjectModelConfig {
                object_id: format!("prune-{tag}-{seq}"),
                version: 1,
                digest: "d".to_owned(),
                checkpoint_sequence: seq as i64,
                transaction_digest: format!("prune-{tag}-{seq}"),
                sender: "0x1".to_owned(),
            })])
            .await
            .expect("object");
    }
    // Cutoff 10: rows at 5 and 6 go
    // (2 events + 2 txs + 2 processed txs + 2 objects).
    for seq in [5u64, 6] {
        manager
            .store_transactions(vec![sample_transaction(seq as u8, seq)])
            .await
            .expect("processed tx");
    }
    let removed = manager.prune_checkpoints(100, 90).await.expect("prune");
    assert!(removed >= 8);
    assert!(
        manager
            .get_events_by_checkpoint_range(5, 6)
            .await
            .expect("range")
            .is_empty()
    );
    assert!(
        manager
            .query_transactions(TransactionQueryFilter {
                from_checkpoint: Some(5),
                to_checkpoint: Some(6),
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("query")
            .is_empty()
    );
    // Retention covering everything prunes nothing.
    assert_eq!(manager.prune_checkpoints(5, 90).await.expect("prune"), 0);
}

#[tokio::test]
async fn gateway_type_branches_and_truncation() {
    let Some(manager) = manager().await else {
        return;
    };
    // INT8 / INT4 / BOOL / NUMERIC / JSON / BYTEA / TIMESTAMPTZ / UUID / TEXT.
    let result = manager
        .sql_query(
            "SELECT 1::INT8 AS i8, 2::INT4 AS i4, true AS b, 1.5::NUMERIC AS n, \
             '{\"a\":1}'::JSONB AS j, '\\x0102'::BYTEA AS by, NOW() AS t, \
             NOW()::TIMESTAMP AS t2, gen_random_uuid() AS u, 'hi' AS s",
            10,
            1024 * 1024,
        )
        .await
        .expect("types");
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row["i8"], serde_json::json!(1));
    assert_eq!(row["i4"], serde_json::json!(2));
    assert_eq!(row["b"], serde_json::json!(true));
    assert_eq!(row["n"], serde_json::json!("1.5"));
    assert_eq!(row["j"], serde_json::json!({ "a": 1 }));
    assert_eq!(row["by"], serde_json::json!("0102"));
    assert!(row["t"].is_string());
    assert!(row["t2"].is_string());
    assert_eq!(row["u"].as_str().map(str::len), Some(36));
    assert_eq!(row["s"], serde_json::json!("hi"));

    // Row-limit truncation.
    let limited = manager
        .sql_query("SELECT 1 AS a UNION ALL SELECT 2 AS a", 1, 1024 * 1024)
        .await
        .expect("limit");
    assert!(limited.truncated);
    assert_eq!(limited.row_count, 1);

    // Byte-budget truncation.
    let heavy = manager
        .sql_query(
            "SELECT repeat('x', 2000) AS s UNION ALL SELECT repeat('y', 2000) AS s",
            100,
            0,
        )
        .await
        .expect("bytes");
    assert!(heavy.truncated);

    // Generous budget keeps every row.
    let kept = manager
        .sql_query("SELECT 1 AS a UNION ALL SELECT 2 AS a", 100, 1024 * 1024)
        .await
        .expect("kept");
    assert!(!kept.truncated);
    assert_eq!(kept.row_count, 2);

    // Exactly at the byte cap stays (the cap is strict `>`).
    let content = "x".repeat(1016);
    let exact = manager
        .sql_query(&format!("SELECT '{content}' AS s"), 100, 1024)
        .await
        .expect("exact");
    assert_eq!(exact.rows.len(), 1);
    assert_eq!(exact.rows[0]["s"].as_str().map(str::len), Some(1016));
    assert!(!exact.truncated);

    // Errors surface sanitized: short ones intact, long ones cut at 500.
    let short = manager
        .sql_query("SELECT * FROM no_such_table_xyz", 10, 1024)
        .await;
    assert!(short.is_err());
    assert!(short.unwrap_err().to_string().contains("does not exist"));
    // A 600-char server message compacts to the 500-char budget plus the
    // `query failed: ` prefix (PG truncates long identifiers itself, so the
    // length comes from a RAISE payload instead).
    let long = manager
        .sql_query(
            "DO $$ BEGIN RAISE EXCEPTION '%', repeat('x', 600); END $$",
            10,
            1024,
        )
        .await;
    let message = long.unwrap_err().to_string();
    assert!(message.starts_with("query failed: "));
    assert!(message.len() < 600);
    assert!(message.len() <= 500 + "query failed: ".len());
}
