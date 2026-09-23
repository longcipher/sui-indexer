use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Sse;
use axum::response::sse::Event as SseEvent;
use axum::routing::get;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use sui_indexer_config::IndexerConfig;
use sui_indexer_storage::StorageManager;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tracing::info;

use crate::BlockFeed;
use crate::query_gateway::{GatewayQuery, QueryGateway, append_limit};

/// Shared state for the query API.
#[derive(Clone)]
pub struct ApiState {
    pub(crate) storage: StorageManager,
    network: String,
    pub(crate) chain_id: String,
    gateway: Arc<QueryGateway>,
    feed: Arc<BlockFeed>,
    metrics: Arc<ApiMetrics>,
    job_metrics: Arc<job_engine::JobMetricsRegistry>,
}

/// In-memory API counters backing the Prometheus endpoint.
#[derive(Debug, Default)]
pub struct ApiMetrics {
    /// Served gateway queries.
    pub queries_total: std::sync::atomic::AtomicU64,
    /// Served SSE connections.
    pub live_connections_total: std::sync::atomic::AtomicU64,
    /// Failed queries.
    pub query_errors_total: std::sync::atomic::AtomicU64,
}

/// Event list query parameters.
#[derive(Debug, Deserialize)]
pub struct EventQuery {
    package: Option<String>,
    module: Option<String>,
    event_type: Option<String>,
    sender: Option<String>,
    from: Option<u64>,
    to: Option<u64>,
    limit: Option<u64>,
}

/// Transaction list query parameters.
#[derive(Debug, Deserialize)]
pub struct TransactionQuery {
    sender: Option<String>,
    from: Option<u64>,
    to: Option<u64>,
    limit: Option<u64>,
}

/// Health response body.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    healthy: bool,
    network: String,
    watermark: Option<u64>,
}

/// Metrics response body.
#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    watermark: Option<u64>,
    events_total: u64,
}

/// SQL gateway query parameters.
#[derive(Debug, Deserialize)]
pub struct SqlQuery {
    sql: String,
    event: Option<String>,
    limit: Option<u64>,
}

/// Live SSE parameters.
#[derive(Debug, Deserialize)]
pub struct LiveQuery {
    from: Option<u64>,
}

/// Build the query API router.
pub fn router(state: ApiState) -> Router {
    use axum::routing::{post, put};
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(prometheus_metrics))
        .route("/status", get(status))
        .route("/query", get(run_sql_query))
        .route("/query/live", get(live_checkpoints))
        .route("/events", get(list_events))
        .route("/transactions", get(list_transactions))
        .route("/checkpoints/{sequence}/events", get(checkpoint_events))
        .route(
            "/jobs",
            get(crate::api_jobs::list_jobs).post(crate::api_jobs::apply_job),
        )
        .route(
            "/jobs/{name}",
            put(crate::api_jobs::update_job).delete(crate::api_jobs::delete_job),
        )
        .route("/jobs/{name}/rescan", post(crate::api_jobs::rescan_job))
        .route("/jobs/{name}/retire", post(crate::api_jobs::retire_job))
        .route("/jobs/{name}/plan", get(crate::api_jobs::plan_job))
        .with_state(state)
}

/// Create API state from config, feed, and storage.
pub fn api_state(
    config: &IndexerConfig,
    storage: &StorageManager,
    feed: &Arc<BlockFeed>,
) -> ApiState {
    api_state_with_metrics(
        config,
        storage,
        feed,
        &Arc::new(job_engine::JobMetricsRegistry::new()),
    )
}

/// Create API state sharing the job runner's metrics registry.
pub fn api_state_with_metrics(
    config: &IndexerConfig,
    storage: &StorageManager,
    feed: &Arc<BlockFeed>,
    job_metrics: &Arc<job_engine::JobMetricsRegistry>,
) -> ApiState {
    let chain_id = crate::chains::chain_id(config);
    ApiState {
        storage: storage.clone(),
        network: config.network.network.clone(),
        chain_id,
        gateway: Arc::new(QueryGateway::new(config.query.clone())),
        feed: Arc::clone(feed),
        metrics: Arc::new(ApiMetrics::default()),
        job_metrics: Arc::clone(job_metrics),
    }
}

/// Serve the query API until cancelled.
pub async fn serve(
    config: &IndexerConfig,
    storage: &StorageManager,
    feed: &Arc<BlockFeed>,
    job_metrics: &Arc<job_engine::JobMetricsRegistry>,
) -> Result<()> {
    if !config.api.enabled {
        return Ok(());
    }
    let state = api_state_with_metrics(config, storage, feed, job_metrics);
    let listener = tokio::net::TcpListener::bind(&config.api.listen).await?;
    info!("Query API listening on {}", config.api.listen);
    axum::serve(listener, router(state)).await?;
    Ok(())
}

async fn health(State(state): State<ApiState>) -> Json<HealthResponse> {
    let watermark = state.storage.get_latest_checkpoint().await.unwrap_or(None);
    Json(HealthResponse {
        healthy: true,
        network: state.network,
        watermark,
    })
}

