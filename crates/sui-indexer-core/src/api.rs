use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use eyre::Result;
use serde::{Deserialize, Serialize};
use sui_indexer_config::IndexerConfig;
use sui_indexer_storage::StorageManager;
use tracing::info;

/// Shared state for the query API.
#[derive(Clone)]
pub struct ApiState {
    storage: StorageManager,
    network: String,
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
    events_total: usize,
}

/// Build the query API router.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/events", get(list_events))
        .route("/transactions", get(list_transactions))
        .route("/checkpoints/:sequence/events", get(checkpoint_events))
        .with_state(state)
}

/// Create API state from config and storage.
pub fn api_state(config: &IndexerConfig, storage: &StorageManager) -> ApiState {
    ApiState {
        storage: storage.clone(),
        network: config.network.network.clone(),
    }
}

/// Serve the query API until cancelled.
pub async fn serve(config: &IndexerConfig, storage: &StorageManager) -> Result<()> {
    if !config.api.enabled {
        return Ok(());
    }
    let state = api_state(config, storage);
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

async fn metrics(State(state): State<ApiState>) -> Json<MetricsResponse> {
    let watermark = state.storage.get_latest_checkpoint().await.unwrap_or(None);
    Json(MetricsResponse {
        watermark,
        events_total: 0,
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
