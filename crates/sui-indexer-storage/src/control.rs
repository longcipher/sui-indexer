//! Job-engine control plane: jobs, versions, cursors, catalog, feeds, queue.
//!
//! A job is data, not a process. The scheduler (reconciler) diffs desired
//! state (`jobs`) against running tasks and starts/stops/updates them with
//! cancellation. Every cursor lives here so a retry resumes, never restarts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One row of the `jobs` registry: desired state per (chain, job).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobRow {
    /// Stable chain key.
    pub chain_id: String,
    /// Job name.
    pub name: String,
    /// Canonical job spec JSON.
    pub spec: serde_json::Value,
    /// `spec + code` digest; a change triggers a re-scan.
    pub spec_hash: String,
    /// Desired state: `active` | `paused` | `retired`.
    pub desired: String,
    /// Last update.
    pub updated_at: Option<DateTime<Utc>>,
}

/// One row of `job_versions`: immutable per version.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobVersionRow {
    /// Stable chain key.
    pub chain_id: String,
    /// Job name.
    pub name: String,
    /// Output version.
    pub version: i32,
    /// Spec digest this version was built from.
    pub spec_hash: String,
    /// Lifecycle: `draft|scanning|catching_up|active|failed|retired|paused`.
    pub status: String,
    /// First height to scan.
    pub scan_from: i64,
    /// Last height to scan (NULL = head).
    pub scan_to: Option<i64>,
    /// Persisted scan cursor.
    pub scan_cursor: Option<i64>,
    /// Rows written by this version.
    pub rows_written: i64,
    /// Last error, if any.
    pub last_error: Option<String>,
    /// When the version started.
    pub started_at: Option<DateTime<Utc>>,
    /// When the version finished.
    pub finished_at: Option<DateTime<Utc>>,
}

/// One row of `job_cursors`: per-version scan progress.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobCursorRow {
    /// Stable chain key.
    pub chain_id: String,
    /// Job name.
    pub name: String,
    /// Output version.
    pub version: i32,
    /// Persisted cursor.
    pub cursor: i64,
    /// Observed tip when the cursor was written.
    pub tip: i64,
}

/// One row of `catalog_objects`: the dynamic catalog.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CatalogObjectRow {
    /// Stable chain key.
    pub chain_id: String,
    /// Object name (table / view / alias).
    pub name: String,
    /// `table` | `view` | `matview` | `refreshable_mv`.
    pub kind: String,
    /// DDL that built the object.
    pub ddl: String,
    /// Backfill select (ranged policy).
    pub select_sql: Option<String>,
    /// DDL checksum for drift detection.
    pub checksum: String,
    /// Visible to the public query surface.
    pub public: bool,
    /// Height column; non-null participates in reorg pruning.
    pub block_column: Option<String>,
    /// `block_scoped` | `refreshable` | `none`.
    pub reorg_mode: String,
    /// Owning job; cascade retirement.
    pub owner_job: Option<String>,
    /// `ranged` | `none` | `refresh`.
    pub backfill: String,
}

/// One row of `feeds`: external sources with cursor semantics.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FeedRow {
    /// Stable chain key.
    pub chain_id: String,
    /// Feed name.
    pub name: String,
    /// Feed kind.
    pub kind: String,
    /// Source settings.
    pub settings: serde_json::Value,
    /// Ingest cursor.
    pub cursor: i64,
}

/// One row of `work_queue`: a ranged unit of work.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WorkItemRow {
    /// Queue id.
    pub id: i64,
    /// Stable chain key.
    pub chain_id: String,
    /// Job name.
    pub job_name: String,
    /// Job version (0 = chain-level work such as repairs).
    pub job_version: i32,
    /// First height (inclusive).
    pub range_lo: i64,
    /// Last height (inclusive).
    pub range_hi: i64,
    /// Attempts so far.
    pub attempts: i32,
    /// Next eligible retry.
    pub next_retry_at: Option<DateTime<Utc>>,
    /// Last error, if any.
    pub last_error: Option<String>,
    /// Whether the item is done.
    pub done: bool,
}

