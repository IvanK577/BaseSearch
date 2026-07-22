//! Import: multipart upload of spreadsheet and delimited files, run as a background job so the UI
//! stays responsive, plus the import history log.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Extension, Multipart, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::{
    Db, SourceMappingColumn, SourceMappingProfileCollection, source_mapping_signature,
};
use crate::domain::table::{ColumnRole, ColumnStorage, SemanticField, SourceColumn, TableShape};
use crate::import::{self, ImportPhase};
use crate::server::auth::Identity;
use crate::server::dto::{ImportFileResultDto, ImportLogDto};
use crate::server::error::{ApiError, blocking};
use crate::server::jobs::{
    JobCreateError, JobHandle, JobKind, JobSnapshot, JobVisibility, PreviewAdmissionError,
    spawn_job_with_admission,
};
use crate::server::sanitize_file_name;
use crate::server::state::AppState;

const SUPPORTED_EXTENSIONS: [&str; 7] = ["xlsx", "xlsb", "xls", "xlsm", "ods", "csv", "tsv"];
const MAX_UPLOAD_FILES: usize = 32;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const MULTIPART_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SHEET_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_SEMANTIC_OPTIONS_BYTES: usize = 256 * 1024;
const MAX_PROFILE_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_FIXED_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_SELECTED_SHEETS: usize = 256;
const MAX_SEMANTIC_OVERRIDES: usize = 4096;
pub(super) const MAX_FILE_BODY_BYTES: u64 = MAX_FILE_BYTES + MULTIPART_OVERHEAD_BYTES;
pub(super) const MAX_BATCH_BODY_BYTES: u64 = MAX_BATCH_BYTES + MULTIPART_OVERHEAD_BYTES;

