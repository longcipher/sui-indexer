//! Job runner: the reconciler loop plus per-version scan tasks.
//!
//! One loop owns desired state: load jobs (DB wins, config auto-applied on
//! boot), diff against running tasks, start/stop versions. SQL-tier versions
//! replay the archive in chunks with persisted cursors; WASM-tier versions
//! read archive windows through the rule host. Outputs are versioned tables
//! with an atomic alias swap on catch-up.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chain_core::ChainAdapter;
use job_engine::{
    DesiredState, JobMetrics, JobMetricsRegistry, ReconcileAction, Reconciler, RunningVersion,
    VersionExecutor,
};
use sui_indexer_config::{ClickHouseConfig, JobSpec, JobTier};
use sui_indexer_storage::{JobControlPlane, StorageManager, VersionStatus};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How often the runner re-reads desired state.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// Job runner: reconciler plus per-version scan tasks.
pub struct JobRunner {
    chain_id: String,
    adapter: Arc<dyn ChainAdapter>,
    storage: StorageManager,
    specs: Vec<JobSpec>,
    clickhouse: ClickHouseConfig,
    registry: Arc<JobMetricsRegistry>,
    reconciler: Reconciler,
    tasks: HashMap<RunningVersion, JoinHandle<()>>,
    metrics: HashMap<RunningVersion, Arc<JobMetrics>>,
    shutdown: Arc<AtomicBool>,
}

impl JobRunner {
    /// Create a runner. Config-file jobs are applied to the DB on [`Self::tick`].
    #[must_use]
    pub fn new(
        chain_id: String,
        adapter: Arc<dyn ChainAdapter>,
        storage: StorageManager,
        specs: Vec<JobSpec>,
        clickhouse: ClickHouseConfig,
        registry: Arc<JobMetricsRegistry>,
    ) -> Self {
        Self {
            chain_id,
            adapter,
            storage,
            specs,
            clickhouse,
            registry,
            reconciler: Reconciler::new(),
            tasks: HashMap::new(),
            metrics: HashMap::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal shutdown; running tasks are aborted.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for task in self.tasks.values() {
            task.abort();
        }
    }

    /// Number of tracked version tasks (observability + tests).
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.tasks.len()
    }

    /// Render per-version Prometheus lines for `/metrics`.
    #[must_use]
    pub fn metrics_text(&self) -> String {
        let mut out = String::new();
        for metrics in self.metrics.values() {
            out.push_str(&metrics.render());
        }
        out
    }

    /// One reconcile tick: apply config jobs, diff desired vs running,
    /// start/stop versions. A no-op job can be added and removed without a
    /// restart through this path.
    pub async fn tick(&mut self) -> eyre::Result<()> {
        let control = self.storage.postgres().clone();
        // Config-file jobs are auto-applied (idempotent upsert).
        for spec in &self.specs {
            if let Err(e) = apply_spec(&control, &self.chain_id, spec).await {
                warn!(job = %spec.name, "config job apply failed: {e:#}");
            }
        }
        let rows = control.list_jobs(&self.chain_id).await?;
        let mut specs_by_name: HashMap<&str, &JobSpec> = HashMap::new();
        for spec in &self.specs {
            specs_by_name.insert(spec.name.as_str(), spec);
        }
        // Desired versions come from the DB rows (latest version per job).
        // Config-file jobs are authoritative for desired state; API-created
        // jobs keep whatever the control plane holds.
        let mut desired_specs: Vec<JobSpec> = Vec::new();
        for row in &rows {
            if row.desired != "active" {
                continue;
            }
            let spec = if let Some(spec) = specs_by_name.get(row.name.as_str()) {
                (*spec).clone()
            } else if let Ok(spec) = serde_json::from_value::<JobSpec>(row.spec.clone()) {
                spec
            } else {
                continue;
            };
            // Settled versions keep serving: a version that is already active
            // with the same logic is never restarted.
            let versions = control.list_job_versions(&self.chain_id, &row.name).await?;
            let settled = versions
                .iter()
                .any(|v| v.status == "active" && v.spec_hash == spec.spec_hash());
            if !settled {
                desired_specs.push(spec);
            }
        }
        let desired = DesiredState::from_specs(&desired_specs);
        for action in self.reconciler.reconcile(&desired) {
            match action {
                ReconcileAction::Start { job, version } => {
                    self.start_version(&job, version).await;
                }
                ReconcileAction::Stop { job, version } => {
                    self.stop_version(&RunningVersion { job, version }).await;
                }
                ReconcileAction::Replace { job, old, new } => {
                    self.stop_version(&RunningVersion {
                        job: job.clone(),
                        version: old,
                    })
                    .await;
                    self.start_version(&job, new).await;
                }
            }
        }
        // Reap finished tasks (gauges keep their final values for dashboards).
        let done: Vec<RunningVersion> = self
            .tasks
            .iter()
            .filter(|(_, handle)| handle.is_finished())
            .map(|(version, _)| version.clone())
            .collect();
        for version in done {
            self.tasks.remove(&version);
            self.reconciler.mark_stopped(&version);
        }
        Ok(())
    }

    /// Run until shutdown.
    pub async fn run(mut self) -> eyre::Result<()> {
        info!(chain = %self.chain_id, "job runner started");
        while !self.shutdown.load(Ordering::Relaxed) {
            if let Err(e) = self.tick().await {
                warn!("job reconcile tick failed: {e:#}");
            }
            tokio::time::sleep(RECONCILE_INTERVAL).await;
        }
        Ok(())
    }

