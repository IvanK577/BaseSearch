//! Durable storage for API-visible job snapshots.
//!
//! The companion database contains status/history only. Worker closures,
//! cancellation flags, uploaded file bytes, and authentication material never
//! enter this store.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::jobs::{JobKind, JobQueueLimits, JobSnapshot};

const TOKEN_DIGEST_PREFIX: &str = "sha256:";

pub fn job_store_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("jobs.db")
}

pub struct JobStore {
    conn: Mutex<Connection>,
    exports_dir: PathBuf,
}

impl JobStore {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(job_store_path(db_path))
            .map_err(|err| format!("open job history database: {err}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS jobs (
                 id INTEGER PRIMARY KEY CHECK (id > 0),
                 status TEXT NOT NULL CHECK (
                     status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')
                 ),
                 updated_ms INTEGER NOT NULL CHECK (updated_ms >= 0),
                 snapshot_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_jobs_updated
                 ON jobs(updated_ms DESC, id DESC);
             CREATE TABLE IF NOT EXISTS job_settings (
                 key TEXT PRIMARY KEY,
                 value_json TEXT NOT NULL
             );",
        )
        .map_err(|err| format!("initialize job history database: {err}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            exports_dir: db_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("exports"),
        })
    }

    pub fn load_or_initialize_limits(
        &self,
        defaults: JobQueueLimits,
    ) -> Result<JobQueueLimits, String> {
        const KEY: &str = "queue_limits_v1";
        let conn = self.connection()?;
        let stored = conn
            .query_row(
                "SELECT value_json FROM job_settings WHERE key = ?1",
                [KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| format!("load job queue limits: {err}"))?;
        if let Some(value) = stored {
            return serde_json::from_str::<JobQueueLimits>(&value)
                .map(JobQueueLimits::normalized)
                .map_err(|err| format!("decode job queue limits: {err}"));
        }
        let limits = defaults.normalized();
        let value = serde_json::to_string(&limits)
            .map_err(|err| format!("encode job queue limits: {err}"))?;
        conn.execute(
            "INSERT INTO job_settings(key, value_json) VALUES (?1, ?2)",
            params![KEY, value],
        )
        .map_err(|err| format!("persist job queue limits: {err}"))?;
        Ok(limits)
    }

    pub fn load(&self) -> Result<Vec<JobSnapshot>, String> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT id, status, snapshot_json
                 FROM jobs
                 ORDER BY id DESC",
            )
            .map_err(|err| format!("prepare job history load: {err}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|err| format!("query job history: {err}"))?;

        let mut snapshots = Vec::new();
        for row in rows {
            let (stored_id, stored_status, snapshot_json) =
                row.map_err(|err| format!("read job history row: {err}"))?;
            let mut snapshot: JobSnapshot = serde_json::from_str(&snapshot_json)
                .map_err(|err| format!("decode job {stored_id} snapshot: {err}"))?;
            if stored_id < 1 || snapshot.id != stored_id as u64 {
                return Err(format!(
                    "job history row {stored_id} has mismatched snapshot id {}",
                    snapshot.id
                ));
            }
            if stored_status != snapshot.status.as_str() {
                return Err(format!("job history row {stored_id} has mismatched status"));
            }
            self.resolve_export_token(&mut snapshot);
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    }

    pub fn upsert(&self, snapshot: &JobSnapshot) -> Result<(), String> {
        let id = i64::try_from(snapshot.id)
            .map_err(|_| format!("job id {} exceeds SQLite range", snapshot.id))?;
        let updated_ms = i64::try_from(snapshot.updated_ms)
            .map_err(|_| format!("job timestamp {} exceeds SQLite range", snapshot.updated_ms))?;
        let persistable = snapshot_for_storage(snapshot);
        let snapshot_json = serde_json::to_string(&persistable)
            .map_err(|err| format!("encode job {} snapshot: {err}", snapshot.id))?;
        self.connection()?
            .execute(
                "INSERT INTO jobs(id, status, updated_ms, snapshot_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     status = excluded.status,
                     updated_ms = excluded.updated_ms,
                     snapshot_json = excluded.snapshot_json",
                params![id, snapshot.status.as_str(), updated_ms, snapshot_json],
            )
            .map_err(|err| format!("persist job {}: {err}", snapshot.id))?;
        Ok(())
    }

    pub fn delete(&self, ids: &[u64]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.connection()?;
        let transaction = conn
            .transaction()
            .map_err(|err| format!("begin job history prune: {err}"))?;
        {
            let mut statement = transaction
                .prepare("DELETE FROM jobs WHERE id = ?1")
                .map_err(|err| format!("prepare job history prune: {err}"))?;
            for id in ids {
                let id =
                    i64::try_from(*id).map_err(|_| format!("job id {id} exceeds SQLite range"))?;
                statement
                    .execute([id])
                    .map_err(|err| format!("delete job {id} from history: {err}"))?;
            }
        }
        transaction
            .commit()
            .map_err(|err| format!("commit job history prune: {err}"))?;
        Ok(())
    }

    fn resolve_export_token(&self, snapshot: &mut JobSnapshot) {
        if snapshot.kind != JobKind::Export {
            return;
        }
        let Some(digest) = export_token_fields(snapshot)
            .into_iter()
            .find_map(|token| token.strip_prefix(TOKEN_DIGEST_PREFIX).map(str::to_string))
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.exports_dir) else {
            return;
        };
        let matching_token = entries.flatten().find_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_dir())?;
            let candidate = entry.file_name().to_string_lossy().into_owned();
            (token_digest(&candidate) == digest).then_some(candidate)
        });
        let Some(matching_token) = matching_token else {
            return;
        };
        if let Some(Value::String(token)) = snapshot
            .result
            .as_mut()
            .and_then(Value::as_object_mut)
            .and_then(|result| result.get_mut("token"))
        {
            *token = matching_token.clone();
        }
        if let Some(Value::String(token)) = snapshot
            .input
            .as_mut()
            .and_then(Value::as_object_mut)
            .and_then(|input| input.get_mut("artifact_token"))
        {
            *token = matching_token;
        }
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "job history database lock is poisoned".to_string())
    }
}

fn snapshot_for_storage(snapshot: &JobSnapshot) -> JobSnapshot {
    let mut persistable = snapshot.clone();
    if persistable.kind == JobKind::Export {
        if let Some(Value::String(token)) = persistable
            .result
            .as_mut()
            .and_then(Value::as_object_mut)
            .and_then(|result| result.get_mut("token"))
            && !token.starts_with(TOKEN_DIGEST_PREFIX)
        {
            *token = format!("{TOKEN_DIGEST_PREFIX}{}", token_digest(token));
        }
        if let Some(Value::String(token)) = persistable
            .input
            .as_mut()
            .and_then(Value::as_object_mut)
            .and_then(|input| input.get_mut("artifact_token"))
            && !token.starts_with(TOKEN_DIGEST_PREFIX)
        {
            *token = format!("{TOKEN_DIGEST_PREFIX}{}", token_digest(token));
        }
    }
    persistable
}

fn export_token_fields(snapshot: &JobSnapshot) -> Vec<&str> {
    let mut fields = Vec::with_capacity(2);
    if let Some(token) = snapshot
        .result
        .as_ref()
        .and_then(|result| result.get("token"))
        .and_then(Value::as_str)
    {
        fields.push(token);
    }
    if let Some(token) = snapshot
        .input
        .as_ref()
        .and_then(|input| input.get("artifact_token"))
        .and_then(Value::as_str)
    {
        fields.push(token);
    }
    fields
}

fn token_digest(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