fn import_admission_error(error: JobCreateError) -> ApiError {
    match error {
        JobCreateError::Forbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Your account cannot import files.",
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

#[derive(Clone, Debug, Serialize)]
struct AppliedSourceProfile {
    id: i64,
    name: String,
    signature: String,
}

#[derive(Debug, Serialize)]
pub struct WorkbookPeekResponse {
    sheets: Vec<SheetPeekResponse>,
}

#[derive(Debug, Serialize)]
struct SheetPeekResponse {
    #[serde(flatten)]
    sheet: import::SheetPeek,
    signature: String,
    profile_suggestions: SourceMappingProfileCollection,
}

#[derive(Default)]
struct ResolvedImportConfiguration {
    sheet_semantics: BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>,
    sheet_fixed_values: BTreeMap<String, BTreeMap<SemanticField, String>>,
    applied_profiles: BTreeMap<String, AppliedSourceProfile>,
}

fn is_supported(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn upload_stamp() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// Removes each uploaded file's private subdirectory.
fn cleanup(files: &[PathBuf]) {
    for file in files {
        if let Some(dir) = file.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[derive(Default)]
struct UploadCleanupGuard {
    files: Vec<PathBuf>,
    armed: bool,
}

impl UploadCleanupGuard {
    fn new() -> Self {
        Self {
            files: Vec::new(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cleanup_on_error<T>(&self, result: Result<T, ApiError>) -> Result<T, ApiError> {
        if result.is_err() {
            cleanup(&self.files);
        }
        result
    }
}

impl std::ops::Deref for UploadCleanupGuard {
    type Target = Vec<PathBuf>;

    fn deref(&self) -> &Self::Target {
        &self.files
    }
}

impl std::ops::DerefMut for UploadCleanupGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.files
    }
}

impl Drop for UploadCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            cleanup(&self.files);
        }
    }
}

fn ensure_upload_space(directory: &std::path::Path, incoming_bytes: u64) -> Result<(), ApiError> {
    let available = fs2::available_space(directory)
        .map_err(|err| ApiError::internal("check free disk space", err))?;
    if available < incoming_bytes.saturating_add(MIN_FREE_BYTES) {
        return Err(ApiError::insufficient_storage(format!(
            "Not enough free disk space to receive this upload. Free at least {} MB and try again.",
            (incoming_bytes.saturating_add(MIN_FREE_BYTES) - available).div_ceil(1024 * 1024)
        )));
    }
    Ok(())
}

async fn read_bounded_metadata(
    field: &mut axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    read_context: &str,
    limit_message: &str,
) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    loop {
        let chunk = field
            .chunk()
            .await
            .map_err(|err| ApiError::bad_request(format!("{read_context}: {err}")))?;
        let Some(chunk) = chunk else {
            return Ok(bytes);
        };
        if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
            return Err(ApiError::payload_too_large(limit_message));
        }
        bytes.extend_from_slice(&chunk);
    }
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    mut multipart: Multipart,
) -> Result<Json<JobSnapshot>, ApiError> {
    let admission = state
        .jobs
        .reserve_for(&identity, JobKind::Import)
        .map_err(import_admission_error)?;
    tokio::fs::create_dir_all(&state.uploads_dir)
        .await
        .map_err(|err| ApiError::internal("create uploads dir", err))?;

    let stamp = upload_stamp();
    // Each upload lands in its own subdirectory under the original file name,
    // so the import log and dedup use the real name and files never collide.
    let mut saved = UploadCleanupGuard::new();
    let mut batch_bytes = 0u64;
    let mut selected_sheets: Option<BTreeSet<String>> = None;
    let mut sheet_semantics: Option<BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>> =
        None;
    let mut sheet_profiles: Option<BTreeMap<String, i64>> = None;
    let mut sheet_fixed_values: Option<BTreeMap<String, BTreeMap<SemanticField, String>>> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Upload failed: {err}")))?
    {
        if field.file_name().is_none() && field.name() == Some("selected_sheets") {
            if selected_sheets.is_some() {
                cleanup(&saved);
                return Err(ApiError::bad_request(
                    "Sheet selection was provided more than once.",
                ));
            }
            let bytes = saved.cleanup_on_error(
                read_bounded_metadata(
                    &mut field,
                    MAX_SHEET_OPTIONS_BYTES,
                    "Read sheet selection",
                    "Sheet selection metadata is too large.",
                )
                .await,
            )?;
            let names: Vec<String> =
                saved
                    .cleanup_on_error(serde_json::from_slice(&bytes).map_err(|_| {
                        ApiError::bad_request("Sheet selection is not valid JSON.")
                    }))?;
            if names.is_empty() || names.len() > MAX_SELECTED_SHEETS {
                cleanup(&saved);
                return Err(ApiError::bad_request(format!(
                    "Select between 1 and {MAX_SELECTED_SHEETS} workbook sheets."
                )));
            }
            let normalized: BTreeSet<String> = names
                .into_iter()
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect();
            if normalized.is_empty() || normalized.iter().any(|name| name.len() > 256) {
                cleanup(&saved);
                return Err(ApiError::bad_request(
                    "Sheet selection contains an invalid name.",
                ));
            }
            selected_sheets = Some(normalized);
            continue;
        }

        if field.file_name().is_none() && field.name() == Some("sheet_semantics") {
            if sheet_semantics.is_some() {
                cleanup(&saved);
                return Err(ApiError::bad_request(
                    "Column mapping was provided more than once.",
                ));
            }
            let bytes = saved.cleanup_on_error(
                read_bounded_metadata(
                    &mut field,
                    MAX_SEMANTIC_OPTIONS_BYTES,
                    "Read column mapping",
                    "Column mapping metadata is too large.",
                )
                .await,
            )?;
            let raw: BTreeMap<String, BTreeMap<usize, Option<SemanticField>>> = saved
                .cleanup_on_error(
                    serde_json::from_slice(&bytes)
                        .map_err(|_| ApiError::bad_request("Column mapping is not valid JSON.")),
                )?;
            let override_count = raw.values().map(BTreeMap::len).sum::<usize>();
            if raw.len() > MAX_SELECTED_SHEETS || override_count > MAX_SEMANTIC_OVERRIDES {
                cleanup(&saved);
                return Err(ApiError::bad_request(format!(
                    "Column mapping supports at most {MAX_SELECTED_SHEETS} sheets and {MAX_SEMANTIC_OVERRIDES} column assignments."
                )));
            }
            let mut normalized = BTreeMap::new();
            for (sheet, semantics) in raw {
                let sheet = sheet.trim().to_string();
                if sheet.is_empty() || sheet.len() > 256 {
                    cleanup(&saved);
                    return Err(ApiError::bad_request(
                        "Column mapping contains an invalid sheet name.",
                    ));
                }
                if semantics.keys().any(|index| *index > 65_535) {
                    cleanup(&saved);
                    return Err(ApiError::bad_request(
                        "Column mapping contains an invalid column index.",
                    ));
                }
                if !semantics.is_empty() {
                    normalized.insert(sheet, semantics);
                }
            }
            sheet_semantics = Some(normalized);
            continue;
        }

        if field.file_name().is_none() && field.name() == Some("sheet_profiles") {
            if sheet_profiles.is_some() {
                cleanup(&saved);
                return Err(ApiError::bad_request(
                    "Source profile selection was provided more than once.",
                ));
            }
            let bytes = saved.cleanup_on_error(
                read_bounded_metadata(
                    &mut field,
                    MAX_PROFILE_OPTIONS_BYTES,
                    "Read source profiles",
                    "Source profile selection metadata is too large.",
                )
                .await,
            )?;
            let raw: BTreeMap<String, i64> =
                saved.cleanup_on_error(serde_json::from_slice(&bytes).map_err(|_| {
                    ApiError::bad_request("Source profile selection is not valid JSON.")
                }))?;
            if raw.len() > MAX_SELECTED_SHEETS {
                cleanup(&saved);
                return Err(ApiError::bad_request(format!(
                    "Select source profiles for at most {MAX_SELECTED_SHEETS} sheets."
                )));
            }
            let mut normalized = BTreeMap::new();
            for (sheet, profile_id) in raw {
                let sheet = sheet.trim().to_string();
                if sheet.is_empty() || sheet.len() > 256 || profile_id <= 0 {
                    cleanup(&saved);
                    return Err(ApiError::bad_request(
                        "Source profile selection contains an invalid sheet or profile id.",
                    ));
                }
                normalized.insert(sheet, profile_id);
            }
            sheet_profiles = Some(normalized);
            continue;
        }

        if field.file_name().is_none() && field.name() == Some("sheet_fixed_values") {
            if sheet_fixed_values.is_some() {
                cleanup(&saved);
                return Err(ApiError::bad_request(
                    "Fixed source values were provided more than once.",
                ));
            }
            let bytes = saved.cleanup_on_error(
                read_bounded_metadata(
                    &mut field,
                    MAX_FIXED_OPTIONS_BYTES,
                    "Read fixed source values",
                    "Fixed source value metadata is too large.",
                )
                .await,
            )?;
            let raw: BTreeMap<String, BTreeMap<SemanticField, String>> = saved
                .cleanup_on_error(serde_json::from_slice(&bytes).map_err(|_| {
                    ApiError::bad_request("Fixed source values are not valid JSON.")
                }))?;
            sheet_fixed_values = Some(saved.cleanup_on_error(validate_sheet_fixed_values(raw))?);
            continue;
        }

        let Some(original) = field.file_name().map(str::to_string) else {
            continue;
        };
        if original.trim().is_empty() {
            continue;
        }
        if saved.len() >= MAX_UPLOAD_FILES {
            cleanup(&saved);
            return Err(ApiError::payload_too_large(format!(
                "Import at most {MAX_UPLOAD_FILES} files at a time."
            )));
        }
        if !is_supported(&original) {
            cleanup(&saved);
            return Err(ApiError::unsupported(format!(
                "Unsupported file type: {original}. Import Excel, OpenDocument, CSV, or TSV files."
            )));
        }
        let subdir = state.uploads_dir.join(format!("{stamp}-{}", saved.len()));
        if let Err(err) = tokio::fs::create_dir_all(&subdir).await {
            cleanup(&saved);
            return Err(ApiError::internal("create upload subdir", err));
        }
        let dest = subdir.join(sanitize_file_name(&original));
        let mut file = match tokio::fs::File::create(&dest).await {
            Ok(file) => file,
            Err(err) => {
                cleanup(&saved);
                return Err(ApiError::internal("create upload file", err));
            }
        };
        use tokio::io::AsyncWriteExt;
        let mut file_bytes = 0u64;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    let chunk_bytes = chunk.len() as u64;
                    if file_bytes.saturating_add(chunk_bytes) > MAX_FILE_BYTES {
                        drop(file);
                        cleanup(&saved);
                        let _ = std::fs::remove_dir_all(&subdir);
                        return Err(ApiError::payload_too_large(format!(
                            "{original} is larger than the 4 GB per-file limit."
                        )));
                    }
                    if batch_bytes.saturating_add(chunk_bytes) > MAX_BATCH_BYTES {
                        drop(file);
                        cleanup(&saved);
                        let _ = std::fs::remove_dir_all(&subdir);
                        return Err(ApiError::payload_too_large(
                            "This upload is larger than the 16 GB batch limit.",
                        ));
                    }
                    if let Err(error) = ensure_upload_space(&state.uploads_dir, chunk_bytes) {
                        drop(file);
                        cleanup(&saved);
                        let _ = std::fs::remove_dir_all(&subdir);
                        return Err(error);
                    }
                    if let Err(err) = file.write_all(&chunk).await {
                        drop(file);
                        cleanup(&saved);
                        let _ = std::fs::remove_dir_all(&subdir);
                        return Err(ApiError::internal("write upload", err));
                    }
                    file_bytes += chunk_bytes;
                    batch_bytes += chunk_bytes;
                }
                Ok(None) => break,
                Err(err) => {
                    drop(file);
                    cleanup(&saved);
                    let _ = std::fs::remove_dir_all(&subdir);
                    return Err(ApiError::bad_request(format!("Upload read failed: {err}")));
                }
            }
        }
        if let Err(err) = file.flush().await {
            drop(file);
            cleanup(&saved);
            let _ = std::fs::remove_dir_all(&subdir);
            return Err(ApiError::internal("flush upload", err));
        }
        saved.push(dest);
    }

    if saved.is_empty() {
        return Err(ApiError::bad_request(
            "No importable files were uploaded. Choose Excel, OpenDocument, CSV, or TSV files.",
        ));
    }
    if (selected_sheets.is_some()
        || sheet_semantics.is_some()
        || sheet_profiles.is_some()
        || sheet_fixed_values.is_some())
        && saved.len() != 1
    {
        cleanup(&saved);
        return Err(ApiError::bad_request(
            "Sheet selection, source profiles, and column mapping can be applied only when importing one file at a time.",
        ));
    }

    let explicit_semantics = sheet_semantics.unwrap_or_default();
    let explicit_fixed_values = sheet_fixed_values.unwrap_or_default();
    let selected_profiles = sheet_profiles.unwrap_or_default();
    let resolved = if saved.len() == 1 && !selected_profiles.is_empty() {
        let db_path = state.db_path.clone();
        let source_path = saved[0].clone();
        let selected_sheets_for_validation = selected_sheets.clone();
        blocking("validate source profiles", move || {
            resolve_import_configuration(
                &db_path,
                &source_path,
                selected_sheets_for_validation.as_ref(),
                selected_profiles,
                explicit_semantics,
                explicit_fixed_values,
            )
        })
        .await
        .inspect_err(|_| cleanup(&saved))?
    } else {
        ResolvedImportConfiguration {
            sheet_semantics: explicit_semantics,
            sheet_fixed_values: explicit_fixed_values,
            applied_profiles: BTreeMap::new(),
        }
    };

    let title = if saved.len() == 1 {
        format!(
            "Importing {}",
            saved[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        )
    } else {
        format!("Importing {} files", saved.len())
    };

    let job_state = state.clone();
    let files = saved.to_vec();
    let sheet_semantics = resolved.sheet_semantics;
    let sheet_fixed_values = resolved.sheet_fixed_values;
    let applied_profiles = resolved.applied_profiles;
    let job_input = json!({
        "artifact_token": stamp,
        "files": saved
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        "selected_sheets": selected_sheets,
        "source_profiles": applied_profiles,
        "sheet_semantics": sheet_semantics,
        "sheet_fixed_values": sheet_fixed_values,
    });
    match spawn_job_with_admission(
        &state.jobs,
        admission,
        JobVisibility::Workspace,
        title,
        Some(job_input),
        move |handle| {
            run_import(
                job_state,
                files,
                selected_sheets,
                sheet_semantics,
                sheet_fixed_values,
                applied_profiles,
                handle,
            )
        },
    ) {
        Ok(snapshot) => {
            saved.disarm();
            Ok(Json(snapshot))
        }
        Err(error) => {
            cleanup(&saved);
            Err(import_admission_error(error))
        }
    }
}