/// Version lifecycle from the design state diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    /// DDL applied, rescan not yet started.
    Draft,
    /// Rescan running.
    Scanning,
    /// Cursor reached the plan boundary, waiting for the sync tip.
    CatchingUp,
    /// Alias swapped; serving reads.
    Active,
    /// Retry budget exhausted.
    Failed,
    /// Superseded by a newer version.
    Retired,
    /// Operator-paused.
    Paused,
}

impl VersionStatus {
    /// Parse a status string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "scanning" => Some(Self::Scanning),
            "catching_up" => Some(Self::CatchingUp),
            "active" => Some(Self::Active),
            "failed" => Some(Self::Failed),
            "retired" => Some(Self::Retired),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }

    /// Canonical string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scanning => "scanning",
            Self::CatchingUp => "catching_up",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Retired => "retired",
            Self::Paused => "paused",
        }
    }

    /// Whether `to` is reachable from `self` under the lifecycle diagram.
    #[must_use]
    pub fn can_transition_to(self, to: Self) -> bool {
        match (self, to) {
            (from, to) if from == to => true,
            (Self::Draft, Self::Scanning) => true,
            (Self::Scanning, Self::CatchingUp)
            | (Self::Scanning, Self::Failed)
            | (Self::Scanning, Self::Retired) => true,
            (Self::CatchingUp, Self::Active)
            | (Self::CatchingUp, Self::Scanning)
            | (Self::CatchingUp, Self::Failed) => true,
            (Self::Failed, Self::Scanning) | (Self::Failed, Self::Retired) => true,
            (Self::Active, Self::Retired) | (Self::Active, Self::Paused) => true,
            (Self::Paused, Self::Scanning) | (Self::Paused, Self::Retired) => true,
            _ => false,
        }
    }
}

/// Control-plane access for the job engine.
///
/// Implemented for [`crate::PostgresStorage`]; the job engine holds the
/// concrete handle so sync and jobs share one pool.
#[async_trait::async_trait]
pub trait JobControlPlane: Send + Sync {
    /// Insert or update a job's desired state.
    async fn upsert_job(
        &self,
        chain_id: &str,
        name: &str,
        spec: &serde_json::Value,
        spec_hash: &str,
        desired: &str,
    ) -> eyre::Result<()>;

    /// Fetch one job.
    async fn get_job(&self, chain_id: &str, name: &str) -> eyre::Result<Option<JobRow>>;

    /// List all jobs for a chain.
    async fn list_jobs(&self, chain_id: &str) -> eyre::Result<Vec<JobRow>>;

    /// Remove a job and all its versions, cursors and owned catalog objects.
    async fn delete_job(&self, chain_id: &str, name: &str) -> eyre::Result<()>;

    /// Insert a job version (no-op when it already exists).
    async fn insert_job_version(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        spec_hash: &str,
        scan_from: i64,
        scan_to: Option<i64>,
    ) -> eyre::Result<()>;

    /// Fetch one job version.
    async fn get_job_version(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
    ) -> eyre::Result<Option<JobVersionRow>>;

    /// List all versions of a job, newest first.
    async fn list_job_versions(
        &self,
        chain_id: &str,
        name: &str,
    ) -> eyre::Result<Vec<JobVersionRow>>;

    /// Move a version along the lifecycle (validated with
    /// [`VersionStatus::can_transition_to`]).
    async fn set_version_status(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        status: VersionStatus,
        error: Option<&str>,
    ) -> eyre::Result<()>;

    /// Advance a version cursor and accumulate rows written.
    async fn advance_version_cursor(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        cursor: i64,
        rows_added: i64,
    ) -> eyre::Result<()>;

    /// Fetch a version cursor.
    async fn get_cursor(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
    ) -> eyre::Result<Option<JobCursorRow>>;

    /// Register or refresh a catalog object (checksum drift = re-apply).
    async fn upsert_catalog_object(&self, object: &CatalogObjectRow) -> eyre::Result<()>;

    /// Public objects visible to the query surface.
    async fn list_public_catalog(&self, chain_id: &str) -> eyre::Result<Vec<CatalogObjectRow>>;

