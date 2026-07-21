//! Job model for long-running operations (import, export, maintenance). Live
//! cancellation stays in memory while API-visible snapshots are mirrored to a
//! companion SQLite store. Jobs run on dedicated OS threads so blocking
//! SQLite / Excel work never stalls the async runtime, and the browser polls
//! their progress through the `/api/jobs` endpoints.
//!
//! SQLite has a single writer, so write jobs run through a durable FIFO queue.
//! Read jobs use a separate bounded queue so exports cannot create an
//! unbounded number of worker threads.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth::{Identity, Permission, Role};
use super::job_store::JobStore;

const INTERRUPTED_ERROR: &str = "Job interrupted because Base Search stopped before it completed.";
const INCOMPLETE_ERROR: &str = "Job worker stopped without publishing a final result.";
const PANIC_ERROR: &str = "Job worker stopped unexpectedly.";
const JOB_HISTORY_LIMIT: usize = 40;
const DEFAULT_MAX_READ_JOBS: usize = 2;
const DEFAULT_MAX_WORKSPACE_PENDING: usize = 24;
const DEFAULT_MAX_USER_PENDING: usize = 6;
const DEFAULT_MAX_ACTIVE_PREVIEWS: usize = 2;
const DEFAULT_UPLOAD_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
const DEFAULT_EXPORT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const ARTIFACT_CLEANUP_INTERVAL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobQueueLimits {
    pub workspace_pending: usize,
    pub per_user_pending: usize,
    pub concurrent_reads: usize,
    pub concurrent_previews: usize,
}

impl Default for JobQueueLimits {
    fn default() -> Self {
        Self {
            workspace_pending: DEFAULT_MAX_WORKSPACE_PENDING,
            per_user_pending: DEFAULT_MAX_USER_PENDING,
            concurrent_reads: DEFAULT_MAX_READ_JOBS,
            concurrent_previews: DEFAULT_MAX_ACTIVE_PREVIEWS,
        }
    }
}

