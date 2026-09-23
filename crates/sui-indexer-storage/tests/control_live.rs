//! Control-plane and skeleton round trips against a live PostgreSQL.
//!
//! Runs when `DATABASE_URL` points at a scratch database (vacuous pass
//! otherwise so ordinary `cargo test` needs no DB). Every mutant run is a
//! fresh process, so all names are namespaced by pid.

use std::sync::atomic::{AtomicU64, Ordering};

use sui_indexer_storage::{
    JobControlPlane, PostgresStorage, VersionStatus, prune_above, store_decoded_block,
};

static INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static SEQ: AtomicU64 = AtomicU64::new(0);

async fn pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    // Reset once per process so the `run_migrations`-noop mutant is observable.
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
        sui_indexer_storage::migrations::run_migrations(&pool)
            .await
            .expect("migrate");
    })
    .await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .ok()?;
    Some(pool)
}

fn ns() -> String {
    format!(
        "test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[tokio::test]
async fn job_lifecycle_round_trip() {
    let Some(pool) = pool().await else { return };
    let storage = PostgresStorage::from_pool(pool);
    let chain = ns();
    let spec = serde_json::json!({ "name": "sandwich" });

    storage
        .upsert_job(&chain, "sandwich", &spec, "hash1", "active")
        .await
        .expect("upsert");
    let job = storage
        .get_job(&chain, "sandwich")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(job.spec_hash, "hash1");
    assert_eq!(storage.list_jobs(&chain).await.expect("list").len(), 1);

    storage
        .insert_job_version(&chain, "sandwich", 1, "hash1", 0, None)
        .await
        .expect("version");
    assert_eq!(
        storage
            .list_job_versions(&chain, "sandwich")
            .await
            .expect("versions")
            .len(),
        1
    );
    let version = storage
        .get_job_version(&chain, "sandwich", 1)
        .await
        .expect("get version")
        .expect("present");
    assert_eq!(version.status, "draft");
    for status in [
        VersionStatus::Scanning,
        VersionStatus::CatchingUp,
        VersionStatus::Active,
    ] {
        storage
            .set_version_status(&chain, "sandwich", 1, status, None)
            .await
            .expect("transition");
    }
    // Illegal jump is rejected: active cannot go back to scanning.
    assert!(
        storage
            .set_version_status(&chain, "sandwich", 1, VersionStatus::Scanning, None)
            .await
            .is_err()
    );

    storage
        .advance_version_cursor(&chain, "sandwich", 1, 99, 10)
        .await
        .expect("cursor");
    let cursor = storage
        .get_cursor(&chain, "sandwich", 1)
        .await
        .expect("cursor")
        .expect("present");
    assert_eq!(cursor.cursor, 99);

    storage
        .upsert_feed(&chain, "prices", "cex", &serde_json::json!({}))
        .await
        .expect("feed");
    assert!(
        storage
            .get_feed(&chain, "prices")
            .await
            .expect("feed")
            .is_some()
    );

    storage
        .upsert_catalog_object(&sui_indexer_storage::CatalogObjectRow {
            chain_id: chain.clone(),
            name: "job_sandwich".to_owned(),
            kind: "view".to_owned(),
            ddl: "CREATE VIEW job_sandwich AS SELECT 1".to_owned(),
            select_sql: None,
            checksum: "abc".to_owned(),
            public: true,
            block_column: Some("_height".to_owned()),
            reorg_mode: "block_scoped".to_owned(),
            owner_job: Some("sandwich".to_owned()),
            backfill: "ranged".to_owned(),
        })
        .await
        .expect("catalog");
    assert_eq!(
        storage
            .list_public_catalog(&chain)
            .await
            .expect("catalog")
            .len(),
        1
    );
    storage
        .delete_catalog_object(&chain, "job_sandwich")
        .await
        .expect("catalog delete");
    assert!(
        storage
            .list_public_catalog(&chain)
            .await
            .expect("catalog")
            .is_empty()
    );

    storage
        .delete_job(&chain, "sandwich")
        .await
        .expect("delete");
    assert!(
        storage
            .get_job(&chain, "sandwich")
            .await
            .expect("get")
            .is_none()
    );
}

#[tokio::test]
async fn delete_job_drops_physical_tables_and_alias() {
    let Some(pool) = pool().await else { return };
    let storage = PostgresStorage::from_pool(pool);
    let chain = ns();
    let table = format!("job_gone_{}", std::process::id());
    let spec = serde_json::json!({ "name": "gone", "output": { "table": table } });
    storage
        .upsert_job(&chain, "gone", &spec, "hash", "active")
        .await
        .expect("job");
    storage
        .insert_job_version(&chain, "gone", 1, "hash", 0, None)
        .await
        .expect("version");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {table}__v1 (x BIGINT)"
    )))
    .execute(storage.pool())
    .await
    .expect("ddl");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE VIEW {table} AS SELECT * FROM {table}__v1"
    )))
    .execute(storage.pool())
    .await
    .expect("ddl");
    storage.delete_job(&chain, "gone").await.expect("delete");
    for object in [format!("{table}__v1"), table] {
        let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(&object)
            .fetch_one(storage.pool())
            .await
            .expect("check");
        assert!(exists.is_none(), "{object} should be gone");
    }
}