    /// Remove a catalog object.
    async fn delete_catalog_object(&self, chain_id: &str, name: &str) -> eyre::Result<()>;

    /// Enqueue one ranged work item.
    async fn enqueue_work(
        &self,
        chain_id: &str,
        job_name: &str,
        job_version: i32,
        range_lo: i64,
        range_hi: i64,
    ) -> eyre::Result<()>;

    /// Claim due items with `SKIP LOCKED` (newest ranges first).
    ///
    /// NOTE: like the repair queue, the claim is not wrapped in an explicit
    /// transaction: it coordinates tasks sharing this pool, which is the
    /// whole deployment (one process per chain). Claimed ranges stay
    /// idempotent, so a duplicate claim only repeats work.
    async fn claim_work(
        &self,
        chain_id: &str,
        job_name: &str,
        limit: i64,
    ) -> eyre::Result<Vec<WorkItemRow>>;

    /// Complete one item: success deletes it, failure backs off or parks it.
    async fn complete_work(
        &self,
        id: i64,
        success: bool,
        error: Option<&str>,
        max_attempts: i32,
        backoff_secs: i64,
    ) -> eyre::Result<()>;

    /// Register or refresh a feed.
    async fn upsert_feed(
        &self,
        chain_id: &str,
        name: &str,
        kind: &str,
        settings: &serde_json::Value,
    ) -> eyre::Result<()>;

    /// Fetch a feed row.
    async fn get_feed(&self, chain_id: &str, name: &str) -> eyre::Result<Option<FeedRow>>;
}

/// PostgreSQL control-plane implementation.
#[async_trait::async_trait]
impl JobControlPlane for crate::PostgresStorage {
    async fn upsert_job(
        &self,
        chain_id: &str,
        name: &str,
        spec: &serde_json::Value,
        spec_hash: &str,
        desired: &str,
    ) -> eyre::Result<()> {
        sqlx::query(
            "INSERT INTO jobs (chain_id, name, spec, spec_hash, desired, updated_at)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT (chain_id, name) DO UPDATE SET
               spec = EXCLUDED.spec, spec_hash = EXCLUDED.spec_hash,
               desired = EXCLUDED.desired, updated_at = NOW()",
        )
        .bind(chain_id)
        .bind(name)
        .bind(spec)
        .bind(spec_hash)
        .bind(desired)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn get_job(&self, chain_id: &str, name: &str) -> eyre::Result<Option<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(
            "SELECT chain_id, name, spec, spec_hash, desired, updated_at
             FROM jobs WHERE chain_id = $1 AND name = $2",
        )
        .bind(chain_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn list_jobs(&self, chain_id: &str) -> eyre::Result<Vec<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(
            "SELECT chain_id, name, spec, spec_hash, desired, updated_at
             FROM jobs WHERE chain_id = $1 ORDER BY name",
        )
        .bind(chain_id)
        .fetch_all(self.pool())
        .await?)
    }