impl JobQueueLimits {
    pub(super) fn normalized(self) -> Self {
        Self {
            workspace_pending: self.workspace_pending.clamp(1, 1_024),
            per_user_pending: self
                .per_user_pending
                .clamp(1, self.workspace_pending.clamp(1, 1_024)),
            concurrent_reads: self.concurrent_reads.clamp(1, 32),
            concurrent_previews: self.concurrent_previews.clamp(1, 8),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactTtl {
    pub uploads_ms: u64,
    pub exports_ms: u64,
}

impl Default for ArtifactTtl {
    fn default() -> Self {
        Self {
            uploads_ms: DEFAULT_UPLOAD_TTL_MS,
            exports_ms: DEFAULT_EXPORT_TTL_MS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArtifactCleanupReport {
    pub removed_uploads: usize,
    pub removed_exports: usize,
    pub skipped_active: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Import,
    Export,
    Optimize,
    Compact,
    Reindex,
    Clear,
    #[cfg_attr(not(feature = "duckdb-olap"), allow(dead_code))]
    OlapBuild,
}

impl JobKind {
    /// Write jobs mutate the database and must not run concurrently.
    fn is_write(self) -> bool {
        !matches!(self, JobKind::Export)
    }

    fn permission(self) -> Permission {
        match self {
            JobKind::Import => Permission::Import,
            JobKind::Export => Permission::Export,
            JobKind::Optimize
            | JobKind::Compact
            | JobKind::Reindex
            | JobKind::Clear
            | JobKind::OlapBuild => Permission::MaintainDatabase,
        }
    }

    fn is_single_flight(self) -> bool {
        matches!(
            self,
            JobKind::Optimize
                | JobKind::Compact
                | JobKind::Reindex
                | JobKind::Clear
                | JobKind::OlapBuild
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobVisibility {
    #[default]
    Private,
    Workspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JobProgress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub percent: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: u64,
    pub kind: JobKind,
    pub status: JobStatus,
    #[serde(default = "legacy_owner_user_id")]
    pub owner_user_id: String,
    #[serde(default)]
    pub visibility: JobVisibility,
    pub title: String,
    pub progress: JobProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Immutable, bounded request metadata needed to explain or audit a job
    /// after the process restarts. File contents and private temporary paths
    /// are never stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    pub cancellable: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_cancellable_reason: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

fn legacy_owner_user_id() -> String {
    "legacy-system".to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobCreateError {
    Forbidden,
    UserQueueFull,
    WorkspaceQueueFull,
    MaintenanceBusy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewAdmissionError {
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAccessError {
    NotFound,
    Forbidden,
}

struct JobEntry {
    snapshot: JobSnapshot,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum WorkClass {
    Read,
    Write,
}

struct PendingJob {
    id: u64,
    owner_user_id: String,
    cancel: Arc<AtomicBool>,
    work: Box<dyn FnOnce(JobHandle) + Send + 'static>,
    class: WorkClass,
}

struct ReservedJob {
    owner_user_id: String,
    kind: JobKind,
}

struct RegistryState {
    entries: HashMap<u64, JobEntry>,
    write_queue: VecDeque<PendingJob>,
    read_queue: VecDeque<PendingJob>,
    reservations: HashMap<u64, ReservedJob>,
    active_write: bool,
    active_reads: usize,
    active_previews: usize,
}

impl RegistryState {
    fn new(entries: HashMap<u64, JobEntry>) -> Self {
        Self {
            entries,
            write_queue: VecDeque::new(),
            read_queue: VecDeque::new(),
            reservations: HashMap::new(),
            active_write: false,
            active_reads: 0,
            active_previews: 0,
        }
    }
}

#[derive(Clone)]
struct ArtifactDirectories {
    uploads: PathBuf,
    exports: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Shared registry of jobs. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct JobRegistry {
    inner: Arc<Mutex<RegistryState>>,
    next_id: Arc<AtomicU64>,
    next_reservation_id: Arc<AtomicU64>,
    limits: JobQueueLimits,
    store: Option<Arc<JobStore>>,
    artifacts: Option<ArtifactDirectories>,
    last_artifact_cleanup_ms: Arc<AtomicU64>,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::with_limits(JobQueueLimits::default())
    }

    pub(crate) fn with_limits(limits: JobQueueLimits) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::new(HashMap::new()))),
            next_id: Arc::new(AtomicU64::new(1)),
            next_reservation_id: Arc::new(AtomicU64::new(1)),
            limits: limits.normalized(),
            store: None,
            artifacts: None,
            last_artifact_cleanup_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Opens durable history for a workspace and reconciles work that could
    /// not have survived the previous process.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        Self::open_with_limits(db_path, JobQueueLimits::default())
    }

    pub(crate) fn open_with_limits(
        db_path: &Path,
        default_limits: JobQueueLimits,
    ) -> Result<Self, String> {
        let store = Arc::new(JobStore::open(db_path)?);
        let limits = store.load_or_initialize_limits(default_limits.normalized())?;
        let mut entries = HashMap::new();
        let mut highest_id = 0;

        for mut snapshot in store.load()? {
            highest_id = highest_id.max(snapshot.id);
            if !snapshot.status.is_terminal() {
                snapshot.status = JobStatus::Failed;
                snapshot.cancellable = false;
                snapshot.error = Some(INTERRUPTED_ERROR.to_string());
                snapshot.updated_ms = now_ms().max(snapshot.updated_ms);
                store.upsert(&snapshot)?;
            }
            entries.insert(
                snapshot.id,
                JobEntry {
                    snapshot,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        }

        let registry = Self {
            inner: Arc::new(Mutex::new(RegistryState::new(entries))),
            next_id: Arc::new(AtomicU64::new(highest_id.saturating_add(1).max(1))),
            next_reservation_id: Arc::new(AtomicU64::new(1)),
            limits,
            store: Some(store),
            artifacts: Some(ArtifactDirectories {
                uploads: db_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("uploads"),
                exports: db_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("exports"),
            }),
            last_artifact_cleanup_ms: Arc::new(AtomicU64::new(now_ms())),
        };
        if let Err(error) = registry.cleanup_default_artifacts() {
            eprintln!("WARNING: {error}");
        }
        registry.prune(JOB_HISTORY_LIMIT);
        Ok(registry)
    }

    pub(crate) fn queue_limits(&self) -> JobQueueLimits {
        self.limits
    }

    /// Registers a new job in the `Queued` state and returns a handle the
    /// worker uses to publish progress and the final outcome.
    #[cfg(test)]
    pub fn create(&self, kind: JobKind, title: impl Into<String>) -> JobHandle {
        self.create_owned(
            kind,
            title,
            "local-owner".to_string(),
            JobVisibility::Private,
            None,
        )
    }

    fn create_owned(
        &self,
        kind: JobKind,
        title: impl Into<String>,
        owner_user_id: String,
        visibility: JobVisibility,
        input: Option<Value>,
    ) -> JobHandle {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        let now = now_ms();
        let snapshot = JobSnapshot {
            id,
            kind,
            status: JobStatus::Queued,
            owner_user_id,
            visibility,
            title: title.into(),
            progress: JobProgress::default(),
            message: None,
            error: None,
            result: None,
            input,
            cancellable: true,
            cancel_requested: false,
            non_cancellable_reason: None,
            created_ms: now,
            updated_ms: now,
        };
        self.inner.lock().unwrap().entries.insert(
            id,
            JobEntry {
                snapshot: snapshot.clone(),
                cancel: Arc::clone(&cancel),
            },
        );
        self.persist(&snapshot);
        JobHandle {
            id,
            registry: self.clone(),
            cancel,
        }
    }

    pub fn snapshot(&self, id: u64) -> Option<JobSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|entry| entry.snapshot.clone())
    }

    pub fn snapshot_for(&self, identity: &Identity, id: u64) -> Option<JobSnapshot> {
        self.snapshot(id)
            .filter(|snapshot| can_view(identity, snapshot))
            .map(|snapshot| redact_for(identity, snapshot))
    }

    /// All retained jobs, newest first.
    pub fn list(&self) -> Vec<JobSnapshot> {
        let mut jobs: Vec<JobSnapshot> = self
            .inner
            .lock()
            .unwrap()
            .entries
            .values()
            .map(|entry| entry.snapshot.clone())
            .collect();
        jobs.sort_by_key(|job| Reverse(job.id));
        jobs
    }

    pub fn list_for(&self, identity: &Identity) -> Vec<JobSnapshot> {
        self.list()
            .into_iter()
            .filter(|snapshot| can_view(identity, snapshot))
            .map(|snapshot| redact_for(identity, snapshot))
            .collect()
    }

    pub(crate) fn reserve_for(
        &self,
        identity: &Identity,
        kind: JobKind,
    ) -> Result<JobAdmission, JobCreateError> {
        if !identity.role.allows(kind.permission()) {
            return Err(JobCreateError::Forbidden);
        }

        let reservation_id = self.next_reservation_id.fetch_add(1, Ordering::SeqCst);
        let mut state = self.inner.lock().unwrap();
        if kind.is_single_flight()
            && (state
                .entries
                .values()
                .any(|entry| entry.snapshot.kind == kind && !entry.snapshot.status.is_terminal())
                || state
                    .reservations
                    .values()
                    .any(|reservation| reservation.kind == kind))
        {
            return Err(JobCreateError::MaintenanceBusy);
        }

        let pending_for_user = state
            .write_queue
            .iter()
            .chain(state.read_queue.iter())
            .filter(|pending| pending.owner_user_id == identity.user_id)
            .count()
            + state
                .reservations
                .values()
                .filter(|reservation| reservation.owner_user_id == identity.user_id)
                .count();
        if pending_for_user >= self.limits.per_user_pending {
            return Err(JobCreateError::UserQueueFull);
        }
        let workspace_pending =
            state.write_queue.len() + state.read_queue.len() + state.reservations.len();
        if workspace_pending >= self.limits.workspace_pending {
            return Err(JobCreateError::WorkspaceQueueFull);
        }

        state.reservations.insert(
            reservation_id,
            ReservedJob {
                owner_user_id: identity.user_id.clone(),
                kind,
            },
        );
        drop(state);
        Ok(JobAdmission {
            id: reservation_id,
            registry: self.clone(),
            kind,
            owner_user_id: identity.user_id.clone(),
            consumed: false,
        })
    }

    pub(crate) fn acquire_preview(&self) -> Result<PreviewPermit, PreviewAdmissionError> {
        let mut state = self.inner.lock().unwrap();
        if state.active_previews >= self.limits.concurrent_previews {
            return Err(PreviewAdmissionError::Busy);
        }
        state.active_previews += 1;
        drop(state);
        Ok(PreviewPermit {
            registry: self.clone(),
            released: false,
        })
    }

    /// Requests cancellation of a running/queued job. Returns false when the
    /// job is unknown or already finished.
    pub fn cancel(&self, id: u64) -> bool {
        let mut changed_snapshot = None;
        let mut dispatch = false;
        let cancelled = {
            let mut state = self.inner.lock().unwrap();
            let Some(entry) = state.entries.get_mut(&id) else {
                return false;
            };
            if entry.snapshot.status.is_terminal() || !entry.snapshot.cancellable {
                return false;
            }
            entry.cancel.store(true, Ordering::SeqCst);
            entry.snapshot.cancel_requested = true;
            entry.snapshot.cancellable = false;
            if entry.snapshot.status == JobStatus::Queued {
                let before = state.write_queue.len() + state.read_queue.len();
                state.write_queue.retain(|pending| pending.id != id);
                state.read_queue.retain(|pending| pending.id != id);
                if state.write_queue.len() + state.read_queue.len() < before {
                    let entry = state.entries.get_mut(&id).expect("job entry still exists");
                    entry.snapshot.status = JobStatus::Cancelled;
                    entry.snapshot.updated_ms = now_ms();
                    changed_snapshot = Some(entry.snapshot.clone());
                    dispatch = true;
                }
            } else {
                let entry = state.entries.get_mut(&id).expect("job entry still exists");
                entry.snapshot.updated_ms = now_ms();
                changed_snapshot = Some(entry.snapshot.clone());
            }
            true
        };
        if let Some(snapshot) = changed_snapshot {
            self.persist(&snapshot);
            self.prune(JOB_HISTORY_LIMIT);
        }
        if dispatch {
            self.dispatch_available();
        }
        cancelled
    }

    pub fn cancel_for(&self, identity: &Identity, id: u64) -> Result<bool, JobAccessError> {
        let snapshot = self.snapshot(id).ok_or(JobAccessError::NotFound)?;
        if !identity.role.is_privileged() && snapshot.owner_user_id != identity.user_id {
            return Err(JobAccessError::Forbidden);
        }
        Ok(self.cancel(id))
    }

    /// Drops finished jobs beyond the newest `keep`, so a long session does not
    /// accumulate history without bound.
    pub fn prune(&self, keep: usize) {
        let mut guard = self.inner.lock().unwrap();
        let mut terminal: Vec<(u64, u64)> = guard
            .entries
            .values()
            .filter(|entry| entry.snapshot.status.is_terminal())
            .map(|entry| (entry.snapshot.id, entry.snapshot.updated_ms))
            .collect();
        if terminal.len() <= keep {
            return;
        }
        terminal.sort_by_key(|(id, updated_ms)| Reverse((*updated_ms, *id)));
        let remove_ids: Vec<u64> = terminal.into_iter().skip(keep).map(|(id, _)| id).collect();
        if let Some(store) = &self.store
            && let Err(err) = store.delete(&remove_ids)
        {
            eprintln!("WARNING: {err}");
            return;
        }
        for id in remove_ids {
            guard.entries.remove(&id);
        }
    }

    fn mutate(&self, id: u64, f: impl FnOnce(&mut JobSnapshot)) {
        let snapshot = {
            let mut state = self.inner.lock().unwrap();
            state.entries.get_mut(&id).map(|entry| {
                f(&mut entry.snapshot);
                entry.snapshot.updated_ms = now_ms();
                entry.snapshot.clone()
            })
        };
        if let Some(snapshot) = snapshot {
            self.persist(&snapshot);
        }
    }

    fn enqueue_admitted<F>(
        &self,
        mut admission: JobAdmission,
        title: impl Into<String>,
        visibility: JobVisibility,
        input: Option<Value>,
        work: F,
    ) -> Result<JobSnapshot, JobCreateError>
    where
        F: FnOnce(JobHandle) + Send + 'static,
    {
        self.prune(JOB_HISTORY_LIMIT);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        let now = now_ms();
        let snapshot = JobSnapshot {
            id,
            kind: admission.kind,
            status: JobStatus::Queued,
            owner_user_id: admission.owner_user_id.clone(),
            visibility,
            title: title.into(),
            progress: JobProgress::default(),
            message: None,
            error: None,
            result: None,
            input,
            cancellable: true,
            cancel_requested: false,
            non_cancellable_reason: None,
            created_ms: now,
            updated_ms: now,
        };
        let pending = PendingJob {
            id,
            owner_user_id: admission.owner_user_id.clone(),
            cancel: Arc::clone(&cancel),
            work: Box::new(work),
            class: if admission.kind.is_write() {
                WorkClass::Write
            } else {
                WorkClass::Read
            },
        };
        {
            let mut state = self.inner.lock().unwrap();
            let Some(reserved) = state.reservations.remove(&admission.id) else {
                return Err(JobCreateError::WorkspaceQueueFull);
            };
            if reserved.kind != admission.kind || reserved.owner_user_id != admission.owner_user_id
            {
                return Err(JobCreateError::Forbidden);
            }
            state.entries.insert(
                id,
                JobEntry {
                    snapshot: snapshot.clone(),
                    cancel,
                },
            );
            match pending.class {
                WorkClass::Write => state.write_queue.push_back(pending),
                WorkClass::Read => state.read_queue.push_back(pending),
            }
        }
        admission.consumed = true;
        self.persist(&snapshot);
        self.dispatch_available();
        self.maybe_cleanup_artifacts();
        Ok(snapshot)
    }

    fn dispatch_available(&self) {
        loop {
            let next = {
                let mut state = self.inner.lock().unwrap();
                let pending = if !state.active_write {
                    match state.write_queue.pop_front() {
                        Some(pending) => {
                            state.active_write = true;
                            Some(pending)
                        }
                        None => None,
                    }
                } else {
                    None
                }
                .or_else(|| {
                    if state.active_reads < self.limits.concurrent_reads {
                        match state.read_queue.pop_front() {
                            Some(pending) => {
                                state.active_reads += 1;
                                Some(pending)
                            }
                            None => None,
                        }
                    } else {
                        None
                    }
                });

                pending.map(|pending| {
                    let snapshot = state
                        .entries
                        .get_mut(&pending.id)
                        .expect("queued job entry exists");
                    snapshot.snapshot.status = JobStatus::Running;
                    snapshot.snapshot.updated_ms = now_ms();
                    (pending, snapshot.snapshot.clone())
                })
            };

            let Some((pending, running_snapshot)) = next else {
                break;
            };
            self.persist(&running_snapshot);
            self.start_worker(pending);
        }
    }

    fn start_worker(&self, pending: PendingJob) {
        let registry = self.clone();
        std::thread::spawn(move || {
            let handle = JobHandle {
                id: pending.id,
                registry: registry.clone(),
                cancel: pending.cancel,
            };
            if handle.is_cancelled() {
                handle.mark_cancelled();
            } else if catch_unwind(AssertUnwindSafe(|| (pending.work)(handle.clone()))).is_err() {
                handle.fail(PANIC_ERROR);
            }
            registry.worker_finished(handle.id, pending.class);
        });
    }

    fn worker_finished(&self, id: u64, class: WorkClass) {
        let final_snapshot = {
            let mut state = self.inner.lock().unwrap();
            match class {
                WorkClass::Write => state.active_write = false,
                WorkClass::Read => state.active_reads = state.active_reads.saturating_sub(1),
            }
            let entry = state.entries.get_mut(&id).expect("worker job entry exists");
            if !entry.snapshot.status.is_terminal() {
                if entry.cancel.load(Ordering::Relaxed) {
                    entry.snapshot.status = JobStatus::Cancelled;
                    entry.snapshot.cancel_requested = true;
                } else {
                    entry.snapshot.status = JobStatus::Failed;
                    entry.snapshot.error = Some(INCOMPLETE_ERROR.to_string());
                }
                entry.snapshot.cancellable = false;
                entry.snapshot.updated_ms = now_ms();
                Some(entry.snapshot.clone())
            } else {
                None
            }
        };
        if let Some(snapshot) = final_snapshot {
            self.persist(&snapshot);
        }
        self.prune(JOB_HISTORY_LIMIT);
        self.maybe_cleanup_artifacts();
        self.dispatch_available();
    }

    fn persist(&self, snapshot: &JobSnapshot) {
        if let Some(store) = &self.store
            && let Err(err) = store.upsert(snapshot)
        {
            eprintln!("WARNING: {err}");
        }
    }

    fn cleanup_default_artifacts(&self) -> Result<ArtifactCleanupReport, String> {
        let Some(directories) = &self.artifacts else {
            return Ok(ArtifactCleanupReport::default());
        };
        self.cleanup_artifacts_at(
            &directories.uploads,
            &directories.exports,
            now_ms(),
            ArtifactTtl::default(),
        )
    }

    fn maybe_cleanup_artifacts(&self) {
        if self.artifacts.is_none() {
            return;
        }
        let now = now_ms();
        let interval_ms = ARTIFACT_CLEANUP_INTERVAL.as_millis() as u64;
        let last = self.last_artifact_cleanup_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < interval_ms
            || self
                .last_artifact_cleanup_ms
                .compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        if let Err(error) = self.cleanup_default_artifacts() {
            eprintln!("WARNING: {error}");
        }
    }

    pub(crate) fn start_artifact_cleanup_task(&self) {
        self.start_artifact_cleanup_task_with(ARTIFACT_CLEANUP_INTERVAL, ArtifactTtl::default());
    }

    fn start_artifact_cleanup_task_with(&self, every: Duration, ttl: ArtifactTtl) {
        if self.artifacts.is_none() {
            return;
        }
        let registry = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(every);
            interval.tick().await;
            loop {
                interval.tick().await;
                let worker = registry.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    worker
                        .last_artifact_cleanup_ms
                        .store(now_ms(), Ordering::Relaxed);
                    let Some(directories) = &worker.artifacts else {
                        return;
                    };
                    if let Err(error) = worker.cleanup_artifacts_at(
                        &directories.uploads,
                        &directories.exports,
                        now_ms(),
                        ttl,
                    ) {
                        eprintln!("WARNING: {error}");
                    }
                })
                .await;
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn start_artifact_cleanup_task_for_test(&self, every: Duration, ttl: ArtifactTtl) {
        self.start_artifact_cleanup_task_with(every, ttl);
    }

    pub(crate) fn cleanup_artifacts_at(
        &self,
        uploads_dir: &Path,
        exports_dir: &Path,
        current_ms: u64,
        ttl: ArtifactTtl,
    ) -> Result<ArtifactCleanupReport, String> {
        let snapshots = self.list();
        let mut report = ArtifactCleanupReport::default();
        cleanup_artifact_directory(
            uploads_dir,
            ArtifactKind::Upload,
            &snapshots,
            current_ms,
            ttl.uploads_ms,
            &mut report,
        )?;
        cleanup_artifact_directory(
            exports_dir,
            ArtifactKind::Export,
            &snapshots,
            current_ms,
            ttl.exports_ms,
            &mut report,
        )?;
        Ok(report)
    }
}

pub(crate) struct JobAdmission {
    id: u64,
    registry: JobRegistry,
    kind: JobKind,
    owner_user_id: String,
    consumed: bool,
}

impl Drop for JobAdmission {
    fn drop(&mut self) {
        if !self.consumed {
            self.registry
                .inner
                .lock()
                .unwrap()
                .reservations
                .remove(&self.id);
        }
    }
}

pub(crate) struct PreviewPermit {
    registry: JobRegistry,
    released: bool,
}

impl Drop for PreviewPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut state = self.registry.inner.lock().unwrap();
        state.active_previews = state.active_previews.saturating_sub(1);
        self.released = true;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    Upload,
    Export,
}

struct ArtifactReference {
    token: String,
    active: bool,
    updated_ms: u64,
}

fn artifact_references(kind: ArtifactKind, snapshots: &[JobSnapshot]) -> Vec<ArtifactReference> {
    snapshots
        .iter()
        .filter_map(|snapshot| {
            let token = match kind {
                ArtifactKind::Upload if snapshot.kind == JobKind::Import => snapshot
                    .input
                    .as_ref()
                    .and_then(|input| input.get("artifact_token"))
                    .and_then(Value::as_str),
                ArtifactKind::Export if snapshot.kind == JobKind::Export => snapshot
                    .result
                    .as_ref()
                    .and_then(|result| result.get("token"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        snapshot
                            .input
                            .as_ref()
                            .and_then(|input| input.get("artifact_token"))
                            .and_then(Value::as_str)
                    }),
                _ => None,
            }?;
            Some(ArtifactReference {
                token: token.to_string(),
                active: !snapshot.status.is_terminal(),
                updated_ms: snapshot.updated_ms,
            })
        })
        .collect()
}

fn artifact_name_matches(kind: ArtifactKind, name: &str, token: &str) -> bool {
    match kind {
        ArtifactKind::Upload => {
            name == token
                || name
                    .strip_prefix(token)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        }
        ArtifactKind::Export => name == token,
    }
}

fn cleanup_artifact_directory(
    directory: &Path,
    kind: ArtifactKind,
    snapshots: &[JobSnapshot],
    current_ms: u64,
    ttl_ms: u64,
    report: &mut ArtifactCleanupReport,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "read artifact directory {}: {error}",
                directory.display()
            ));
        }
    };
    let references = artifact_references(kind, snapshots);
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("read artifact entry in {}: {error}", directory.display()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect artifact {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let matching = references
            .iter()
            .filter(|reference| artifact_name_matches(kind, &name, &reference.token))
            .collect::<Vec<_>>();
        if matching.iter().any(|reference| reference.active) {
            report.skipped_active += 1;
            continue;
        }
        let reference_time = matching
            .iter()
            .map(|reference| reference.updated_ms)
            .max()
            .or_else(|| {
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as u64)
            })
            .unwrap_or(current_ms);
        if current_ms.saturating_sub(reference_time) < ttl_ms {
            continue;
        }
        if metadata.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .map_err(|error| format!("remove expired artifact {}: {error}", path.display()))?;
        match kind {
            ArtifactKind::Upload => report.removed_uploads += 1,
            ArtifactKind::Export => report.removed_exports += 1,
        }
    }
    Ok(())
}

fn can_view(identity: &Identity, snapshot: &JobSnapshot) -> bool {
    identity.role.is_privileged()
        || snapshot.owner_user_id == identity.user_id
        || snapshot.visibility == JobVisibility::Workspace
}

fn redact_for(identity: &Identity, mut snapshot: JobSnapshot) -> JobSnapshot {
    if identity.role == Role::Viewer
        && snapshot.kind == JobKind::Import
        && snapshot.visibility == JobVisibility::Workspace
    {
        snapshot.title = "Import".to_string();
        snapshot.input = None;
        snapshot.result = None;
        snapshot.message = Some(
            match snapshot.status {
                JobStatus::Queued => "Import is waiting to start.",
                JobStatus::Running => "Import is in progress.",
                JobStatus::Succeeded => "Import completed.",
                JobStatus::Failed => "Import failed. Ask an editor or administrator for details.",
                JobStatus::Cancelled => "Import was cancelled.",
            }
            .to_string(),
        );
        if snapshot.error.is_some() {
            snapshot.error =
                Some("Import failed. Ask an editor or administrator for details.".to_string());
        }
        snapshot.non_cancellable_reason = None;
    }
    snapshot
}

/// Worker-side handle to publish progress and outcome for one job.
#[derive(Clone)]
pub struct JobHandle {
    id: u64,
    registry: JobRegistry,
    cancel: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Option<JobSnapshot> {
        self.registry.snapshot(self.id)
    }

    /// Atomically crosses the point after which the underlying operation can
    /// no longer be stopped safely. Returns false when cancellation won the
    /// race, so the caller must not begin the mutation.
    pub fn enter_non_cancellable(&self, phase: impl Into<String>) -> bool {
        let phase = phase.into();
        let (allowed, snapshot) = {
            let mut state = self.registry.inner.lock().unwrap();
            let Some(entry) = state.entries.get_mut(&self.id) else {
                return false;
            };
            if entry.snapshot.status.is_terminal() {
                return false;
            }
            if entry.cancel.load(Ordering::SeqCst) || entry.snapshot.cancel_requested {
                entry.snapshot.status = JobStatus::Cancelled;
                entry.snapshot.cancellable = false;
                entry.snapshot.cancel_requested = true;
                entry.snapshot.updated_ms = now_ms();
                (false, entry.snapshot.clone())
            } else {
                entry.snapshot.cancellable = false;
                entry.snapshot.cancel_requested = false;
                entry.snapshot.non_cancellable_reason = Some(phase.clone());
                entry.snapshot.progress.phase = phase;
                entry.snapshot.updated_ms = now_ms();
                (true, entry.snapshot.clone())
            }
        };
        self.registry.persist(&snapshot);
        allowed
    }

    #[cfg(test)]
    pub fn set_running(&self) {
        self.registry.mutate(self.id, |snapshot| {
            snapshot.status = JobStatus::Running;
        });
    }

    pub fn set_phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.registry.mutate(self.id, |snapshot| {
            snapshot.progress.phase = phase;
        });
    }

    pub fn set_progress(&self, phase: &str, done: u64, total: u64) {
        self.registry.mutate(self.id, |snapshot| {
            snapshot.progress.phase = phase.to_string();
            snapshot.progress.done = done;
            snapshot.progress.total = total;
            snapshot.progress.percent = if total > 0 {
                (done as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
        });
    }

    pub fn set_result(&self, result: Value) {
        self.registry.mutate(self.id, |snapshot| {
            snapshot.result = Some(result);
        });
    }

    pub fn set_message(&self, message: impl Into<String>) {
        let message = message.into();
        self.registry.mutate(self.id, |snapshot| {
            snapshot.message = Some(message);
        });
    }

    pub fn succeed(&self, result: Option<Value>) {
        let cancel_requested = self.cancel.load(Ordering::SeqCst);
        self.registry.mutate(self.id, |snapshot| {
            snapshot.status = if cancel_requested && snapshot.cancel_requested {
                JobStatus::Cancelled
            } else {
                JobStatus::Succeeded
            };
            snapshot.cancellable = false;
            if snapshot.status == JobStatus::Succeeded {
                snapshot.result = result;
                snapshot.progress.percent = 100.0;
            }
        });
        self.registry.prune(JOB_HISTORY_LIMIT);
    }

    pub fn fail(&self, error: impl Into<String>) {
        let error = error.into();
        self.registry.mutate(self.id, |snapshot| {
            snapshot.status = JobStatus::Failed;
            snapshot.cancellable = false;
            snapshot.error = Some(error);
        });
        self.registry.prune(JOB_HISTORY_LIMIT);
    }

    pub fn mark_cancelled(&self) {
        self.registry.mutate(self.id, |snapshot| {
            snapshot.status = JobStatus::Cancelled;
            snapshot.cancellable = false;
            snapshot.cancel_requested = true;
        });
        self.registry.prune(JOB_HISTORY_LIMIT);
    }
}

/// Queues `work` for a dedicated worker thread. Write jobs are FIFO and run one
/// at a time; read jobs are FIFO and run up to the registry's fixed limit.
#[cfg(test)]
pub fn spawn_job<F>(
    registry: &JobRegistry,
    kind: JobKind,
    title: impl Into<String>,
    work: F,
) -> Result<JobSnapshot, JobCreateError>
where
    F: FnOnce(JobHandle) + Send + 'static,
{
    let admission = registry.reserve_for(&Identity::local_owner(), kind)?;
    registry.enqueue_admitted(admission, title, JobVisibility::Private, None, work)
}

pub fn spawn_job_for<F>(
    registry: &JobRegistry,
    identity: &Identity,
    kind: JobKind,
    visibility: JobVisibility,
    title: impl Into<String>,
    work: F,
) -> Result<JobSnapshot, JobCreateError>
where
    F: FnOnce(JobHandle) + Send + 'static,
{
    spawn_job_for_with_input(registry, identity, kind, visibility, title, None, work)
}

pub fn spawn_job_for_with_input<F>(
    registry: &JobRegistry,
    identity: &Identity,
    kind: JobKind,
    visibility: JobVisibility,
    title: impl Into<String>,
    input: Option<Value>,
    work: F,
) -> Result<JobSnapshot, JobCreateError>
where
    F: FnOnce(JobHandle) + Send + 'static,
{
    let admission = registry.reserve_for(identity, kind)?;
    spawn_job_with_admission(registry, admission, visibility, title, input, work)
}

pub(crate) fn spawn_job_with_admission<F>(
    registry: &JobRegistry,
    admission: JobAdmission,
    visibility: JobVisibility,
    title: impl Into<String>,
    input: Option<Value>,
    work: F,
) -> Result<JobSnapshot, JobCreateError>
where
    F: FnOnce(JobHandle) + Send + 'static,
{
    registry.enqueue_admitted(admission, title, visibility, input, work)
}