    async fn start_version(&mut self, job: &str, version: u32) {
        let key = RunningVersion {
            job: job.to_owned(),
            version,
        };
        if self.tasks.contains_key(&key) {
            return;
        }
        let metrics = Arc::new(JobMetrics::labelled(&self.chain_id, job, version));
        self.metrics.insert(key.clone(), metrics.clone());
        self.registry.register(
            job_engine::registry_key(&self.chain_id, job, version),
            metrics.clone(),
        );
        let runner = VersionTask {
            chain_id: self.chain_id.clone(),
            adapter: self.adapter.clone(),
            storage: self.storage.clone(),
            clickhouse: self.clickhouse.clone(),
            job: job.to_owned(),
            version,
            metrics,
            shutdown: self.shutdown.clone(),
        };
        info!(job, version, "spawning version scan");
        let job_name = job.to_owned();
        self.tasks.insert(
            key,
            tokio::spawn(async move {
                if let Err(e) = runner.run().await {
                    warn!(job = %job_name, version, "version scan failed: {e:#}");
                }
            }),
        );
    }

    async fn stop_version(&mut self, version: &RunningVersion) {
        if let Some(handle) = self.tasks.remove(version) {
            handle.abort();
            info!(job = %version.job, version = version.version, "version task aborted");
        }
        self.metrics.remove(version);
        self.registry.unregister(&job_engine::registry_key(
            &self.chain_id,
            &version.job,
            version.version,
        ));
        self.reconciler.mark_stopped(version);
    }
}

/// Apply one spec to the control plane: validate, hash, insert/update.
async fn apply_spec(
    control: &Arc<sui_indexer_storage::PostgresStorage>,
    chain_id: &str,
    spec: &JobSpec,
) -> eyre::Result<()> {
    spec.validate()
        .map_err(|reason| eyre::eyre!("invalid job {}: {reason}", spec.name))?;
    let spec_json = serde_json::to_value(spec)?;
    let hash = spec.spec_hash();
    control
        .upsert_job(chain_id, &spec.name, &spec_json, &hash, &spec.desired)
        .await?;
    // A changed spec auto-bumps the version, mirroring the API path.
    let existing = control.get_job(chain_id, &spec.name).await?;
    let versions = control.list_job_versions(chain_id, &spec.name).await?;
    let latest_version = versions.iter().map(|row| row.version).max();
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
    let decision = job_engine::decide_apply(spec, latest_info).map_err(|e| eyre::eyre!("{e}"))?;
    control
        .insert_job_version(
            chain_id,
            &spec.name,
            decision.target_version() as i32,
            &hash,
            spec.scan.from as i64,
            job_engine::parse_scan_to(&spec.scan.to, u64::MAX).map(|v| v as i64),
        )
        .await?;
    Ok(())
}

/// One version's scan task.
struct VersionTask {
    chain_id: String,
    adapter: Arc<dyn ChainAdapter>,
    storage: StorageManager,
    clickhouse: ClickHouseConfig,
    job: String,
    version: u32,
    metrics: Arc<JobMetrics>,
    shutdown: Arc<AtomicBool>,
}

impl VersionTask {
    async fn run(&self) -> eyre::Result<()> {
        let control = self.storage.postgres().clone();
        let result = self.run_inner(control.clone()).await;
        // Failures are visible in the control plane (quarantine signal for
        // the operator runbook) instead of stalling silently at `scanning`.
        if let Err(error) = &result {
            let _ = control
                .set_version_status(
                    &self.chain_id,
                    &self.job,
                    self.version as i32,
                    VersionStatus::Failed,
                    Some(&error.to_string()),
                )
                .await;
        }
        result
    }

    async fn run_inner(
        &self,
        control: std::sync::Arc<sui_indexer_storage::PostgresStorage>,
    ) -> eyre::Result<()> {
        let Some(job_row) = control.get_job(&self.chain_id, &self.job).await? else {
            return Ok(());
        };
        let spec: JobSpec = serde_json::from_value(job_row.spec.clone())?;
        control
            .set_version_status(
                &self.chain_id,
                &self.job,
                self.version as i32,
                VersionStatus::Scanning,
                None,
            )
            .await?;

        let tip = self
            .adapter
            .head()
            .await
            .map_err(|e| eyre::eyre!("tip read failed: {e}"))?;
        let to = job_engine::parse_scan_to(&spec.scan.to, tip).unwrap_or(tip);
        // Resume after the last *completed* height; the cursor starts one
        // below `scan.from` so a fresh version scans everything. Stays in
        // i64: a naive `as u64` cast would wrap -1 into u64::MAX.
        let from = spec.scan.from.max(
            control
                .get_cursor(&self.chain_id, &self.job, self.version as i32)
                .await?
                .map(|c| c.cursor.saturating_add(1).max(0) as u64)
                .unwrap_or(spec.scan.from),
        );
        self.metrics.set_scan_height(from);
        self.metrics.set_lag(tip.saturating_sub(from));

        let physical = job_engine::physical_table(&spec.output.table, self.version)
            .map_err(|e| eyre::eyre!("{e}"))?;
        self.ensure_output(&spec, &physical).await?;
        // Archive tier converges behind PG: DDL is ensured here, chunks
        // replay inside ClickHouse (path A) best-effort per chunk.
        let ch_executor = self.ensure_archive_output(&spec, &physical).await?;

        match spec.tier {
            JobTier::Sql => {
                self.run_sql(&spec, &physical, from, to, ch_executor.as_ref())
                    .await?;
            }
            JobTier::Wasm => {
                self.run_wasm(&spec, &physical, from, to, ch_executor.as_ref())
                    .await?;
            }
        }

        // Catch-up: alias swap, then retire the previous active version.
        // The lifecycle moves through `catching_up` even when the scan was
        // empty: readers must never observe a version that skipped it.
        // A failed tip read fails here too (see above): activating against
        // an unknown tip would publish an empty alias as caught up.
        let tip = self
            .adapter
            .head()
            .await
            .map_err(|e| eyre::eyre!("tip read failed: {e}"))?;
        if to >= tip {
            let alias = job_engine::alias_swap_ddl(&spec.output.table, self.version)
                .map_err(|e| eyre::eyre!("{e}"))?;
            sqlx::query(sqlx::AssertSqlSafe(alias))
                .execute(control.pool())
                .await?;
            control
                .set_version_status(
                    &self.chain_id,
                    &self.job,
                    self.version as i32,
                    VersionStatus::CatchingUp,
                    None,
                )
                .await?;
            control
                .set_version_status(
                    &self.chain_id,
                    &self.job,
                    self.version as i32,
                    VersionStatus::Active,
                    None,
                )
                .await?;
            for row in control.list_job_versions(&self.chain_id, &self.job).await? {
                if row.version != self.version as i32 && row.status == "active" {
                    control
                        .set_version_status(
                            &self.chain_id,
                            &self.job,
                            row.version,
                            VersionStatus::Retired,
                            None,
                        )
                        .await?;
                }
            }
            info!(job = %self.job, version = self.version, "alias swapped, version active");
        } else {
            control
                .set_version_status(
                    &self.chain_id,
                    &self.job,
                    self.version as i32,
                    VersionStatus::CatchingUp,
                    None,
                )
                .await?;
        }
        Ok(())
    }