fn run_import(
    state: AppState,
    files: Vec<PathBuf>,
    selected_sheets: Option<BTreeSet<String>>,
    sheet_semantics: BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>,
    sheet_fixed_values: BTreeMap<String, BTreeMap<SemanticField, String>>,
    applied_profiles: BTreeMap<String, AppliedSourceProfile>,
    handle: JobHandle,
) {
    let mut db = match Db::open(&state.db_path) {
        Ok(db) => db,
        Err(err) => {
            cleanup(&files);
            handle.fail(err);
            return;
        }
    };

    let cancel = handle.cancel_flag();
    let file_count = files.len();
    let mut results: Vec<ImportFileResultDto> = Vec::with_capacity(file_count);

    for (idx, path) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let file_label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        handle.set_message(format!("File {}/{}: {file_label}", idx + 1, file_count));

        let options = if file_count == 1 {
            import::ImportOptions {
                selected_sheets: selected_sheets.clone(),
                sheet_semantics: sheet_semantics.clone(),
                sheet_fixed_values: sheet_fixed_values.clone(),
            }
        } else {
            import::ImportOptions::default()
        };
        let summary = import::import_file_with_options(
            &mut db,
            path,
            &options,
            &cancel,
            &mut |phase, done, total| {
                let phase_name = match phase {
                    ImportPhase::Reading => "reading",
                    ImportPhase::Inserting => "inserting",
                    ImportPhase::Indexing => "indexing",
                };
                handle.set_progress(
                    &format!("{phase_name} ({}/{})", idx + 1, file_count),
                    done,
                    total,
                );
            },
        );
        results.push(ImportFileResultDto::from(&summary));
    }

    let total_rows = db.total_rows();
    if results.iter().any(|result| result.error.is_none()) {
        remember_fixed_source_context(&db, &sheet_fixed_values);
    }
    cleanup(&files);

    let imported: u64 = results.iter().map(|r| r.imported).sum();
    let duplicates: u64 = results.iter().map(|r| r.duplicates).sum();
    let failed_files = results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    let payload = json!({
        "files": results,
        "total_rows": total_rows,
        "imported": imported,
        "duplicates": duplicates,
        "source_profiles": applied_profiles,
    });

    if handle.is_cancelled() {
        handle.set_result(payload);
        handle.set_message(format!("Cancelled. Database now holds {total_rows} rows."));
        handle.mark_cancelled();
    } else if failed_files > 0 {
        handle.set_result(payload);
        handle.set_message(format!(
            "Imported {imported} rows, but {failed_files} of {file_count} files failed."
        ));
        handle.fail(format!(
            "{failed_files} of {file_count} files could not be imported. Review the per-file results."
        ));
    } else {
        handle.set_message(format!(
            "Imported {imported} rows ({duplicates} duplicates). Database now holds {total_rows} rows."
        ));
        handle.succeed(Some(payload));
    }
}