#[tokio::test]
async fn work_queue_claim_and_complete() {
    let Some(pool) = pool().await else { return };
    let storage = PostgresStorage::from_pool(pool);
    let chain = ns();
    storage
        .enqueue_work(&chain, "job", 1, 0, 99)
        .await
        .expect("enqueue");
    storage
        .enqueue_work(&chain, "job", 1, 100, 199)
        .await
        .expect("enqueue");
    let claimed = storage.claim_work(&chain, "job", 10).await.expect("claim");
    assert_eq!(claimed.len(), 2);
    // Newest ranges first.
    assert!(claimed[0].range_lo >= claimed[1].range_lo);
    storage
        .complete_work(claimed[0].id, true, None, 3, 1)
        .await
        .expect("complete");
    let rest = storage.claim_work(&chain, "job", 10).await.expect("claim");
    assert_eq!(rest.len(), 1);
    storage
        .complete_work(rest[0].id, false, Some("boom"), 1, 0)
        .await
        .expect("fail");
}

#[tokio::test]
async fn skeleton_store_is_idempotent_and_prunable() {
    let Some(pool) = pool().await else { return };
    let chain = ns();
    let block = sample_block(50);
    let first = store_decoded_block(&pool, &chain, &block)
        .await
        .expect("store");
    assert_eq!((first.blocks, first.txs, first.events), (1, 1, 1));
    let second = store_decoded_block(&pool, &chain, &block)
        .await
        .expect("store");
    assert_eq!((second.blocks, second.txs, second.events), (0, 0, 0));

    // Catalog-driven prune removes job rows above the fork.
    sqlx::query("CREATE TABLE IF NOT EXISTS test_prune__v1 (_height BIGINT)")
        .execute(&pool)
        .await
        .expect("ddl");
    sqlx::query("INSERT INTO test_prune__v1 VALUES (60), (40)")
        .execute(&pool)
        .await
        .expect("rows");
    let removed = prune_above(
        &pool,
        &chain,
        45,
        &[("test_prune__v1".to_owned(), "_height".to_owned())],
    )
    .await
    .expect("prune");
    // 1 skeleton block + 1 tx + 1 event + 1 job row above 45.
    assert_eq!(removed, 4);
    sqlx::query("DROP TABLE test_prune__v1")
        .execute(&pool)
        .await
        .expect("drop");
}

fn sample_block(height: u64) -> chain_core::DecodedBlock {
    use chain_core::{BlockRow, Commitment, CoreRows, DecodedBlock, Ev, ParentRef, TxRow};
    let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap_or_default();
    DecodedBlock {
        height,
        hash: vec![1],
        parent: ParentRef::from_hash(vec![0]),
        ts,
        commitment: Commitment::Final,
        rows: CoreRows {
            block: Some(BlockRow {
                height,
                hash: vec![1],
                parent_hash: vec![0],
                parent_ref: serde_json::json!({}),
                ts,
                commitment: 2,
                chain_meta: serde_json::json!({}),
            }),
            txs: vec![TxRow {
                height,
                block_ts: ts,
                tx_index: 0,
                tx_hash: vec![2],
                sender: "s".to_owned(),
                success: true,
                fee: 0,
                chain_meta: serde_json::json!({}),
            }],
        },
        events: vec![Ev {
            height,
            block_ts: ts,
            tx_index: 0,
            ev_index: 0,
            inner_ix: 0,
            stack_height: 0,
            emitter: "e".to_owned(),
            topics: vec![],
            payload: vec![],
            tx_hash: vec![2],
            sender: "s".to_owned(),
            extra: serde_json::json!({}),
        }],
        native: vec![],
        skipped: false,
    }
}