    /// Create the versioned physical table (SQL tier derives the schema from
    /// the user query; WASM tier uses the canonical output shape).
    async fn ensure_output(&self, spec: &JobSpec, physical: &str) -> eyre::Result<()> {
        let control = self.storage.postgres().clone();
        let ddl = match spec.tier {
            JobTier::Sql => {
                // Bind placeholders so schema derivation executes nothing
                // but parses: `LIMIT 0` over an empty range.
                let bound = job_engine::bind_range(&spec.sql, 0, 0);
                let stmt = job_engine::build_create_table_as(physical, &bound)
                    .map_err(|e| eyre::eyre!("{e}"))?;
                stmt.sql
            }
            JobTier::Wasm => format!(
                "CREATE TABLE IF NOT EXISTS {physical} (\
                 chain TEXT, _height BIGINT, _rule_version INT, \
                 _commitment SMALLINT, data JSONB, data_hash TEXT, \
                 UNIQUE (chain, _height, _rule_version, _commitment, data_hash))"
            ),
        };
        // Audited: `physical` passed the identifier allow-list inside the
        // builders; WASM DDL interpolates only that name.
        job_engine::validate_identifier(&spec.output.table).map_err(|e| eyre::eyre!("{e}"))?;
        sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(control.pool())
            .await?;
        // Register the stable alias in the dynamic catalog so the new table
        // is queryable through `/query` with no code change.
        let alias_ddl = job_engine::alias_swap_ddl(&spec.output.table, self.version)
            .map_err(|e| eyre::eyre!("{e}"))?;
        control
            .upsert_catalog_object(&sui_indexer_storage::CatalogObjectRow {
                chain_id: self.chain_id.clone(),
                name: spec.output.table.clone(),
                kind: "view".to_owned(),
                ddl: alias_ddl.clone(),
                select_sql: None,
                checksum: job_engine::checksum(&alias_ddl),
                public: true,
                block_column: Some("_height".to_owned()),
                reorg_mode: spec.output.reorg_mode.clone(),
                owner_job: Some(self.job.clone()),
                backfill: "ranged".to_owned(),
            })
            .await?;
        Ok(())
    }

    /// Ensure the archive-side versioned table when the ClickHouse tier is
    /// enabled. Degraded mode (unreachable archive) warns and continues
    /// PG-only; the archive converges on re-scan.
    async fn ensure_archive_output(
        &self,
        spec: &JobSpec,
        physical: &str,
    ) -> eyre::Result<Option<archive_store::ClickHouseExecutor>> {
        if !self.clickhouse.enabled {
            return Ok(None);
        }
        let client = match archive_store::ClickHouseClient::from_config(&self.clickhouse) {
            Ok(client) => client,
            Err(e) => {
                warn!(job = %self.job, "archive disabled for this run: {e:#}");
                self.metrics.add_failure(false);
                return Ok(None);
            }
        };
        let db = self.clickhouse.analytics_database.clone();
        let ddl = match spec.tier {
            JobTier::Sql => {
                if let Err(e) = job_engine::ddl::validate_select_sql(&spec.sql) {
                    warn!(job = %self.job, "archive skipped (SQL): {e}");
                    return Ok(None);
                }
                archive_store::job_table_as_select_ddl(
                    &db,
                    physical,
                    &spec.output.order_by,
                    &spec.output.partition_by,
                    &spec.sql,
                )
            }
            JobTier::Wasm => {
                archive_store::job_output_ddl(&db, physical, &[], &spec.output.partition_by, None)
            }
        };
        if let Err(e) = client.query(&ddl).await {
            warn!(job = %self.job, "archive DDL failed, continuing PG-only: {e:#}");
            self.metrics.add_failure(false);
            return Ok(None);
        }
        Ok(Some(archive_store::ClickHouseExecutor::new(
            client,
            db,
            physical.to_owned(),
            spec.sql.clone(),
        )))
    }