fn remember_fixed_source_context(
    db: &Db,
    fixed_values: &BTreeMap<String, BTreeMap<SemanticField, String>>,
) {
    let has_currency = fixed_values
        .values()
        .any(|values| values.contains_key(&SemanticField::Currency));
    let has_weight_unit = fixed_values
        .values()
        .any(|values| values.contains_key(&SemanticField::WeightUnit));
    let mut columns = Vec::with_capacity(2);
    if has_currency {
        columns.push(SourceColumn {
            id: "fixed_currency".to_string(),
            header: "Currency".to_string(),
            source_index: 0,
            role: ColumnRole::Text,
            semantic: Some(SemanticField::Currency),
            storage: ColumnStorage::SchemaColumn("contract".to_string()),
        });
    }
    if has_weight_unit {
        columns.push(SourceColumn {
            id: "fixed_weight_unit".to_string(),
            header: "Weight unit".to_string(),
            source_index: 1,
            role: ColumnRole::Text,
            semantic: Some(SemanticField::WeightUnit),
            storage: ColumnStorage::SchemaColumn("unit".to_string()),
        });
    }
    if !columns.is_empty() {
        db.remember_table_shape(&TableShape { columns });
    }
}

/// Previews the structure of one uploaded spreadsheet (sheets, columns, a
/// sample row) without importing it. The file is written to a temp path, read
/// once, and deleted immediately.
pub async fn peek(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<WorkbookPeekResponse>, ApiError> {
    let _preview = state.jobs.acquire_preview().map_err(|error| match error {
        PreviewAdmissionError::Busy => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "preview_busy",
            "Another file preview is already using the available preview capacity.",
        ),
    })?;
    tokio::fs::create_dir_all(&state.uploads_dir)
        .await
        .map_err(|err| ApiError::internal("create uploads dir", err))?;

    let mut dest: Option<(PathBuf, String)> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Upload failed: {err}")))?
    {
        let Some(original) = field.file_name().map(str::to_string) else {
            continue;
        };
        if original.trim().is_empty() {
            continue;
        }
        if !is_supported(&original) {
            return Err(ApiError::unsupported(format!(
                "Unsupported file type: {original}. Preview Excel, OpenDocument, CSV, or TSV files."
            )));
        }
        let path = state.uploads_dir.join(format!(
            "peek-{}-{}",
            upload_stamp(),
            sanitize_file_name(&original)
        ));
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|err| ApiError::internal("create peek file", err))?;
        use tokio::io::AsyncWriteExt;
        let mut file_bytes = 0u64;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    let chunk_bytes = chunk.len() as u64;
                    if file_bytes.saturating_add(chunk_bytes) > MAX_FILE_BYTES {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(ApiError::payload_too_large(format!(
                            "{original} is larger than the 4 GB preview limit."
                        )));
                    }
                    if let Err(error) = ensure_upload_space(&state.uploads_dir, chunk_bytes) {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(error);
                    }
                    if let Err(err) = file.write_all(&chunk).await {
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(ApiError::internal("write peek", err));
                    }
                    file_bytes += chunk_bytes;
                }
                Ok(None) => break,
                Err(err) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(ApiError::bad_request(format!("Upload read failed: {err}")));
                }
            }
        }
        if let Err(err) = file.flush().await {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::internal("flush peek upload", err));
        }
        dest = Some((path, sanitize_file_name(&original)));
        break; // Only the first file is previewed.
    }

    let Some((path, source_name)) = dest else {
        return Err(ApiError::bad_request(
            "No file to preview. Choose a spreadsheet first.",
        ));
    };
    let peek_path = path.clone();
    let db_path = state.db_path.clone();
    let result = blocking("peek", move || {
        let mut peek =
            import::peek_file(&peek_path, MAX_SELECTED_SHEETS).map_err(ApiError::bad_request)?;
        if is_delimited_name(&source_name) && peek.sheets.len() == 1 {
            peek.sheets[0].name.clone_from(&source_name);
        }
        let db = Db::open_runtime(&db_path)
            .map_err(|error| ApiError::internal("open source profile database", error))?;
        workbook_peek_response(&db, peek)
    })
    .await;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(Json(result?))
}

