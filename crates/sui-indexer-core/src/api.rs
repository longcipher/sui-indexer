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
use crate::query_gateway::{GatewayQuery, QueryGateway, append_limit, validate_select};

/// Shared state for the query API.
#[derive(Clone)]
pub struct ApiState {
    storage: StorageManager,
    network: String,
    gateway: Arc<QueryGateway>,
    feed: Arc<BlockFeed>,
    metrics: Arc<ApiMetrics>,
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
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(prometheus_metrics))
        .route("/status", get(status))
        .route("/query", get(run_sql_query))
        .route("/query/live", get(live_checkpoints))
        .route("/events", get(list_events))
        .route("/transactions", get(list_transactions))
        .route("/checkpoints/:sequence/events", get(checkpoint_events))
        .with_state(state)
}

/// Create API state from config, feed, and storage.
pub fn api_state(
    config: &IndexerConfig,
    storage: &StorageManager,
    feed: &Arc<BlockFeed>,
) -> ApiState {
    ApiState {
        storage: storage.clone(),
        network: config.network.network.clone(),
        gateway: Arc::new(QueryGateway::new(config.query.clone())),
        feed: Arc::clone(feed),
        metrics: Arc::new(ApiMetrics::default()),
    }
}

/// Serve the query API until cancelled.
pub async fn serve(
    config: &IndexerConfig,
    storage: &StorageManager,
    feed: &Arc<BlockFeed>,
) -> Result<()> {
    if !config.api.enabled {
        return Ok(());
    }
    let state = api_state(config, storage, feed);
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
    format!(
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
    )
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
    let validated = validate_select(&query.sql).map_err(|_| StatusCode::BAD_REQUEST)?;
    let limited = append_limit(&validated, query.limit.unwrap_or(1000).clamp(1, 1000));
    let result = state
        .gateway
        .execute(
            &state.storage,
            GatewayQuery {
                sql: limited,
                event: query.event,
                limit: query.limit,
            },
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
            Ok(update) if update.committed >= from => Some(Ok::<_, Infallible>(
                SseEvent::default().data(
                    serde_json::json!({
                        "committed": update.committed,
                        "latest": update.latest,
                    })
                    .to_string(),
                ),
            )),
            Ok(_) | Err(_) => None,
        });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
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