async fn ready(State(state): State<ApiState>) -> Result<Json<HealthResponse>, StatusCode> {
    let healthy = state.storage.health_check().await.unwrap_or(false);
    if healthy {
        Ok(Json(HealthResponse {
            healthy,
            network: state.network,
            watermark: state.storage.get_latest_checkpoint().await.unwrap_or(None),
        }))
    } else {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    }
}

async fn prometheus_metrics(State(state): State<ApiState>) -> String {
    use std::sync::atomic::Ordering;
    let watermark = state
        .storage
        .get_latest_checkpoint()
        .await
        .unwrap_or(None)
        .unwrap_or(0);
    let counts = state.storage.table_counts().await.unwrap_or_default();
    let queries = state.metrics.queries_total.load(Ordering::Relaxed);
    let live = state.metrics.live_connections_total.load(Ordering::Relaxed);
    let errors = state.metrics.query_errors_total.load(Ordering::Relaxed);
    let base = format!(
        "# HELP sui_indexer_committed_checkpoint Last contiguously committed checkpoint.\n\
         # TYPE sui_indexer_committed_checkpoint gauge\n\
         sui_indexer_committed_checkpoint {watermark}\n\
         # HELP sui_indexer_table_rows Rows per canonical table.\n\
         # TYPE sui_indexer_table_rows gauge\n\
         sui_indexer_table_rows{{table=\"checkpoints\"}} {}\n\
         sui_indexer_table_rows{{table=\"transactions\"}} {}\n\
         sui_indexer_table_rows{{table=\"events\"}} {}\n\
         sui_indexer_table_rows{{table=\"objects\"}} {}\n\
         # HELP sui_indexer_gateway_queries_total Served gateway queries.\n\
         # TYPE sui_indexer_gateway_queries_total counter\n\
         sui_indexer_gateway_queries_total {queries}\n\
         # HELP sui_indexer_live_connections_total Served SSE connections.\n\
         # TYPE sui_indexer_live_connections_total counter\n\
         sui_indexer_live_connections_total {live}\n\
         # HELP sui_indexer_query_errors_total Failed gateway queries.\n\
         # TYPE sui_indexer_query_errors_total counter\n\
         sui_indexer_query_errors_total {errors}\n",
        counts.checkpoints, counts.transactions, counts.events, counts.objects,
    );
    format!("{base}{}", state.job_metrics.render())
}

async fn status(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let watermark = state.storage.get_latest_checkpoint().await.unwrap_or(None);
    let progress = state.storage.get_progress("default").await.unwrap_or(None);
    let counts = state.storage.table_counts().await.unwrap_or_default();
    let gaps = match progress {
        Some(ref progress) => {
            let tip = watermark.unwrap_or(progress.continuous_checkpoint.max(0) as u64);
            state
                .storage
                .detect_gaps(progress.floor_checkpoint.max(0) as u64, tip)
                .await
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    Json(serde_json::json!({
        "healthy": true,
        "network": state.network,
        "watermark": watermark,
        "progress": progress,
        "tables": {
            "checkpoints": counts.checkpoints,
            "transactions": counts.transactions,
            "events": counts.events,
            "objects": counts.objects,
        },
        "gaps": gaps.iter().map(|(start, end)| serde_json::json!({
            "start": start,
            "end": end,
        })).collect::<Vec<_>>(),
    }))
}

async fn run_sql_query(
    State(state): State<ApiState>,
    Query(query): Query<SqlQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use std::sync::atomic::Ordering;
    // Allow-list = core tables ∪ `catalog_objects WHERE public`.
    let catalog = QueryGateway::public_catalog_tables(&state.storage, &state.chain_id).await;
    let validated = state
        .gateway
        .validate_with_catalog(&query.sql, &catalog)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let limited = append_limit(&validated, query.limit.unwrap_or(1000).clamp(1, 1000));
    let result = state
        .gateway
        .execute_with_catalog(
            &state.storage,
            GatewayQuery {
                sql: limited,
                event: query.event,
                limit: query.limit,
            },
            &catalog,
        )
        .await
        .map_err(|_| {
            state
                .metrics
                .query_errors_total
                .fetch_add(1, Ordering::Relaxed);
            StatusCode::UNPROCESSABLE_ENTITY
        })?;
    state.metrics.queries_total.fetch_add(1, Ordering::Relaxed);
    Ok(Json(serde_json::json!(result)))
}

async fn live_checkpoints(
    State(state): State<ApiState>,
    Query(query): Query<LiveQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    use std::sync::atomic::Ordering;
    state
        .metrics
        .live_connections_total
        .fetch_add(1, Ordering::Relaxed);
    let from = query.from.unwrap_or(0);
    let stream =
        BroadcastStream::new(state.feed.subscribe()).filter_map(move |message| match message {
            Ok(update) => live_event(update, from).map(Ok),
            Err(_) => None,
        });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Whether a broadcast block update passes the live-stream floor.
fn live_update_visible(update: &crate::block_feed::BlockUpdate, from: u64) -> bool {
    update.committed >= from
}

/// Build the SSE event for a live update, or `None` below the floor.
fn live_event(update: crate::block_feed::BlockUpdate, from: u64) -> Option<SseEvent> {
    live_update_visible(&update, from).then(|| {
        SseEvent::default().data(
            serde_json::json!({
                "committed": update.committed,
                "latest": update.latest,
            })
            .to_string(),
        )
    })
}

async fn list_events(
    State(state): State<ApiState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let events = state
        .storage
        .query_events(sui_indexer_storage::EventQueryFilter {
            package: query.package.as_deref(),
            module: query.module.as_deref(),
            event_type: query.event_type.as_deref(),
            sender: query.sender.as_deref(),
            from_checkpoint: query.from,
            to_checkpoint: query.to,
            limit: query.limit.unwrap_or(50),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "data": events })))
}

async fn list_transactions(
    State(state): State<ApiState>,
    Query(query): Query<TransactionQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let transactions = state
        .storage
        .query_transactions(sui_indexer_storage::TransactionQueryFilter {
            sender: query.sender.as_deref(),
            from_checkpoint: query.from,
            to_checkpoint: query.to,
            limit: query.limit.unwrap_or(50),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "data": transactions })))
}