    async fn delete_job(&self, chain_id: &str, name: &str) -> eyre::Result<()> {
        // Drop physical output tables and the alias view first: the alias is
        // the stable query surface, so it must not outlive the job serving
        // stale data. Names come from stored rows and pass the allow-list;
        // CASCADE covers dependent views and materialized views. These names
        // live in the job-owned `{alias}[__vN]` namespace.
        if let Some(job) = self.get_job(chain_id, name).await? {
            if let Some(table) = job
                .spec
                .get("output")
                .and_then(|output| output.get("table"))
                .and_then(|table| table.as_str())
            {
                crate::skeleton::validate_sql_name(table).map_err(|e| eyre::eyre!("{e}"))?;
                let versions = self.list_job_versions(chain_id, name).await?;
                for version in &versions {
                    let physical = format!("{}__v{}", table, version.version);
                    crate::skeleton::validate_sql_name(&physical)
                        .map_err(|e| eyre::eyre!("{e}"))?;
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DROP TABLE IF EXISTS {physical} CASCADE"
                    )))
                    .execute(self.pool())
                    .await?;
                }
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP VIEW IF EXISTS {table} CASCADE"
                )))
                .execute(self.pool())
                .await?;
            }
        }
        sqlx::query("DELETE FROM catalog_objects WHERE chain_id = $1 AND owner_job = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM work_queue WHERE chain_id = $1 AND job_name = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM job_cursors WHERE chain_id = $1 AND name = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM job_versions WHERE chain_id = $1 AND name = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM jobs WHERE chain_id = $1 AND name = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn insert_job_version(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        spec_hash: &str,
        scan_from: i64,
        scan_to: Option<i64>,
    ) -> eyre::Result<()> {
        // Cursors mean "last *completed* height": a fresh version starts
        // exactly at `scan_from` instead of skipping it.
        let initial = scan_from.saturating_sub(1);
        sqlx::query(
            "INSERT INTO job_versions
               (chain_id, name, version, spec_hash, status, scan_from, scan_to, scan_cursor)
             VALUES ($1, $2, $3, $4, 'draft', $5, $6, $7)
             ON CONFLICT (chain_id, name, version) DO NOTHING",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .bind(spec_hash)
        .bind(scan_from)
        .bind(scan_to)
        .bind(initial)
        .execute(self.pool())
        .await?;
        sqlx::query(
            "INSERT INTO job_cursors (chain_id, name, version, cursor, tip)
             VALUES ($1, $2, $3, $4, $4)
             ON CONFLICT (chain_id, name, version) DO NOTHING",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .bind(initial)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn get_job_version(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
    ) -> eyre::Result<Option<JobVersionRow>> {
        Ok(sqlx::query_as::<_, JobVersionRow>(
            "SELECT chain_id, name, version, spec_hash, status, scan_from, scan_to,
                    scan_cursor, rows_written, last_error, started_at, finished_at
             FROM job_versions WHERE chain_id = $1 AND name = $2 AND version = $3",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn list_job_versions(
        &self,
        chain_id: &str,
        name: &str,
    ) -> eyre::Result<Vec<JobVersionRow>> {
        Ok(sqlx::query_as::<_, JobVersionRow>(
            "SELECT chain_id, name, version, spec_hash, status, scan_from, scan_to,
                    scan_cursor, rows_written, last_error, started_at, finished_at
             FROM job_versions WHERE chain_id = $1 AND name = $2 ORDER BY version DESC",
        )
        .bind(chain_id)
        .bind(name)
        .fetch_all(self.pool())
        .await?)
    }

    async fn set_version_status(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        status: VersionStatus,
        error: Option<&str>,
    ) -> eyre::Result<()> {
        let current: Option<String> = sqlx::query_scalar(
            "SELECT status FROM job_versions WHERE chain_id = $1 AND name = $2 AND version = $3",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .fetch_optional(self.pool())
        .await?;
        if let Some(current) = current {
            let from = VersionStatus::parse(&current)
                .ok_or_else(|| eyre::eyre!("unknown stored version status: {current}"))?;
            if !from.can_transition_to(status) {
                eyre::bail!(
                    "illegal version transition {} -> {}",
                    from.as_str(),
                    status.as_str()
                );
            }
        }
        let finished = matches!(
            status,
            VersionStatus::Active | VersionStatus::Retired | VersionStatus::Failed
        );
        sqlx::query(
            "UPDATE job_versions SET status = $4, last_error = $5,
               finished_at = CASE WHEN $6 THEN NOW() ELSE finished_at END,
               updated_at = NOW()
             WHERE chain_id = $1 AND name = $2 AND version = $3",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .bind(status.as_str())
        .bind(error)
        .bind(finished)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn advance_version_cursor(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
        cursor: i64,
        rows_added: i64,
    ) -> eyre::Result<()> {
        let mut tx = self.pool().begin().await?;
        advance_version_cursor_tx(&mut tx, chain_id, name, version, cursor, rows_added).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn get_cursor(
        &self,
        chain_id: &str,
        name: &str,
        version: i32,
    ) -> eyre::Result<Option<JobCursorRow>> {
        Ok(sqlx::query_as::<_, JobCursorRow>(
            "SELECT chain_id, name, version, cursor, tip
             FROM job_cursors WHERE chain_id = $1 AND name = $2 AND version = $3",
        )
        .bind(chain_id)
        .bind(name)
        .bind(version)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn upsert_catalog_object(&self, object: &CatalogObjectRow) -> eyre::Result<()> {
        sqlx::query(
            "INSERT INTO catalog_objects
               (chain_id, name, kind, ddl, select_sql, checksum, public,
                block_column, reorg_mode, owner_job, backfill, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,NOW())
             ON CONFLICT (chain_id, name) DO UPDATE SET
               kind = EXCLUDED.kind, ddl = EXCLUDED.ddl,
               select_sql = EXCLUDED.select_sql, checksum = EXCLUDED.checksum,
               public = EXCLUDED.public, block_column = EXCLUDED.block_column,
               reorg_mode = EXCLUDED.reorg_mode, owner_job = EXCLUDED.owner_job,
               backfill = EXCLUDED.backfill, updated_at = NOW()",
        )
        .bind(&object.chain_id)
        .bind(&object.name)
        .bind(&object.kind)
        .bind(&object.ddl)
        .bind(&object.select_sql)
        .bind(&object.checksum)
        .bind(object.public)
        .bind(&object.block_column)
        .bind(&object.reorg_mode)
        .bind(&object.owner_job)
        .bind(&object.backfill)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn list_public_catalog(&self, chain_id: &str) -> eyre::Result<Vec<CatalogObjectRow>> {
        Ok(sqlx::query_as::<_, CatalogObjectRow>(
            "SELECT chain_id, name, kind, ddl, select_sql, checksum, public,
                    block_column, reorg_mode, owner_job, backfill
             FROM catalog_objects WHERE chain_id = $1 AND public ORDER BY name",
        )
        .bind(chain_id)
        .fetch_all(self.pool())
        .await?)
    }

    async fn delete_catalog_object(&self, chain_id: &str, name: &str) -> eyre::Result<()> {
        sqlx::query("DELETE FROM catalog_objects WHERE chain_id = $1 AND name = $2")
            .bind(chain_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn enqueue_work(
        &self,
        chain_id: &str,
        job_name: &str,
        job_version: i32,
        range_lo: i64,
        range_hi: i64,
    ) -> eyre::Result<()> {
        sqlx::query(
            "INSERT INTO work_queue (chain_id, job_name, job_version, range_lo, range_hi)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(chain_id)
        .bind(job_name)
        .bind(job_version)
        .bind(range_lo)
        .bind(range_hi)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn claim_work(
        &self,
        chain_id: &str,
        job_name: &str,
        limit: i64,
    ) -> eyre::Result<Vec<WorkItemRow>> {
        Ok(sqlx::query_as::<_, WorkItemRow>(
            "SELECT id, chain_id, job_name, job_version, range_lo, range_hi,
                    attempts, next_retry_at, last_error, done
             FROM work_queue
             WHERE chain_id = $1 AND job_name = $2 AND done = FALSE
               AND next_retry_at <= NOW()
             ORDER BY range_hi DESC LIMIT $3
             FOR UPDATE SKIP LOCKED",
        )
        .bind(chain_id)
        .bind(job_name)
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }

    async fn complete_work(
        &self,
        id: i64,
        success: bool,
        error: Option<&str>,
        max_attempts: i32,
        backoff_secs: i64,
    ) -> eyre::Result<()> {
        if success {
            sqlx::query("DELETE FROM work_queue WHERE id = $1")
                .bind(id)
                .execute(self.pool())
                .await?;
            return Ok(());
        }
        sqlx::query(
            "UPDATE work_queue SET attempts = attempts + 1,
               last_error = $2,
               next_retry_at = NOW() + make_interval(secs => $3 * (2 ^ attempts)),
               done = (attempts + 1) >= $4
             WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .bind(backoff_secs as f64)
        .bind(max_attempts)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn upsert_feed(
        &self,
        chain_id: &str,
        name: &str,
        kind: &str,
        settings: &serde_json::Value,
    ) -> eyre::Result<()> {
        sqlx::query(
            "INSERT INTO feeds (chain_id, name, kind, settings, updated_at)
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT (chain_id, name) DO UPDATE SET
               kind = EXCLUDED.kind, settings = EXCLUDED.settings, updated_at = NOW()",
        )
        .bind(chain_id)
        .bind(name)
        .bind(kind)
        .bind(settings)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn get_feed(&self, chain_id: &str, name: &str) -> eyre::Result<Option<FeedRow>> {
        Ok(sqlx::query_as::<_, FeedRow>(
            "SELECT chain_id, name, kind, settings, cursor
             FROM feeds WHERE chain_id = $1 AND name = $2",
        )
        .bind(chain_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await?)
    }
}

/// Advance a version cursor inside an explicit transaction: the row
/// writes and the cursor move commit atomically, so a quota breach rolls
/// back the partial chunk instead of leaving duplicates behind.
pub async fn advance_version_cursor_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    chain_id: &str,
    name: &str,
    version: i32,
    cursor: i64,
    rows_added: i64,
) -> eyre::Result<()> {
    sqlx::query(
        "UPDATE job_versions SET scan_cursor = GREATEST(COALESCE(scan_cursor, scan_from), $4),
           rows_written = rows_written + $5, updated_at = NOW()
         WHERE chain_id = $1 AND name = $2 AND version = $3",
    )
    .bind(chain_id)
    .bind(name)
    .bind(version)
    .bind(cursor)
    .bind(rows_added)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO job_cursors (chain_id, name, version, cursor, tip)
         VALUES ($1, $2, $3, $4, $4)
         ON CONFLICT (chain_id, name, version) DO UPDATE SET
           cursor = GREATEST(job_cursors.cursor, EXCLUDED.cursor),
           tip = GREATEST(job_cursors.tip, EXCLUDED.tip),
           updated_at = NOW()",
    )
    .bind(chain_id)
    .bind(name)
    .bind(version)
    .bind(cursor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_allows_documented_transitions() {
        assert!(VersionStatus::Draft.can_transition_to(VersionStatus::Scanning));
        assert!(VersionStatus::Scanning.can_transition_to(VersionStatus::CatchingUp));
        assert!(VersionStatus::CatchingUp.can_transition_to(VersionStatus::Active));
        assert!(VersionStatus::CatchingUp.can_transition_to(VersionStatus::Scanning));
        assert!(VersionStatus::Scanning.can_transition_to(VersionStatus::Failed));
        assert!(VersionStatus::Failed.can_transition_to(VersionStatus::Scanning));
        assert!(VersionStatus::Active.can_transition_to(VersionStatus::Retired));
        assert!(VersionStatus::Active.can_transition_to(VersionStatus::Paused));
        assert!(VersionStatus::Active.can_transition_to(VersionStatus::Active));
    }

    #[test]
    fn lifecycle_rejects_skips_and_resurrection() {
        assert!(!VersionStatus::Draft.can_transition_to(VersionStatus::Active));
        assert!(!VersionStatus::Retired.can_transition_to(VersionStatus::Scanning));
        assert!(!VersionStatus::Failed.can_transition_to(VersionStatus::Active));
        assert!(!VersionStatus::Active.can_transition_to(VersionStatus::Scanning));
        assert!(!VersionStatus::Draft.can_transition_to(VersionStatus::Retired));
        assert!(VersionStatus::Paused.can_transition_to(VersionStatus::Scanning));
        assert!(VersionStatus::Paused.can_transition_to(VersionStatus::Retired));
        assert!(!VersionStatus::Paused.can_transition_to(VersionStatus::Active));
    }

    #[test]
    fn status_strings_round_trip() {
        for status in [
            VersionStatus::Draft,
            VersionStatus::Scanning,
            VersionStatus::CatchingUp,
            VersionStatus::Active,
            VersionStatus::Failed,
            VersionStatus::Retired,
            VersionStatus::Paused,
        ] {
            assert_eq!(VersionStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(VersionStatus::parse("bogus"), None);
    }
}