    async fn run_sql(
        &self,
        spec: &JobSpec,
        physical: &str,
        from: u64,
        to: u64,
        ch: Option<&archive_store::ClickHouseExecutor>,
    ) -> eyre::Result<()> {
        let executor = job_engine::SqlExecutor::new(
            self.chain_id.clone(),
            spec,
            self.version,
            physical.to_owned(),
            self.storage.postgres().clone(),
        )
        .map_err(|e| eyre::eyre!("{e}"))?;
        let mut chunks = job_engine::plan_chunks(from, to, spec.scan.chunk.max(1));
        // Freshness beats backfill: newest-first is already the plan order.
        let _ = &mut chunks;
        for chunk in chunks {
            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }
            match executor.run_chunk(chunk).await {
                Ok(outcome) => {
                    self.metrics.add_rows(outcome.rows_written);
                    self.metrics.set_scan_height(chunk.hi.saturating_sub(1));
                    // Archive converges behind PG; a failed archive chunk
                    // never blocks the cursor (it heals on re-scan).
                    if let Some(ch) = ch {
                        if let Err(e) = ch.run_chunk(chunk).await {
                            warn!(job = %self.job, "archive chunk failed: {e:#}");
                            self.metrics.add_failure(false);
                        }
                    }
                }
                Err(e) => {
                    self.metrics.add_failure(false);
                    // Split-on-failure: halve the chunk and retry the halves.
                    let mid = chunk.lo + (chunk.hi - chunk.lo) / 2;
                    if mid > chunk.lo && mid < chunk.hi {
                        warn!(job = %self.job, "splitting failed chunk [{}, {})", chunk.lo, chunk.hi);
                        for half in [
                            job_engine::ScanChunk {
                                lo: chunk.lo,
                                hi: mid,
                            },
                            job_engine::ScanChunk {
                                lo: mid,
                                hi: chunk.hi,
                            },
                        ] {
                            let outcome = executor.run_chunk(half).await.map_err(|e| {
                                self.metrics.add_failure(true);
                                eyre::eyre!("{e}")
                            })?;
                            self.metrics.add_rows(outcome.rows_written);
                        }
                    } else {
                        self.metrics.add_failure(true);
                        return Err(eyre::eyre!("{e}"));
                    }
                }
            }
        }
        Ok(())
    }

    async fn run_wasm(
        &self,
        spec: &JobSpec,
        physical: &str,
        from: u64,
        to: u64,
        ch: Option<&archive_store::ClickHouseExecutor>,
    ) -> eyre::Result<()> {
        let bytes = tokio::fs::read(&spec.module)
            .await
            .map_err(|e| eyre::eyre!("read wasm {}: {e}", spec.module))?;
        let rule_spec = rule_host::RuleSpec {
            name: spec.name.clone(),
            version: self.version,
            abi_version: rule_host::ABI_VERSION,
        };
        let host = rule_host::RuleHost::load(
            &bytes,
            rule_spec,
            spec.runtime.fuel,
            spec.runtime.max_memory_mb,
        )
        .map_err(|e| eyre::eyre!("{e}"))?;
        let pool = self.storage.postgres().pool().clone();
        for chunk in job_engine::plan_chunks(from, to, spec.scan.chunk.max(1)) {
            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }
            let events = load_window(&pool, &self.chain_id, chunk.lo, chunk.hi).await?;
            let input = rule_host::WindowInput {
                abi_version: rule_host::ABI_VERSION,
                events,
                blocks: Vec::new(),
                feeds: Vec::new(),
                window_lo: chunk.lo,
                window_hi: chunk.hi,
            };
            let rows = host.on_window(&input).await.map_err(|e| {
                self.metrics.add_failure(true);
                eyre::eyre!("{e}")
            })?;
            if rows.len() as u64 > spec.runtime.max_rows_per_window {
                self.metrics.add_failure(true);
                return Err(eyre::eyre!("wasm rule exceeded row budget"));
            }
            write_out_rows(&pool, physical, &self.chain_id, &rows).await?;
            // Archive converges behind PG; failures never block the cursor.
            if let Some(ch) = ch {
                let payload: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|row| {
                        serde_json::json!({
                            "chain": self.chain_id,
                            "_height": row.height,
                            "_rule_version": row.rule_version,
                            "_commitment": row.commitment,
                            "data": row.values.to_string(),
                        })
                    })
                    .collect();
                if let Err(e) = ch.write_rows(&payload).await {
                    warn!(job = %self.job, "archive write failed: {e:#}");
                    self.metrics.add_failure(false);
                }
            }
            self.storage
                .postgres()
                .advance_version_cursor(
                    &self.chain_id,
                    &self.job,
                    self.version as i32,
                    chunk.hi.saturating_sub(1) as i64,
                    rows.len() as i64,
                )
                .await?;
            self.metrics.add_rows(rows.len() as u64);
            self.metrics.set_scan_height(chunk.hi.saturating_sub(1));
        }
        Ok(())
    }
}