async fn checkpoint_events(
    State(state): State<ApiState>,
    Path(sequence): Path<u64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let events = state
        .storage
        .get_events_by_checkpoint_range(sequence, sequence)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "data": events })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_events_carry_updates_above_the_floor() {
        let update = crate::block_feed::BlockUpdate {
            committed: 10,
            latest: 12,
        };
        let event = live_event(update, 10).expect("visible");
        let body = format!("{:?}", event);
        assert!(body.contains("10"));
        assert!(live_event(update, 11).is_none());
    }

    #[test]
    fn live_floor_filters_stale_updates() {
        let update = crate::block_feed::BlockUpdate {
            committed: 10,
            latest: 12,
        };
        assert!(live_update_visible(&update, 10));
        assert!(live_update_visible(&update, 0));
        assert!(!live_update_visible(&update, 11));
    }

    async fn live_state() -> Option<ApiState> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let config = sui_indexer_config::IndexerConfig::default();
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
        let feed = Arc::new(BlockFeed::new(8));
        Some(api_state(&config, &storage, &feed))
    }

    #[tokio::test]
    async fn prometheus_exposes_core_gauges() {
        let Some(state) = live_state().await else {
            return;
        };
        let body = prometheus_metrics(State(state)).await;
        assert!(body.contains("sui_indexer_committed_checkpoint"));
        assert!(body.contains("sui_indexer_gateway_queries_total"));
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn router_serves_health() {
        let Some(state) = live_state().await else {
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.expect("serve");
        });
        let body: serde_json::Value = reqwest::Client::new()
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("health")
            .error_for_status()
            .expect("ok")
            .json()
            .await
            .expect("json");
        assert_eq!(body["healthy"], serde_json::json!(true));
        // Unknown routes 404: the router is not the default one.
        let missing = reqwest::Client::new()
            .get(format!("http://{addr}/nope"))
            .send()
            .await
            .expect("missing");
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn enabled_api_serves_until_cancelled() {
        let Some(state) = live_state().await else {
            return;
        };
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.api.enabled = true;
        config.api.listen = "127.0.0.1:0".to_string();
        // Enabled serve binds and runs: it must NOT return immediately.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            serve(&config, &state.storage, &state.feed, &state.job_metrics),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn disabled_api_returns_immediately() {
        let Some(state) = live_state().await else {
            return;
        };
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.api.enabled = false;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            serve(&config, &state.storage, &state.feed, &state.job_metrics),
        )
        .await
        .expect("returns");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn collection_handlers_return_shaped_bodies() {
        let Some(state) = live_state().await else {
            return;
        };
        let events = list_events(
            State(state.clone()),
            Query(EventQuery {
                package: None,
                module: None,
                event_type: None,
                sender: None,
                from: None,
                to: None,
                limit: Some(5),
            }),
        )
        .await
        .expect("events");
        assert!(events.0.get("data").is_some());
        let txs = list_transactions(
            State(state.clone()),
            Query(TransactionQuery {
                sender: None,
                from: None,
                to: None,
                limit: Some(5),
            }),
        )
        .await
        .expect("transactions");
        assert!(txs.0.get("data").is_some());
        let checkpoint = checkpoint_events(State(state.clone()), Path(1))
            .await
            .expect("checkpoint");
        assert!(checkpoint.0.get("data").is_some());
        let status = status(State(state.clone())).await;
        assert_eq!(status.0["healthy"], serde_json::json!(true));
        assert!(status.0.get("network").is_some());
        let health = health(State(state.clone())).await;
        assert!(health.0.healthy);
        assert!(ready(State(state.clone())).await.is_ok());
    }

    #[tokio::test]
    async fn prometheus_includes_job_gauges() {
        let Some(state) = live_state().await else {
            return;
        };
        state.job_metrics.register(
            "test/job/1".to_string(),
            std::sync::Arc::new(job_engine::JobMetrics::labelled("test", "job", 1)),
        );
        let body = prometheus_metrics(State(state)).await;
        assert!(body.contains("indexer_job_scan_height"));
        assert!(body.contains("{chain=\"test\",job=\"job\",version=\"1\"}"));
    }
}
