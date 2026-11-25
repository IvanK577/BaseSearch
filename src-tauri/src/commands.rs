use crate::jobs::{JobKind, JobResult, JobState, JobStatus, JobStore};
use base_search_core::{
    Error, Operation, Progress,
    import::{self, ImportFileRequest, ImportReport, Preview},
    model::{Dimension, QuerySpec, Record, SearchPage, Summary, WorkspaceInfo},
    workspace::Workspace,
    {export, legacy, query, summary as analytics},
};
use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{App, Manager, State};

type AppResult<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub struct DesktopState {
    path: PathBuf,
    workspace: Arc<Mutex<Option<Arc<Workspace>>>>,
    jobs: JobStore,
    smoke_marker: Option<PathBuf>,
}

impl DesktopState {
    pub fn new(app: &App) -> tauri::Result<Self> {
        let smoke_dir = std::env::var_os("BASE_SEARCH_SMOKE_DIR").map(PathBuf::from);
        let folder = match &smoke_dir {
            Some(folder) => folder.clone(),
            None => app.path().app_local_data_dir()?,
        };
        Ok(Self {
            path: folder.join("workspace.sqlite"),
            workspace: Arc::new(Mutex::new(None)),
            jobs: JobStore::default(),
            smoke_marker: smoke_dir.map(|folder| folder.join("window-ready.txt")),
        })
    }

    fn workspace(&self) -> base_search_core::Result<Arc<Workspace>> {
        let mut cached = self
            .workspace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(workspace) = cached.as_ref() {
            return Ok(workspace.clone());
        }
        let workspace = Arc::new(Workspace::open(&self.path).map_err(|err| {
            Error::Message(format!(
                "Не удалось открыть рабочую базу в «{}». Проверьте, что папка доступна для записи. {err}",
                self.path.display()
            ))
        })?);
        *cached = Some(workspace.clone());
        Ok(workspace)
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> base_search_core::Result<T> + Send + 'static,
) -> AppResult<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|err| format!("Не удалось выполнить операцию: {err}"))?
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn workspace_info(state: State<'_, DesktopState>) -> AppResult<WorkspaceInfo> {
    let desktop = state.inner().clone();
    blocking(move || {
        let info = desktop.workspace()?.info()?;
        if let Some(marker) = desktop.smoke_marker {
            fs::write(marker, env!("CARGO_PKG_VERSION"))?;
        }
        Ok(info)
    })
    .await
}

#[tauri::command]
pub async fn preview_file(path: String) -> AppResult<Preview> {
    blocking(move || import::preview_file(Path::new(&path))).await
}

#[tauri::command]
pub async fn search(
    state: State<'_, DesktopState>,
    query: QuerySpec,
    offset: u64,
    limit: u64,
) -> AppResult<SearchPage> {
    let desktop = state.inner().clone();
    blocking(move || {
        let workspace = desktop.workspace()?;
        query::search(&workspace, &query, offset, limit)
    })
    .await
}

#[tauri::command]
pub async fn record_detail(
    state: State<'_, DesktopState>,
    id: i64,
    revision: Option<i64>,
) -> AppResult<Option<Record>> {
    let desktop = state.inner().clone();
    blocking(move || {
        let workspace = desktop.workspace()?;
        query::record(&workspace, id, revision)
    })
    .await
}

#[tauri::command]
pub async fn summary(
    state: State<'_, DesktopState>,
    query: QuerySpec,
    dimension: Dimension,
) -> AppResult<Summary> {
    let desktop = state.inner().clone();
    blocking(move || {
        let workspace = desktop.workspace()?;
        analytics::summarize(&workspace, &query, dimension)
    })
    .await
}

fn result_from_import(report: ImportReport) -> JobResult {
    JobResult {
        rows: report.inserted_rows,
        skipped_sheets: report.skipped_sheets,
        files: report.files,
        path: None,
    }
}

fn begin_job(
    desktop: &DesktopState,
    kind: JobKind,
    work: impl FnOnce(&Workspace, &Operation<'_>) -> base_search_core::Result<JobResult>
    + Send
    + 'static,
) -> AppResult<u32> {
    let ticket = desktop.jobs.start(kind)?;
    let id = ticket.id;
    let desktop = desktop.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let notify = |progress: Progress| desktop.jobs.progress(id, progress);
        let op = Operation::with_shared_cancel(&ticket.cancel, &notify);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            op.check()?;
            let workspace = desktop.workspace()?;
            work(&workspace, &op)
        }));
        let (state, message, result) = match outcome {
            Ok(Ok(result))
                if !result.files.is_empty()
                    && result.files.iter().all(|file| file.error.is_some()) =>
            {
                (
                    JobState::Failed,
                    "Файлы не удалось загрузить. Проверьте отчёт.".into(),
                    Some(result),
                )
            }
            Ok(Ok(result)) => {
                let message = match kind {
                    JobKind::Import => "Импорт завершён",
                    JobKind::Migration => "База 2.2.0 перенесена",
                    JobKind::Export => "Выгрузка готова",
                };
                (JobState::Complete, message.into(), Some(result))
            }
            Ok(Err(Error::Cancelled)) => (JobState::Cancelled, "Операция отменена".into(), None),
            Ok(Err(error)) => (JobState::Failed, error.to_string(), None),
            Err(_) => (
                JobState::Failed,
                "Внутренняя ошибка. Повторите операцию.".into(),
                None,
            ),
        };
        desktop.jobs.finish(id, state, message, result);
    });
    Ok(id)
}

#[tauri::command]
pub fn start_import(
    state: State<'_, DesktopState>,
    files: Vec<ImportFileRequest>,
) -> AppResult<u32> {
    if files.is_empty() {
        return Err("Выберите хотя бы один файл.".into());
    }
    begin_job(state.inner(), JobKind::Import, move |workspace, op| {
        import::import_files(workspace, &files, op).map(result_from_import)
    })
}

#[tauri::command]
pub fn start_migration(state: State<'_, DesktopState>, path: String) -> AppResult<u32> {
    begin_job(state.inner(), JobKind::Migration, move |workspace, op| {
        legacy::migrate_legacy(workspace, Path::new(&path), op).map(result_from_import)
    })
}

#[tauri::command]
pub fn start_export(
    state: State<'_, DesktopState>,
    query: QuerySpec,
    path: String,
    columns: Option<Vec<String>>,
) -> AppResult<u32> {
    begin_job(state.inner(), JobKind::Export, move |workspace, op| {
        let rows = export::export(workspace, &query, Path::new(&path), columns.as_deref(), op)?;
        Ok(JobResult {
            rows,
            skipped_sheets: 0,
            files: vec![],
            path: Some(path),
        })
    })
}

#[tauri::command]
pub fn job_status(state: State<'_, DesktopState>, id: u32) -> AppResult<JobStatus> {
    state.jobs.status(id)
}

#[tauri::command]
pub fn cancel_job(state: State<'_, DesktopState>, id: u32) -> AppResult<()> {
    state.jobs.cancel(id)
}

#[tauri::command]
pub fn package_smoke_result(
    state: State<'_, DesktopState>,
    error: Option<String>,
) -> AppResult<()> {
    let marker = state
        .smoke_marker
        .as_ref()
        .ok_or("Package test mode is disabled.")?
        .with_file_name("package-smoke.json");
    let result = serde_json::to_vec(&serde_json::json!({ "error": error }))
        .map_err(|err| err.to_string())?;
    let temporary = marker.with_extension("tmp");
    fs::write(&temporary, result).map_err(|err| err.to_string())?;
    fs::rename(&temporary, marker).map_err(|err| err.to_string())
}