/// Load one archive window from the skeleton `chain_events` table.
async fn load_window(
    pool: &sqlx::PgPool,
    chain_id: &str,
    lo: u64,
    hi: u64,
) -> eyre::Result<Vec<chain_core::Ev>> {
    let rows = sqlx::query(
        "SELECT height, block_ts, tx_index, ev_index, inner_ix, stack_height,
                emitter, topics, payload, tx_hash, sender, extra
         FROM chain_events
         WHERE chain_id = $1 AND height >= $2 AND height < $3
         ORDER BY height, tx_index, ev_index, inner_ix",
    )
    .bind(chain_id)
    .bind(lo as i64)
    .bind(hi as i64)
    .fetch_all(pool)
    .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        use sqlx::Row as _;
        events.push(chain_core::Ev {
            height: row.try_get::<i64, _>("height")? as u64,
            block_ts: row.try_get("block_ts")?,
            tx_index: row.try_get::<i32, _>("tx_index")? as u32,
            ev_index: row.try_get::<i32, _>("ev_index")? as u32,
            inner_ix: row.try_get::<i32, _>("inner_ix")? as u32,
            stack_height: row.try_get::<i32, _>("stack_height")? as u32,
            emitter: row.try_get("emitter")?,
            topics: row.try_get("topics")?,
            payload: row.try_get("payload")?,
            tx_hash: row.try_get("tx_hash")?,
            sender: row.try_get("sender")?,
            extra: row.try_get("extra")?,
        });
    }
    Ok(events)
}