fn is_delimited_name(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "csv" | "tsv"))
}

fn sheet_signature(sheet: &import::SheetPeek) -> String {
    source_mapping_signature(
        &sheet
            .columns
            .iter()
            .map(|column| SourceMappingColumn {
                header: column.header.clone(),
                role: column.role,
            })
            .collect::<Vec<_>>(),
    )
}

fn workbook_peek_response(
    db: &Db,
    peek: import::WorkbookPeek,
) -> Result<WorkbookPeekResponse, ApiError> {
    let mut sheets = Vec::with_capacity(peek.sheets.len());
    for sheet in peek.sheets {
        let signature = sheet_signature(&sheet);
        let profile_suggestions = db
            .suggest_source_mapping_profiles(&signature)
            .map_err(super::source_profiles::profile_error)?;
        sheets.push(SheetPeekResponse {
            sheet,
            signature,
            profile_suggestions,
        });
    }
    Ok(WorkbookPeekResponse { sheets })
}

fn validate_sheet_fixed_values(
    raw: BTreeMap<String, BTreeMap<SemanticField, String>>,
) -> Result<BTreeMap<String, BTreeMap<SemanticField, String>>, ApiError> {
    if raw.len() > MAX_SELECTED_SHEETS {
        return Err(ApiError::bad_request(format!(
            "Fixed values support at most {MAX_SELECTED_SHEETS} sheets."
        )));
    }
    let mut validated = BTreeMap::new();
    for (sheet, values) in raw {
        let sheet = sheet.trim().to_string();
        if sheet.is_empty() || sheet.len() > 256 || values.len() > 2 {
            return Err(ApiError::bad_request(
                "Fixed source values contain an invalid sheet or too many values.",
            ));
        }
        let mut sheet_values = BTreeMap::new();
        for (semantic, value) in values {
            if !matches!(
                semantic,
                SemanticField::Currency | SemanticField::WeightUnit
            ) {
                return Err(ApiError::bad_request(
                    "Only Currency and WeightUnit can be fixed source values.",
                ));
            }
            let value = value.trim();
            if value.is_empty() || value.chars().count() > 32 {
                return Err(ApiError::bad_request(
                    "Fixed source values must contain between 1 and 32 characters.",
                ));
            }
            sheet_values.insert(semantic, value.to_string());
        }
        if !sheet_values.is_empty() {
            validated.insert(sheet, sheet_values);
        }
    }
    Ok(validated)
}

