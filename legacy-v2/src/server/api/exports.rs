//! Export: build a CSV/XLSX of the current query as a background job, then
//! stream the finished file to the browser for download.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;

use crate::db::{Db, Query, ResultSort};
use crate::export::{self, ExportError};
use crate::server::auth::Identity;
use crate::server::error::{ApiError, blocking};
use crate::server::jobs::{
    JobCreateError, JobHandle, JobKind, JobSnapshot, JobVisibility, spawn_job_with_admission,
};
use crate::server::sanitize_file_name;
use crate::server::state::AppState;

fn default_format() -> String {
    "csv".to_string()
}

#[derive(Deserialize)]
pub struct ExportRequest {
    #[serde(default)]
    query: Query,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    field_ids: Option<Vec<String>>,
    #[serde(default)]
    sort: Option<ResultSort>,
}

fn export_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn export_admission_error(error: JobCreateError) -> ApiError {
    match error {
        JobCreateError::Forbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Your account cannot export data.",
        ),
        JobCreateError::UserQueueFull => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "user_job_queue_full",
            "You already have the maximum number of pending jobs.",
        ),
        JobCreateError::WorkspaceQueueFull => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_job_queue_full",
            "The workspace job queue is full. Try again after another job finishes.",
        ),
        JobCreateError::MaintenanceBusy => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "maintenance_busy",
            "Database maintenance is already pending or running.",
        ),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<ExportRequest>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let admission = state
        .jobs
        .reserve_for(&identity, JobKind::Export)
        .map_err(export_admission_error)?;
    let ext = match req.format.to_lowercase().as_str() {
        "csv" => "csv",
        "xlsx" => "xlsx",
        other => {
            return Err(ApiError::unsupported(format!(
                "Unsupported export format: {other}. Use csv or xlsx."
            )));
        }
    };

    let base = req
        .filename
        .as_deref()
        .map(sanitize_file_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "base-search-export".to_string());
    let file_name = if base.to_lowercase().ends_with(&format!(".{ext}")) {
        base
    } else {
        // Drop any other extension the user typed, then apply the chosen one.
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(&base);
        format!("{stem}.{ext}")
    };

    let requested_field_ids = req.field_ids;
    let sort = req.sort;
    let validation_state = state.clone();
    let validation_sort = sort.clone();
    let fields = blocking("validate export request", move || {
        let db = validation_state.open_read()?;
        let catalog = db.result_fields_cached();
        let fields = export::resolve_fields(&catalog, requested_field_ids.as_deref())
            .map_err(export_request_error)?;
        export::validate_sort(&catalog, validation_sort.as_ref()).map_err(export_request_error)?;
        Ok(fields)
    })
    .await?;

    let token = export_token();
    let subdir = state.exports_dir.join(&token);
    tokio::fs::create_dir_all(&subdir)
        .await
        .map_err(|err| ApiError::internal("create export dir", err))?;
    let dest = subdir.join(&file_name);

    let title = format!("Exporting {file_name}");
    let query = req.query;
    let job_state = state.clone();
    let job_file_name = file_name.clone();
    let job = ExportJobSpec {
        state: job_state,
        query,
        dest,
        file_name: job_file_name,
        token: token.clone(),
        format: ext.to_string(),
        fields,
        sort,
    };
    let input = Some(json!({
        "artifact_token": token,
        "file_name": file_name.clone(),
        "format": ext,
    }));
    match spawn_job_with_admission(
        &state.jobs,
        admission,
        JobVisibility::Private,
        title,
        input,
        move |handle| run_export(job, handle),
    ) {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&subdir).await;
            Err(export_admission_error(error))
        }
    }
}

struct ExportJobSpec {
    state: AppState,
    query: Query,
    dest: PathBuf,
    file_name: String,
    token: String,
    format: String,
    fields: Vec<crate::search::FieldInfo>,
    sort: Option<ResultSort>,
}

fn run_export(job: ExportJobSpec, handle: JobHandle) {
    let db = match Db::open_runtime(&job.state.db_path) {
        Ok(db) => db,
        Err(err) => {
            handle.fail(err);
            return;
        }
    };
    let cancel = handle.cancel_flag();
    let result = export::export_selected(
        &db,
        &job.query,
        &job.dest,
        &job.fields,
        job.sort.as_ref(),
        &cancel,
        |done, total| handle.set_progress("writing", done, total),
    );

    match result {
        Ok(written) => {
            let bytes = std::fs::metadata(&job.dest).map(|m| m.len()).unwrap_or(0);
            let field_snapshot = job
                .fields
                .iter()
                .map(|field| json!({ "id": field.id, "label": field.label }))
                .collect::<Vec<_>>();
            let record_scope = job.query.record_scope;
            handle.set_message(format!("Exported {written} rows"));
            handle.succeed(Some(json!({
                "file_name": job.file_name,
                "token": job.token,
                "rows": written,
                "count": written,
                "bytes": bytes,
                "download_url": format!("/api/v2/export/{}/download", handle.id()),
                "fields": field_snapshot,
                "field_ids": job.fields.iter().map(|field| field.id.clone()).collect::<Vec<_>>(),
                "sort": job.sort,
                "query": job.query,
                "record_scope": record_scope,
                "format": job.format,
            })));
        }
        Err(ExportError::Cancelled) => {
            let _ = std::fs::remove_dir_all(job.dest.parent().unwrap_or(&job.dest));
            handle.mark_cancelled();
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(job.dest.parent().unwrap_or(&job.dest));
            handle.fail(export_error_message(err));
        }
    }
}

fn export_error_message(err: ExportError) -> String {
    match err {
        ExportError::TooManyRowsForXlsx(rows) => {
            format!("{rows} rows exceed the Excel worksheet limit. Export CSV instead.")
        }
        ExportError::UnsupportedExtension(ext) if ext.is_empty() => {
            "Unsupported export extension. Use .csv or .xlsx.".to_string()
        }
        ExportError::UnsupportedExtension(ext) => {
            format!("Unsupported export extension: .{ext}. Use .csv or .xlsx.")
        }
        ExportError::Cancelled => "Export cancelled.".to_string(),
        ExportError::Other(message) => message,
    }
}

fn export_request_error(err: export::ExportSelectionError) -> ApiError {
    ApiError::bad_request(err.to_string())
}

pub async fn download(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<u64>,
) -> Result<Response, ApiError> {
    let snapshot = state
        .jobs
        .snapshot_for(&identity, id)
        .filter(|snapshot| snapshot.kind == JobKind::Export)
        .ok_or_else(|| ApiError::not_found(format!("No export job with id {id}")))?;
    let result = snapshot
        .result
        .ok_or_else(|| ApiError::conflict("The export is not finished yet."))?;

    let file_name = str_field(&result, "file_name")
        .ok_or_else(|| ApiError::internal("export result", "missing file_name"))?;
    let token = str_field(&result, "token")
        .ok_or_else(|| ApiError::internal("export result", "missing token"))?;

    let path = state.exports_dir.join(&token).join(&file_name);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("The export file is no longer available."))?;

    let content_type = if file_name.to_lowercase().ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else {
        "text/csv; charset=utf-8"
    };
    let disposition = format!("attachment; filename=\"{file_name}\"");
    let body = Body::from_stream(ReaderStream::new(file));

    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
