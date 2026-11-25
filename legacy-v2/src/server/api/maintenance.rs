//! Database statistics and maintenance jobs (optimize, reindex, clear).

use axum::Json;
use axum::extract::{Extension, State};
use serde::Serialize;

use crate::db::Db;
use crate::server::auth::Identity;
use crate::server::dto::StorageDto;
use crate::server::error::{ApiError, blocking};
use crate::server::jobs::{JobKind, JobSnapshot, JobVisibility, spawn_job_for};
use crate::server::state::AppState;

#[derive(Serialize)]
pub struct StatsDto {
    total_rows: u64,
    unindexed_rows: u64,
    has_shape: bool,
    import_count: usize,
    last_import: Option<String>,
    storage: StorageDto,
}

pub async fn stats(State(state): State<AppState>) -> Result<Json<StatsDto>, ApiError> {
    let dto = blocking("database stats", move || {
        let db = state.open_read()?;
        let storage = db
            .storage_info(state.db_path())
            .map_err(|err| ApiError::internal("storage info", err))?;
        let log = db.import_log(500);
        Ok(StatsDto {
            total_rows: db.total_rows(),
            unindexed_rows: db.unindexed_rows(),
            has_shape: db.table_shape().is_some(),
            import_count: log.len(),
            last_import: log.first().map(|entry| entry.imported_at.clone()),
            storage: StorageDto::from(&storage),
        })
    })
    .await?;
    Ok(Json(dto))
}

fn spawn_write<F>(
    state: &AppState,
    identity: &Identity,
    kind: JobKind,
    title: &str,
    work: F,
) -> Result<Json<JobSnapshot>, ApiError>
where
    F: FnOnce(crate::server::jobs::JobHandle) + Send + 'static,
{
    spawn_job_for(
        &state.jobs,
        identity,
        kind,
        JobVisibility::Workspace,
        title,
        work,
    )
    .map(Json)
    .map_err(|error| {
        use crate::server::jobs::JobCreateError;
        match error {
            JobCreateError::Forbidden => ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "forbidden",
                "Your account cannot run database maintenance.",
            ),
            JobCreateError::UserQueueFull => ApiError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "user_job_queue_full",
                "You already have the maximum number of pending jobs.",
            ),
            JobCreateError::WorkspaceQueueFull => ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "workspace_job_queue_full",
                "The workspace job queue is full. Try again after another job finishes.",
            ),
            JobCreateError::MaintenanceBusy => ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "maintenance_busy",
                "This maintenance operation is already pending or running.",
            ),
        }
    })
}

pub async fn optimize(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let db_path = state.db_path.clone();
    spawn_write(
        &state,
        &identity,
        JobKind::Optimize,
        "Optimizing the database",
        move |handle| {
            handle.set_phase("preparing");
            let db = match Db::open(&db_path) {
                Ok(db) => db,
                Err(err) => return handle.fail(err),
            };
            if !handle.enter_non_cancellable("checkpointing") {
                handle.mark_cancelled();
                return;
            }
            match db.checkpoint_wal_truncate() {
                Ok(info) => {
                    handle.set_message(format!(
                        "WAL checkpoint complete: {} log frames, {} checkpointed.",
                        info.log_frames, info.checkpointed_frames
                    ));
                    handle.succeed(None);
                }
                Err(err) => handle.fail(err.to_string()),
            }
        },
    )
}

pub async fn compact(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let db_path = state.db_path.clone();
    spawn_write(
        &state,
        &identity,
        JobKind::Compact,
        "Compacting the database (VACUUM)",
        move |handle| {
            handle.set_phase("preparing");
            let db = match Db::open(&db_path) {
                Ok(db) => db,
                Err(err) => return handle.fail(err),
            };
            if !handle.enter_non_cancellable("compacting") {
                handle.mark_cancelled();
                return;
            }
            match db.vacuum_database() {
                Ok(()) => {
                    handle.set_message("Database compacted. Free space returned to disk.");
                    handle.succeed(None);
                }
                Err(err) => handle.fail(err.to_string()),
            }
        },
    )
}

pub async fn reindex(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let db_path = state.db_path.clone();
    spawn_write(
        &state,
        &identity,
        JobKind::Reindex,
        "Rebuilding the search index",
        move |handle| {
            let mut db = match Db::open(&db_path) {
                Ok(db) => db,
                Err(err) => return handle.fail(err),
            };
            let cancel = handle.cancel_flag();
            match db.index_fts(&cancel, |done, total| {
                handle.set_progress("indexing", done, total)
            }) {
                Ok(_) if handle.is_cancelled() => handle.mark_cancelled(),
                Ok((indexed, _)) => {
                    handle.set_message(format!("Indexed {indexed} rows."));
                    handle.succeed(None);
                }
                Err(err) => handle.fail(err.to_string()),
            }
        },
    )
}

/// Rebuilds the optional DuckDB analytical projection. Only available when the
/// binary was compiled with the `duckdb-olap` feature.
#[cfg(feature = "duckdb-olap")]
pub async fn olap(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let db_path = state.db_path.clone();
    spawn_write(
        &state,
        &identity,
        JobKind::OlapBuild,
        "Building the DuckDB projection",
        move |handle| {
            if !handle.enter_non_cancellable("building projection") {
                handle.mark_cancelled();
                return;
            }
            let projection = crate::duckdb_olap::default_projection_path(&db_path);
            match crate::duckdb_olap::build_projection_atomic(&db_path, &projection) {
                Ok(build) => {
                    handle.set_message(format!(
                        "Projection built: {} rows in {:.1}s.",
                        build.rows,
                        build.elapsed_ms / 1000.0
                    ));
                    handle.succeed(Some(serde_json::json!({
                        "rows": build.rows,
                        "path": build.projection_path.display().to_string(),
                    })));
                }
                Err(err) => handle.fail(err),
            }
        },
    )
}

#[cfg(not(feature = "duckdb-olap"))]
pub async fn olap(
    State(_state): State<AppState>,
    Extension(_identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    Err(ApiError::unsupported(
        "The DuckDB analytical projection is not available in this build.",
    ))
}

pub async fn clear(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let db_path = state.db_path.clone();
    spawn_write(
        &state,
        &identity,
        JobKind::Clear,
        "Clearing the database",
        move |handle| {
            handle.set_phase("preparing");
            let mut db = match Db::open(&db_path) {
                Ok(db) => db,
                Err(err) => return handle.fail(err),
            };
            if !handle.enter_non_cancellable("clearing") {
                handle.mark_cancelled();
                return;
            }
            match db.clear_all() {
                Ok(()) => {
                    handle.set_message("Database cleared. All records and history removed.");
                    handle.succeed(None);
                }
                Err(err) => handle.fail(err.to_string()),
            }
        },
    )
}