fn resolve_import_configuration(
    db_path: &std::path::Path,
    source_path: &std::path::Path,
    selected_sheets: Option<&BTreeSet<String>>,
    selected_profiles: BTreeMap<String, i64>,
    explicit_semantics: BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>,
    explicit_fixed_values: BTreeMap<String, BTreeMap<SemanticField, String>>,
) -> Result<ResolvedImportConfiguration, ApiError> {
    let peek =
        import::peek_file(source_path, MAX_SELECTED_SHEETS).map_err(ApiError::bad_request)?;
    let actual = peek
        .sheets
        .iter()
        .map(|sheet| {
            (
                sheet.name.clone(),
                (sheet_signature(sheet), sheet.columns.len()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let db = Db::open_runtime(db_path)
        .map_err(|error| ApiError::internal("open source profile database", error))?;
    let mut resolved = ResolvedImportConfiguration {
        sheet_semantics: BTreeMap::new(),
        sheet_fixed_values: BTreeMap::new(),
        applied_profiles: BTreeMap::new(),
    };

    for (sheet, profile_id) in selected_profiles {
        if selected_sheets.is_some_and(|selected| !selected.contains(&sheet)) {
            return Err(ApiError::bad_request(format!(
                "Source profile for '{sheet}' cannot be applied because that sheet is not selected."
            )));
        }
        let Some((actual_signature, width)) = actual.get(&sheet) else {
            return Err(ApiError::bad_request(format!(
                "Source profile refers to a sheet named '{sheet}', but that sheet was not detected."
            )));
        };
        let profile = db
            .get_source_mapping_profile(profile_id)
            .map_err(super::source_profiles::profile_error)?
            .ok_or_else(|| ApiError::not_found("Selected source mapping profile was not found."))?;
        if profile.signature != *actual_signature || profile.mapping.len() != *width {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "profile_signature_mismatch",
                format!(
                    "The selected profile '{}' does not match the current columns in sheet '{sheet}'. Preview the file again or choose another profile.",
                    profile.name
                ),
            ));
        }
        resolved.sheet_semantics.insert(
            sheet.clone(),
            profile.mapping.iter().copied().enumerate().collect(),
        );
        if !profile.fixed_values.is_empty() {
            resolved
                .sheet_fixed_values
                .insert(sheet.clone(), profile.fixed_values.clone());
        }
        resolved.applied_profiles.insert(
            sheet,
            AppliedSourceProfile {
                id: profile.id,
                name: profile.name,
                signature: profile.signature,
            },
        );
    }

    validate_explicit_sheet_configuration(&actual, &explicit_semantics, &explicit_fixed_values)?;
    for (sheet, overrides) in explicit_semantics {
        resolved
            .sheet_semantics
            .entry(sheet)
            .or_default()
            .extend(overrides);
    }
    for (sheet, values) in explicit_fixed_values {
        resolved
            .sheet_fixed_values
            .entry(sheet)
            .or_default()
            .extend(values);
    }
    Ok(resolved)
}

fn validate_explicit_sheet_configuration(
    actual: &BTreeMap<String, (String, usize)>,
    semantics: &BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>,
    fixed_values: &BTreeMap<String, BTreeMap<SemanticField, String>>,
) -> Result<(), ApiError> {
    for (sheet, assignments) in semantics {
        let Some((_, width)) = actual.get(sheet) else {
            return Err(ApiError::bad_request(format!(
                "Column mapping refers to a sheet named '{sheet}', but that sheet was not detected."
            )));
        };
        if assignments.keys().any(|index| *index >= *width) {
            return Err(ApiError::bad_request(format!(
                "Column mapping for '{sheet}' refers to a column outside the detected table."
            )));
        }
    }
    for sheet in fixed_values.keys() {
        if !actual.contains_key(sheet) {
            return Err(ApiError::bad_request(format!(
                "Fixed source values refer to a sheet named '{sheet}', but that sheet was not detected."
            )));
        }
    }
    Ok(())
}

fn default_log_limit() -> u64 {
    50
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default = "default_log_limit")]
    limit: u64,
}

