//! Progress resolution against a live PostgreSQL.
//!
//! Runs when `DATABASE_URL` points at a scratch database (vacuous pass
//! otherwise so ordinary `cargo test` needs no DB). Each test process gets a
//! fresh database so watermarks and progress rows never leak across runs.

use sui_indexer_core::progress::Progress;
use sui_indexer_storage::StorageManager;

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn fresh_manager() -> Option<StorageManager> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dbname = format!("fresh_core_{}_{}", std::process::id(), n);
    let admin_url = url.replace("sui_indexer_test", "postgres");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .ok()?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {dbname}"
    )))
    .execute(&admin)
    .await
    .ok()?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {dbname}")))
        .execute(&admin)
        .await
        .ok()?;
    let db = sui_indexer_config::DatabaseConfig {
        url: url.replace("sui_indexer_test", &dbname),
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

#[tokio::test]
async fn explicit_start_wins() {
    let Some(storage) = fresh_manager().await else {
        return;
    };
    assert_eq!(
        Progress::resolve_resume(&storage, Some(100))
            .await
            .expect("resume"),
        100
    );
}

#[tokio::test]
async fn continuous_plus_one_with_watermark_fallback() {
    let Some(storage) = fresh_manager().await else {
        return;
    };
    storage
        .advance_continuous("default", 50, 40, None)
        .await
        .expect("advance");
    assert_eq!(
        Progress::resolve_resume(&storage, None)
            .await
            .expect("resume"),
        51
    );
    assert_eq!(
        Progress::frontier(&storage).await.expect("frontier"),
        (50, 40)
    );
}

#[tokio::test]
async fn zero_progress_falls_back_to_watermark() {
    let Some(storage) = fresh_manager().await else {
        return;
    };
    storage
        .update_checkpoint_progress(5)
        .await
        .expect("watermark");
    storage
        .advance_continuous("default", 0, 0, None)
        .await
        .expect("advance");
    // Continuous 0 is not progress: the watermark decides.
    assert_eq!(
        Progress::resolve_resume(&storage, None)
            .await
            .expect("resume"),
        6
    );
}
