//! Admin job API: registry reads plus gated mutations.
//!
//! `GET /jobs`, `POST /jobs`, `PUT /jobs/:name`, `DELETE /jobs/:name`,
//! `POST /jobs/:name/rescan`, `POST /jobs/:name/retire`,
//! `GET /jobs/:name/plan`. Mutations require the `x-indexer-admin: 1`
//! header on top of the deployment's trusted-network gate.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sui_indexer_storage::{JobControlPlane, VersionStatus};

use crate::api::ApiState;

/// Admin header value that authorises job mutations.
pub const ADMIN_HEADER: &str = "x-indexer-admin";

fn require_admin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let authorised = headers
        .get(ADMIN_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some("1");
    if authorised {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Job spec apply body.
#[derive(Debug, Deserialize)]
pub struct ApplyJobBody {
    /// Job spec (TOML `[[jobs]]` shape, as JSON).
    pub spec: sui_indexer_config::JobSpec,
}

/// Desired-state update body.
#[derive(Debug, Deserialize)]
pub struct DesiredBody {
    /// `active` | `paused` | `retired`.
    pub desired: String,
}

/// Rescan body.
#[derive(Debug, Deserialize)]
pub struct RescanBody {
    /// First height (default 0: full-chain re-scan).
    pub from: Option<u64>,
    /// Explicit version (default: latest + 1).
    pub version: Option<u32>,
}

/// Plan query.
#[derive(Debug, Deserialize)]
pub struct PlanQuery {
    /// Tip used to bound `to = "head"` scans.
    pub tip: Option<u64>,
}

/// Job summary with its versions.
#[derive(Debug, Serialize)]
pub struct JobSummary {
    chain_id: String,
    name: String,
    desired: String,
    spec_hash: String,
    versions: Vec<sui_indexer_storage::JobVersionRow>,
}

/// List jobs with versions for this chain.
pub async fn list_jobs(State(state): State<ApiState>) -> Result<Json<Vec<JobSummary>>, StatusCode> {
    let control = state.storage.postgres().clone();
    let jobs = control
        .list_jobs(&state.chain_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs {
        let versions = control
            .list_job_versions(&state.chain_id, &job.name)
            .await
            .unwrap_or_default();
        out.push(JobSummary {
            chain_id: job.chain_id,
            name: job.name,
            desired: job.desired,
            spec_hash: job.spec_hash,
            versions,
        });
    }
    Ok(Json(out))
}

/// Apply (create or bump) a job.
pub async fn apply_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ApplyJobBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_admin(&headers)?;
    let spec = body.spec;
    spec.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
    let control = state.storage.postgres().clone();
    let spec_json = serde_json::to_value(&spec).map_err(|_| StatusCode::BAD_REQUEST)?;
    let hash = spec.spec_hash();
    control
        .upsert_job(
            &state.chain_id,
            &spec.name,
            &spec_json,
            &hash,
            &spec.desired,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let existing = control
        .get_job(&state.chain_id, &spec.name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let versions = control
        .list_job_versions(&state.chain_id, &spec.name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let latest_version = versions.iter().map(|row| row.version).max();
    // A changed spec auto-bumps the version: re-applying edited logic at the
    // same version builds a new version instead of dropping the edit.
    let latest_info = match (&existing, latest_version) {
        (Some(job), Some(version)) => {
            let stored_hash = versions
                .iter()
                .find(|row| row.version == version)
                .map(|row| row.spec_hash.as_str())
                .unwrap_or("");
            Some((version as u32, stored_hash, job.desired.as_str()))
        }
        _ => None,
    };
    let decision =
        job_engine::decide_apply(&spec, latest_info).map_err(|_| StatusCode::BAD_REQUEST)?;
    let version = decision.target_version() as i32;
    control
        .insert_job_version(
            &state.chain_id,
            &spec.name,
            version,
            &hash,
            spec.scan.from as i64,
            job_engine::parse_scan_to(&spec.scan.to, u64::MAX).map(|v| v as i64),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "chain": state.chain_id,
        "name": spec.name,
        "version": version,
        "spec_hash": hash,
    })))
}

/// Update a job's desired state.
pub async fn update_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<DesiredBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_admin(&headers)?;
    if !matches!(body.desired.as_str(), "active" | "paused" | "retired") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let control = state.storage.postgres().clone();
    let Some(mut job) = control
        .get_job(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    job.desired = body.desired.clone();
    let mut spec: sui_indexer_config::JobSpec =
        serde_json::from_value(job.spec).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    spec.desired = body.desired.clone();
    let spec_json = serde_json::to_value(&spec).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    control
        .upsert_job(
            &state.chain_id,
            &name,
            &spec_json,
            &job.spec_hash,
            &body.desired,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        serde_json::json!({ "name": name, "desired": body.desired }),
    ))
}

/// Delete a job and its versions, cursors and owned catalog objects.
pub async fn delete_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, StatusCode> {
    require_admin(&headers)?;
    state
        .storage
        .postgres()
        .delete_job(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Trigger an explicit re-scan: a new version from `from`.
pub async fn rescan_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<RescanBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_admin(&headers)?;
    let control = state.storage.postgres().clone();
    let Some(job) = control
        .get_job(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let spec: sui_indexer_config::JobSpec =
        serde_json::from_value(job.spec).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let latest = control
        .list_job_versions(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .map(|row| row.version)
        .max()
        .unwrap_or(0);
    let next = body.version.map_or(latest + 1, |v| v as i32).max(1);
    control
        .insert_job_version(
            &state.chain_id,
            &name,
            next,
            &spec.spec_hash(),
            body.from.unwrap_or(0) as i64,
            None,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    control
        .set_version_status(&state.chain_id, &name, next, VersionStatus::Scanning, None)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Json(serde_json::json!({
        "name": name,
        "version": next,
        "from": body.from.unwrap_or(0),
    })))
}

/// Retire a job's active version.
pub async fn retire_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_admin(&headers)?;
    let control = state.storage.postgres().clone();
    let versions = control
        .list_job_versions(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(active) = versions.into_iter().find(|row| row.status == "active") else {
        return Err(StatusCode::NOT_FOUND);
    };
    control
        .set_version_status(
            &state.chain_id,
            &name,
            active.version,
            VersionStatus::Retired,
            None,
        )
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Json(
        serde_json::json!({ "name": name, "retired": active.version }),
    ))
}

/// Dry run: DDL plus estimated scan size, no writes.
pub async fn plan_job(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    Query(query): Query<PlanQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let control = state.storage.postgres().clone();
    let Some(job) = control
        .get_job(&state.chain_id, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let spec: sui_indexer_config::JobSpec =
        serde_json::from_value(job.spec).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let tip = match query.tip {
        Some(tip) => tip,
        None => control
            .get_cursor(&state.chain_id, &name, spec.version as i32)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_or(spec.scan.from, |cursor| cursor.tip.max(0) as u64),
    };
    let decision = job_engine::decide_apply(&spec, None).map_err(|_| StatusCode::BAD_REQUEST)?;
    let plan = job_engine::plan_job(&spec, decision, tip).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({
        "name": plan.name,
        "version": plan.version,
        "ddl": plan.ddl,
        "estimated_heights": plan.estimated_heights,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiState, api_state};

    #[test]
    fn admin_gate_rejects_missing_header() {
        assert_eq!(
            // No runtime needed: the gate is pure header logic, exercised
            // through a synchronous shim.
            require_admin(&HeaderMap::new()),
            Err(StatusCode::FORBIDDEN)
        );
        let mut headers = HeaderMap::new();
        headers.insert(ADMIN_HEADER, "1".parse().expect("header value"));
        assert_eq!(require_admin(&headers), Ok(()));
        let mut wrong = HeaderMap::new();
        wrong.insert(ADMIN_HEADER, "0".parse().expect("header value"));
        assert_eq!(require_admin(&wrong), Err(StatusCode::FORBIDDEN));
    }

    async fn live_state(chain: &str) -> Option<(ApiState, String)> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let mut config = sui_indexer_config::IndexerConfig::default();
        config.chain.chain_id = chain.to_owned();
        let db = sui_indexer_config::DatabaseConfig {
            url,
            max_connections: 2,
            min_connections: 1,
            connect_timeout: 10,
            idle_timeout: None,
            auto_migrate: false,
        };
        let storage = sui_indexer_storage::StorageManager::new_postgres(db)
            .await
            .ok()?;
        storage.initialize().await.ok()?;
        let feed = std::sync::Arc::new(crate::BlockFeed::new(8));
        let state = api_state(&config, &storage, &feed);
        Some((state, chain.to_owned()))
    }

    fn admin_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ADMIN_HEADER, "1".parse().expect("header value"));
        headers
    }

    fn sql_spec(name: &str) -> sui_indexer_config::JobSpec {
        sui_indexer_config::JobSpec {
            name: name.to_owned(),
            output: sui_indexer_config::OutputConfig {
                table: format!("job_{name}"),
                ..Default::default()
            },
            sql:
                "SELECT height AS _height FROM chain_events WHERE height >= {lo} AND height < {hi}"
                    .to_owned(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn jobs_crud_flows() {
        let chain = format!("jobs-{}", std::process::id());
        let Some((state, _)) = live_state(&chain).await else {
            return;
        };
        let name = format!("job{}", std::process::id() % 100000);
        // Forbidden without the admin header.
        let denied = apply_job(
            State(state.clone()),
            HeaderMap::new(),
            Json(ApplyJobBody {
                spec: sql_spec(&name),
            }),
        )
        .await;
        assert_eq!(denied.unwrap_err(), StatusCode::FORBIDDEN);

        // Apply twice: idempotent, version 1.
        for _ in 0..2 {
            let applied = apply_job(
                State(state.clone()),
                admin_headers(),
                Json(ApplyJobBody {
                    spec: sql_spec(&name),
                }),
            )
            .await
            .expect("apply")
            .0;
            assert_eq!(applied["version"], serde_json::json!(1));
        }
        let listed = list_jobs(State(state.clone())).await.expect("list").0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, name);

        // Plan reports DDL without writing.
        let plan = plan_job(
            State(state.clone()),
            Path(name.clone()),
            Query(PlanQuery { tip: Some(100) }),
        )
        .await
        .expect("plan")
        .0;
        assert!(plan["ddl"].as_array().expect("ddl").len() == 1);

        // Invalid desired state is rejected; paused sticks.
        let bad = update_job(
            State(state.clone()),
            admin_headers(),
            Path(name.clone()),
            Json(DesiredBody {
                desired: "bogus".to_owned(),
            }),
        )
        .await;
        assert_eq!(bad.unwrap_err(), StatusCode::BAD_REQUEST);
        let paused = update_job(
            State(state.clone()),
            admin_headers(),
            Path(name.clone()),
            Json(DesiredBody {
                desired: "paused".to_owned(),
            }),
        )
        .await
        .expect("pause")
        .0;
        assert_eq!(paused["desired"], serde_json::json!("paused"));

        // Missing job 404s on update and plan.
        assert_eq!(
            update_job(
                State(state.clone()),
                admin_headers(),
                Path("missing".to_string()),
                Json(DesiredBody {
                    desired: "paused".to_owned()
                }),
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            plan_job(
                State(state.clone()),
                Path("missing".to_string()),
                Query(PlanQuery { tip: None })
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );

        // Delete is 204 and empties the list.
        assert_eq!(
            delete_job(State(state.clone()), admin_headers(), Path(name.clone()))
                .await
                .expect("delete"),
            StatusCode::NO_CONTENT
        );
        assert!(
            list_jobs(State(state.clone()))
                .await
                .expect("list")
                .0
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reapply_with_changed_logic_auto_bumps() {
        let chain = format!("jobs3-{}", std::process::id());
        let Some((state, _)) = live_state(&chain).await else {
            return;
        };
        let name = format!("autobump{}", std::process::id() % 100000);
        let applied = apply_job(
            State(state.clone()),
            admin_headers(),
            Json(ApplyJobBody {
                spec: sql_spec(&name),
            }),
        )
        .await
        .expect("apply")
        .0;
        assert_eq!(applied["version"], serde_json::json!(1));
        // Same version number, edited SQL: the apply bumps to v2 instead of
        // dropping the edit.
        let mut edited = sql_spec(&name);
        edited.sql = "SELECT height AS _height FROM chain_events WHERE height >= {lo} AND height < {hi} AND 1 = 1".to_owned();
        let bumped = apply_job(
            State(state.clone()),
            admin_headers(),
            Json(ApplyJobBody { spec: edited }),
        )
        .await
        .expect("apply")
        .0;
        assert_eq!(bumped["version"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn rescan_bumps_and_retire_closes() {
        let chain = format!("jobs2-{}", std::process::id());
        let Some((state, _)) = live_state(&chain).await else {
            return;
        };
        let name = format!("rescan{}", std::process::id() % 100000);
        let _ = apply_job(
            State(state.clone()),
            admin_headers(),
            Json(ApplyJobBody {
                spec: sql_spec(&name),
            }),
        )
        .await
        .expect("apply");
        // Promote to v2 and activate it so retire has a target.
        {
            use sui_indexer_storage::{JobControlPlane, VersionStatus};
            let control = state.storage.postgres().clone();
            control
                .insert_job_version(&chain, &name, 2, "hash2", 0, None)
                .await
                .expect("v2");
            for (version, status) in [(1, VersionStatus::Scanning), (1, VersionStatus::Failed)] {
                control
                    .set_version_status(&chain, &name, version, status, None)
                    .await
                    .expect("status");
            }
            for status in [
                VersionStatus::Scanning,
                VersionStatus::CatchingUp,
                VersionStatus::Active,
            ] {
                control
                    .set_version_status(&chain, &name, 2, status, None)
                    .await
                    .expect("status");
            }
        }
        // Rescan lands on v3 (latest + 1).
        let rescanned = rescan_job(
            State(state.clone()),
            admin_headers(),
            Path(name.clone()),
            Json(RescanBody {
                from: Some(0),
                version: None,
            }),
        )
        .await
        .expect("rescan")
        .0;
        assert_eq!(rescanned["version"], serde_json::json!(3));
        // Retire closes the active version (v2), not the older ones.
        let retired = retire_job(State(state.clone()), admin_headers(), Path(name.clone()))
            .await
            .expect("retire")
            .0;
        assert_eq!(retired["retired"], serde_json::json!(2));
        // Nothing active left: retire 404s.
        assert_eq!(
            retire_job(State(state.clone()), admin_headers(), Path(name.clone()))
                .await
                .unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            rescan_job(
                State(state.clone()),
                admin_headers(),
                Path("missing".to_string()),
                Json(RescanBody {
                    from: None,
                    version: None
                }),
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );
    }
}