pub async fn log(
    State(state): State<AppState>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Vec<ImportLogDto>>, ApiError> {
    let limit = params.limit.clamp(1, 500);
    let entries = blocking("import log", move || {
        let db = state.open_read()?;
        Ok(db
            .import_log(limit)
            .iter()
            .map(ImportLogDto::from)
            .collect::<Vec<_>>())
    })
    .await?;
    Ok(Json(entries))
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use tokio::io::{AsyncRead, ReadBuf};
    use tokio_util::io::ReaderStream;
    use tower::ServiceExt;

    use super::*;
    use crate::server::auth::{AuthStore, Identity, Sessions};
    use crate::server::jobs::{JobKind, JobQueueLimits, JobRegistry};
    use crate::server::state::AppStateInner;

    struct CountingReader {
        bytes: Vec<u8>,
        offset: usize,
        consumed: Arc<AtomicUsize>,
        yield_pending: bool,
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.yield_pending {
                self.yield_pending = false;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if self.offset == self.bytes.len() {
                return Poll::Ready(Ok(()));
            }
            let count = buf.remaining().min(self.bytes.len() - self.offset);
            let end = self.offset + count;
            buf.put_slice(&self.bytes[self.offset..end]);
            self.offset = end;
            self.consumed.store(end, AtomicOrdering::Relaxed);
            self.yield_pending = true;
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn oversized_metadata_is_rejected_before_the_request_stream_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let uploads_dir = temp.path().join("uploads");
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs: JobRegistry::new(),
            uploads_dir: uploads_dir.clone(),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "bounded-metadata";
        let mut bytes = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"rows.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"selected_sheets\"\r\n\r\n"
        )
        .into_bytes();
        bytes.resize(bytes.len() + MAX_SHEET_OPTIONS_BYTES + 1024 * 1024, b'x');
        bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let body_bytes = bytes.len();
        let consumed = Arc::new(AtomicUsize::new(0));
        let body = Body::from_stream(ReaderStream::with_capacity(
            CountingReader {
                bytes,
                offset: 0,
                consumed: Arc::clone(&consumed),
                yield_pending: false,
            },
            1024,
        ));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .unwrap();

        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            consumed.load(AtomicOrdering::Relaxed) < body_bytes / 2,
            "metadata rejection must stop polling the request body near the field cap"
        );
        assert!(
            !uploads_dir.exists() || std::fs::read_dir(&uploads_dir).unwrap().next().is_none(),
            "oversized metadata must clean files streamed earlier in the request"
        );
    }

    #[tokio::test]
    async fn upload_rejects_more_than_the_bounded_file_count() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs: JobRegistry::new(),
            uploads_dir: temp.path().join("uploads"),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "base-search-upload-boundary";
        let mut body = String::new();
        for index in 0..33 {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{index}.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n"
            ));
        }
        body.push_str(&format!("--{boundary}--\r\n"));

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn upload_rejects_one_sheet_selection_for_multiple_workbooks() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs: JobRegistry::new(),
            uploads_dir: temp.path().join("uploads"),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "base-search-sheet-selection";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"selected_sheets\"\r\n\r\n[\"January\"]\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"one.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\none,1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"two.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\ntwo,2\r\n\
             --{boundary}--\r\n"
        );

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upload_applies_manual_column_mapping_to_the_import_job() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let jobs = JobRegistry::new();
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path: db_path.clone(),
            jobs: jobs.clone(),
            uploads_dir: temp.path().join("uploads"),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "base-search-column-mapping";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"sheet_semantics\"\r\n\r\n{{\"table\":{{\"0\":\"Recipient\",\"1\":\"Value\"}}}}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"unknown.csv\"\r\nContent-Type: text/csv\r\n\r\nAlpha,Beta\r\nACME IMPORT,1250.50\r\n\
             --{boundary}--\r\n"
        );

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        for _ in 0..100 {
            if jobs.list().first().is_some_and(|job| {
                matches!(
                    job.status,
                    crate::server::jobs::JobStatus::Succeeded
                        | crate::server::jobs::JobStatus::Failed
                        | crate::server::jobs::JobStatus::Cancelled
                )
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let job = jobs.list().into_iter().next().expect("import job");
        assert_eq!(job.status, crate::server::jobs::JobStatus::Succeeded);

        let db = Db::open(&db_path).unwrap();
        let query = crate::db_types::Query {
            filters: crate::db_types::Filters {
                recipient: "ACME IMPORT".to_string(),
                ..crate::db_types::Filters::default()
            },
            ..crate::db_types::Query::default()
        };
        assert_eq!(db.count(&query).unwrap(), 1);
        let analytics = db.analytics(&query, 10).unwrap();
        assert!((analytics.overview.total_value_usd - 1250.50).abs() < 0.001);
    }

    #[tokio::test]
    async fn full_import_queue_is_rejected_before_any_upload_file_is_stored() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let jobs = JobRegistry::with_limits(JobQueueLimits {
            workspace_pending: 1,
            per_user_pending: 1,
            concurrent_reads: 1,
            concurrent_previews: 1,
        });
        let _held = jobs
            .reserve_for(&Identity::local_owner(), JobKind::Import)
            .unwrap();
        let uploads_dir = temp.path().join("uploads");
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs,
            uploads_dir: uploads_dir.clone(),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "queue-full-import";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"large.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n--{boundary}--\r\n"
        );
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            !uploads_dir.exists() || std::fs::read_dir(&uploads_dir).unwrap().next().is_none(),
            "queue rejection must not leave upload artifacts"
        );
    }

    #[tokio::test]
    async fn preview_admission_is_bounded_before_the_preview_file_is_stored() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let jobs = JobRegistry::with_limits(JobQueueLimits {
            workspace_pending: 2,
            per_user_pending: 1,
            concurrent_reads: 1,
            concurrent_previews: 1,
        });
        let _held = jobs.acquire_preview().unwrap();
        let uploads_dir = temp.path().join("uploads");
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs,
            uploads_dir: uploads_dir.clone(),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "preview-busy";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"preview.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n--{boundary}--\r\n"
        );
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports/peek")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            !uploads_dir.exists() || std::fs::read_dir(&uploads_dir).unwrap().next().is_none(),
            "preview rejection must not leave upload artifacts"
        );
    }

    #[tokio::test]
    async fn rejected_upload_cleans_files_already_streamed_before_bad_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace.db");
        Db::open(&db_path).unwrap();
        let uploads_dir = temp.path().join("uploads");
        let state = Arc::new(AppStateInner {
            auth: AuthStore::open(&db_path).unwrap(),
            db_path,
            jobs: JobRegistry::new(),
            uploads_dir: uploads_dir.clone(),
            exports_dir: temp.path().join("exports"),
            lan_exposed: false,
            require_auth: false,
            sessions: Sessions::new(),
            projection_trust: Mutex::new(None),
        });
        let boundary = "bad-metadata-after-file";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"rows.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"sheet_semantics\"\r\n\r\nnot-json\r\n\
             --{boundary}--\r\n"
        );
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/imports")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let response = crate::server::api::router(state)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            !uploads_dir.exists() || std::fs::read_dir(&uploads_dir).unwrap().next().is_none(),
            "rejected multipart requests must not leave streamed files"
        );
    }
}