/// Write WASM output rows to the versioned physical table.
async fn write_out_rows(
    pool: &sqlx::PgPool,
    physical: &str,
    chain_id: &str,
    rows: &[chain_core::OutRow],
) -> eyre::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    // Strip the trailing `__vN` version suffix before validating the alias.
    let alias = physical
        .rsplit_once("__v")
        .map_or(physical, |(alias, _)| alias);
    job_engine::validate_identifier(alias).map_err(|e| eyre::eyre!("{e}"))?;
    let mut builder = sqlx::QueryBuilder::new(format!(
        "INSERT INTO {physical} (chain, _height, _rule_version, _commitment, data, data_hash) "
    ));
    builder.push_values(rows.iter(), |mut b, row| {
        b.push_bind(chain_id)
            .push_bind(row.height as i64)
            .push_bind(row.rule_version)
            .push_bind(row.commitment)
            .push_bind(row.values.clone())
            .push_bind(job_engine::checksum(&row.values.to_string()));
    });
    builder.push(" ON CONFLICT (chain, _height, _rule_version, _commitment, data_hash) DO NOTHING");
    // Audited: `physical` is `alias__vN` with an allow-listed alias.
    builder.build().execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;
    use std::sync::Arc;

    struct StubAdapter {
        tip: u64,
    }

    #[async_trait::async_trait]
    impl ChainAdapter for StubAdapter {
        fn kind(&self) -> chain_core::ChainKind {
            chain_core::ChainKind::Move
        }

        fn commitment(&self) -> chain_core::CommitmentModel {
            chain_core::CommitmentModel::Final
        }

        fn schema(&self) -> &chain_core::ChainSchema {
            static SCHEMA: std::sync::OnceLock<chain_core::ChainSchema> =
                std::sync::OnceLock::new();
            SCHEMA.get_or_init(chain_core::ChainSchema::default)
        }

        async fn head(&self) -> Result<u64, chain_core::ChainError> {
            Ok(self.tip)
        }

        async fn fetch(
            &self,
            range: Range<u64>,
        ) -> Result<Vec<chain_core::DecodedBlock>, chain_core::ChainError> {
            Ok(range.map(chain_core::DecodedBlock::skipped).collect())
        }
    }

    fn stub(tip: u64) -> Arc<dyn ChainAdapter> {
        Arc::new(StubAdapter { tip })
    }

    /// Adapter whose tip never arrives: tasks stay in flight.
    struct SleepyStub;

    #[async_trait::async_trait]
    impl ChainAdapter for SleepyStub {
        fn kind(&self) -> chain_core::ChainKind {
            chain_core::ChainKind::Move
        }

        fn commitment(&self) -> chain_core::CommitmentModel {
            chain_core::CommitmentModel::Final
        }

        fn schema(&self) -> &chain_core::ChainSchema {
            static SCHEMA: std::sync::OnceLock<chain_core::ChainSchema> =
                std::sync::OnceLock::new();
            SCHEMA.get_or_init(chain_core::ChainSchema::default)
        }

        async fn head(&self) -> Result<u64, chain_core::ChainError> {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok(0)
        }

        async fn fetch(
            &self,
            range: Range<u64>,
        ) -> Result<Vec<chain_core::DecodedBlock>, chain_core::ChainError> {
            Ok(range.map(chain_core::DecodedBlock::skipped).collect())
        }
    }

    async fn live_storage() -> Option<StorageManager> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let db = sui_indexer_config::DatabaseConfig {
            url,
            max_connections: 4,
            min_connections: 1,
            connect_timeout: 10,
            idle_timeout: None,
            auto_migrate: false,
        };
        let storage = StorageManager::new_postgres(db).await.ok()?;
        storage.initialize().await.ok()?;
        Some(storage)
    }

    fn chain() -> String {
        format!("jobs-{}-{}", std::process::id(), next_id())
    }

    /// Unique job/table name namespaced by process so reruns never collide.
    /// Underscores only: the name becomes a SQL identifier.
    fn job_name(prefix: &str) -> String {
        format!("{}_{}_{}", prefix, std::process::id(), next_id())
    }

    fn next_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    fn sql_spec(name: &str, table: &str, chain_filter: Option<&str>) -> JobSpec {
        let scope = chain_filter.map_or_else(String::new, |c| format!("chain_id = '{c}' AND "));
        JobSpec {
            name: name.to_owned(),
            output: sui_indexer_config::OutputConfig {
                table: table.to_owned(),
                ..Default::default()
            },
            sql: format!(
                "SELECT height AS _height, 1 AS _rule_version, 1 AS _commitment \
                 FROM chain_events WHERE {scope}height >= {{lo}} AND height < {{hi}}"
            ),
            ..Default::default()
        }
    }

    async fn wait_for_status(
        storage: &StorageManager,
        chain: &str,
        job: &str,
        version: i32,
        want: &str,
    ) {
        use sui_indexer_storage::JobControlPlane as _;
        let mut last = String::new();
        for _ in 0..80 {
            let row = storage
                .postgres()
                .get_job_version(chain, job, version)
                .await
                .expect("version");
            let current = row
                .as_ref()
                .map(|row| row.status.clone())
                .unwrap_or_default();
            let error = row.and_then(|row| row.last_error).unwrap_or_default();
            if current == want {
                return;
            }
            last = format!("status={current} error={error}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("version {job} v{version} never reached {want} ({last})");
    }

    /// Best-effort cleanup of versioned tables and alias views so repeated
    /// runs do not accumulate schema objects.
    async fn cleanup_job(storage: &StorageManager, table: &str) {
        for object in [
            format!("{table}__v1"),
            format!("{table}__v2"),
            table.to_owned(),
        ] {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP TABLE IF EXISTS {object}"
            )))
            .execute(storage.postgres().pool())
            .await;
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("DROP VIEW IF EXISTS {object}")))
                .execute(storage.postgres().pool())
                .await;
        }
    }

    #[tokio::test]
    async fn stop_and_shutdown_remove_tasks() {
        use sui_indexer_storage::JobControlPlane as _;
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("stop");
        let table = job_name("job_stop");
        let spec = sql_spec(&job, &table, None);
        // API-created job (no config entry): the control plane owns desired
        // state, so a DB pause sticks instead of being reverted by config.
        apply_spec(storage.postgres(), &chain, &spec)
            .await
            .expect("apply");
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        assert_eq!(runner.running_count(), 1);
        // Pausing in the DB stops the version on the next tick (synchronous).
        let control = storage.postgres().clone();
        let row = control
            .get_job(&chain, &job)
            .await
            .expect("job")
            .expect("row");
        control
            .upsert_job(&chain, &job, &row.spec, &row.spec_hash, "paused")
            .await
            .expect("pause");
        runner.tick().await.expect("tick");
        assert_eq!(runner.running_count(), 0);

        // Shutdown aborts in-flight tasks; the next tick reaps them. The
        // sleepy tip keeps the task blocked in `head()` deterministically.
        let job2 = job_name("stopinner");
        let table2 = job_name("job_stopinner");
        let spec_inner = sql_spec(&job2, &table2, None);
        let mut runner2 = JobRunner::new(
            chain.clone(),
            Arc::new(SleepyStub),
            storage.clone(),
            vec![spec_inner],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner2.tick().await.expect("tick");
        assert_eq!(runner2.running_count(), 1);
        runner2.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        runner2.tick().await.expect("tick");
        assert_eq!(runner2.running_count(), 0);
        cleanup_job(&storage, &table).await;
        cleanup_job(&storage, &table2).await;
    }

    #[tokio::test]
    async fn run_returns_once_shut_down() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let runner = JobRunner::new(
            chain(),
            stub(0),
            storage,
            vec![],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), runner.run())
            .await
            .expect("returns promptly")
            .expect("ok");
    }

    #[tokio::test]
    async fn run_ticks_until_shutdown() {
        use sui_indexer_storage::JobControlPlane as _;
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("runloop");
        let table = job_name("job_runloop");
        let runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![sql_spec(&job, &table, None)],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        let handle = tokio::spawn(runner.run());
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        // At least one tick applied the spec even before shutdown.
        assert!(
            storage
                .postgres()
                .get_job(&chain, &job)
                .await
                .expect("job")
                .is_some()
        );
        // Shutdown is observed through the shared flag by the (moved) runner:
        // abort the loop task; the tick already proved the loop runs.
        handle.abort();
    }

    /// Adapter whose tip read always fails: versions must fail visibly
    /// instead of activating an empty alias.
    struct FailingTip;

    #[async_trait::async_trait]
    impl ChainAdapter for FailingTip {
        fn kind(&self) -> chain_core::ChainKind {
            chain_core::ChainKind::Move
        }

        fn commitment(&self) -> chain_core::CommitmentModel {
            chain_core::CommitmentModel::Final
        }

        fn schema(&self) -> &chain_core::ChainSchema {
            static SCHEMA: std::sync::OnceLock<chain_core::ChainSchema> =
                std::sync::OnceLock::new();
            SCHEMA.get_or_init(chain_core::ChainSchema::default)
        }

        async fn head(&self) -> Result<u64, chain_core::ChainError> {
            Err(chain_core::ChainError::Transport("down".to_owned()))
        }

        async fn fetch(
            &self,
            range: Range<u64>,
        ) -> Result<Vec<chain_core::DecodedBlock>, chain_core::ChainError> {
            Ok(range.map(chain_core::DecodedBlock::skipped).collect())
        }
    }

    #[tokio::test]
    async fn tip_outage_fails_instead_of_activating_empty() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("tipdown");
        let table = job_name("job_tipdown");
        let spec = sql_spec(&job, &table, None);
        let mut runner = JobRunner::new(
            chain.clone(),
            Arc::new(FailingTip),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        wait_for_status(&storage, &chain, &job, 1, "failed").await;
    }

    #[tokio::test]
    async fn version_task_surfaces_errors_directly() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("direct");
        let table = job_name("job_direct");
        let spec = sql_spec(&job, &table, None);
        apply_spec(storage.postgres(), &chain, &spec)
            .await
            .expect("apply");
        let task = VersionTask {
            chain_id: chain.clone(),
            adapter: stub(0),
            storage: storage.clone(),
            clickhouse: sui_indexer_config::ClickHouseConfig::default(),
            job: job.clone(),
            version: 1,
            metrics: Arc::new(JobMetrics::labelled(&chain, &job, 1)),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        task.run().await.expect("run");
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn sql_version_scans_and_swaps_alias() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("tick");
        let table = job_name("job_tick");
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![sql_spec(&job, &table, None)],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        assert_eq!(runner.running_count(), 1);
        wait_for_status(&storage, &chain, &job, 1, "active").await;
        // Alias view exists and the catalog exposes it publicly.
        let alias: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(&table)
            .fetch_one(storage.postgres().pool())
            .await
            .expect("alias");
        assert_eq!(alias.unwrap_or_default(), table);
        let metrics = runner.metrics_text();
        assert!(metrics.contains(&format!("job=\"{job}\"")));
        assert!(metrics.contains(&format!(
            "indexer_job_failures_total{{chain=\"{chain}\",job=\"{job}\",version=\"1\"}} 0"
        )));
        cleanup_job(&storage, &table).await;
        // Second tick reaps the finished task; shutdown is quiet.
        runner.tick().await.expect("tick");
        assert_eq!(runner.running_count(), 0);
        runner.shutdown();
        runner.tick().await.expect("tick");
        assert_eq!(runner.running_count(), 0);
    }

    #[tokio::test]
    async fn bounded_scan_parks_at_catching_up() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("catch");
        let table = job_name("job_catch");
        let mut spec = sql_spec(&job, &table, None);
        spec.scan.to = "50".to_owned();
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(100),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        // Scan ends at 50 below the tip of 100: catching up, no alias swap.
        wait_for_status(&storage, &chain, &job, 1, "catching_up").await;
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn new_version_retires_the_previous_active() {
        use sui_indexer_storage::JobControlPlane as _;
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let job = job_name("sup");
        let table = job_name("job_sup");
        let control = storage.postgres().clone();
        let spec_json = serde_json::to_value(&sql_spec(&job, &table, None)).expect("json");
        let hash = "seed-hash";
        control
            .upsert_job(&chain, &job, &spec_json, hash, "active")
            .await
            .expect("job");
        control
            .insert_job_version(&chain, &job, 0, hash, 0, None)
            .await
            .expect("v0");
        control
            .set_version_status(&chain, &job, 0, VersionStatus::Scanning, None)
            .await
            .expect("v0 scanning");
        control
            .insert_job_version(&chain, &job, 1, hash, 0, None)
            .await
            .expect("v1");
        for status in [
            VersionStatus::Scanning,
            VersionStatus::CatchingUp,
            VersionStatus::Active,
        ] {
            control
                .set_version_status(&chain, &job, 1, status, None)
                .await
                .expect("v1 active");
        }
        // Runner owns v2: spec version 2 with the same shape.
        let mut spec2 = sql_spec(&job, &table, None);
        spec2.version = 2;
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![spec2],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        wait_for_status(&storage, &chain, &job, 2, "active").await;
        let versions = control
            .list_job_versions(&chain, &job)
            .await
            .expect("versions");
        let status_of = |v: i32| {
            versions
                .iter()
                .find(|row| row.version == v)
                .map(|row| row.status.as_str())
        };
        // v0 (scanning) is untouched; v1 (active) retired; v2 serves.
        assert_eq!(status_of(0), Some("scanning"));
        assert_eq!(status_of(1), Some("retired"));
        assert_eq!(status_of(2), Some("active"));
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn quota_split_recovers_chunks() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        // Two rows inside the first-processed chunk [91, 101), one on each
        // side of the split midpoint 96: the whole exceeds the row quota,
        // the halves fit, so the split recovers.
        for height in [91i64, 97] {
            sqlx::query(
                "INSERT INTO chain_events
                   (chain_id, height, block_ts, tx_index, ev_index, emitter)
                 VALUES ($1, $2, NOW(), 0, 0, 'e')
                 ON CONFLICT DO NOTHING",
            )
            .bind(&chain)
            .bind(height)
            .execute(storage.postgres().pool())
            .await
            .expect("seed");
        }
        let job = job_name("split");
        let table = job_name("job_split");
        let mut spec = sql_spec(&job, &table, Some(&chain));
        spec.scan.chunk = 10;
        spec.runtime.max_rows_per_window = 1;
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(100),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        wait_for_status(&storage, &chain, &job, 1, "active").await;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table}__v1"
        )))
        .fetch_one(storage.postgres().pool())
        .await
        .expect("count");
        assert_eq!(count, 2);
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn degenerate_split_never_commits_empty_ranges() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        // Bad SQL with single-height chunks: every chunk fails whole, and the
        // degenerate halves must not commit phantom cursors either.
        let job = job_name("narrow");
        let table = job_name("job_narrow");
        let mut spec = sql_spec(&job, &table, Some(&chain));
        spec.sql =
            "SELECT * FROM no_such_table_xyz WHERE height >= {lo} AND height < {hi}".to_owned();
        spec.scan.chunk = 1;
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(5),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        // The version fails instead of going active...
        wait_for_status(&storage, &chain, &job, 1, "failed").await;
        // ...and no phantom cursor was committed (fresh cursors start at -1).
        use sui_indexer_storage::JobControlPlane as _;
        let row = storage
            .postgres()
            .get_job_version(&chain, &job, 1)
            .await
            .expect("version")
            .expect("row");
        assert_ne!(row.status, "active");
        let cursor = storage
            .postgres()
            .get_cursor(&chain, &job, 1)
            .await
            .expect("cursor")
            .expect("cursor row");
        assert_eq!(cursor.cursor, -1);
        cleanup_job(&storage, &table).await;
    }

    // Static guest: ignores the window, returns one canned row. The packed
    // return is (58 << 32) | 1024: 58 bytes of JSON at offset 1024.
    const GUEST_WAT: &str = r#"
        (module
            (memory (export "memory") 1)
            (data (i32.const 1024) "[{\"height\":5,\"rule_version\":1,\"commitment\":1,\"values\":{}}]")
            (func (export "alloc") (param i32) (result i32)
                i32.const 2048)
            (func (export "on_window") (param i32 i32) (result i64)
                i64.const 249108104192))"#;

    fn wasm_spec(name: &str, table: &str, module: &str, max_rows: u64) -> JobSpec {
        JobSpec {
            name: name.to_owned(),
            tier: JobTier::Wasm,
            module: module.to_owned(),
            output: sui_indexer_config::OutputConfig {
                table: table.to_owned(),
                ..Default::default()
            },
            runtime: sui_indexer_config::RuntimeConfig {
                max_rows_per_window: max_rows,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn wasm_version_writes_rows_and_goes_active() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("wasm-{}", next_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let module = dir.join("rule.wasm");
        std::fs::write(&module, wat::parse_str(GUEST_WAT).expect("wat")).expect("write");
        let chain = chain();
        let job = job_name("wasm");
        let table = job_name("job_wasm");
        // The guest emits exactly 1 row against a quota of 1: at the
        // boundary, not over it.
        let spec = wasm_spec(&job, &table, &module.to_string_lossy(), 1);
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        wait_for_status(&storage, &chain, &job, 1, "active").await;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table}__v1"
        )))
        .fetch_one(storage.postgres().pool())
        .await
        .expect("count");
        assert_eq!(count, 1);
        std::fs::remove_dir_all(&dir).expect("cleanup");
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn wasm_row_budget_breach_fails_the_version() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("wasm-breach-{}", next_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let module = dir.join("rule.wasm");
        std::fs::write(&module, wat::parse_str(GUEST_WAT).expect("wat")).expect("write");
        let chain = chain();
        let job = job_name("wasmfail");
        let table = job_name("job_wasmfail");
        // The guest emits 1 row; the budget allows 0.
        let spec = wasm_spec(&job, &table, &module.to_string_lossy(), 0);
        let mut runner = JobRunner::new(
            chain.clone(),
            stub(0),
            storage.clone(),
            vec![spec],
            sui_indexer_config::ClickHouseConfig::default(),
            std::sync::Arc::new(job_engine::JobMetricsRegistry::new()),
        );
        runner.tick().await.expect("tick");
        // Quarantined: the budget breach fails the version instead of
        // activating it.
        wait_for_status(&storage, &chain, &job, 1, "failed").await;
        assert!(runner.metrics_text().contains("indexer_job_failures_total"));
        std::fs::remove_dir_all(&dir).expect("cleanup");
        cleanup_job(&storage, &table).await;
    }

    #[tokio::test]
    async fn window_load_preserves_order() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        for (tx, ev) in [(1i32, 1i32), (1, 0), (0, 0)] {
            sqlx::query(
                "INSERT INTO chain_events
                   (chain_id, height, block_ts, tx_index, ev_index, emitter)
                 VALUES ($1, 7, NOW(), $2, $3, 'e')
                 ON CONFLICT DO NOTHING",
            )
            .bind(&chain)
            .bind(tx)
            .bind(ev)
            .execute(storage.postgres().pool())
            .await
            .expect("seed");
        }
        let events = load_window(storage.postgres().pool(), &chain, 7, 8)
            .await
            .expect("window");
        assert_eq!(events.len(), 3);
        let keys: Vec<(u32, u32)> = events.iter().map(|e| (e.tx_index, e.ev_index)).collect();
        assert_eq!(keys, vec![(0, 0), (1, 0), (1, 1)]);
    }

    #[tokio::test]
    async fn out_rows_land_in_the_physical_table() {
        let Some(storage) = live_storage().await else {
            return;
        };
        let chain = chain();
        let physical = job_name("test_w") + "__v1";
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE {physical} (chain TEXT, _height BIGINT, _rule_version INT, \
             _commitment SMALLINT, data JSONB, data_hash TEXT, \
             UNIQUE (chain, _height, _rule_version, _commitment, data_hash))"
        )))
        .execute(storage.postgres().pool())
        .await
        .expect("ddl");
        write_out_rows(
            storage.postgres().pool(),
            &physical,
            &chain,
            &[chain_core::OutRow {
                height: 3,
                rule_version: 1,
                commitment: 1,
                values: serde_json::json!({ "profit": 5 }),
            }],
        )
        .await
        .expect("write");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {physical} WHERE _height = 3"
        )))
        .fetch_one(storage.postgres().pool())
        .await
        .expect("count");
        assert_eq!(count, 1);
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {physical}")))
            .execute(storage.postgres().pool())
            .await
            .expect("drop");
    }
}
