use base_search_core::{Progress, import::ImportFileResult};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

const RETAINED_JOBS: usize = 24;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Import,
    Migration,
    Export,
}

impl JobKind {
    fn label(self) -> &'static str {
        match self {
            Self::Import => "Читаем исходные файлы",
            Self::Migration => "Проверяем базу 2.2.0",
            Self::Export => "Подготавливаем выгрузку",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Running,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    pub rows: u64,
    pub skipped_sheets: u64,
    pub files: Vec<ImportFileResult>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    pub id: u32,
    pub kind: JobKind,
    pub state: JobState,
    pub processed: u64,
    pub total: Option<u64>,
    pub message: String,
    pub result: Option<JobResult>,
}

pub struct JobTicket {
    pub id: u32,
    pub cancel: Arc<AtomicBool>,
}

struct Entry {
    status: JobStatus,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Registry {
    next_id: u32,
    entries: VecDeque<Entry>,
}

#[derive(Clone, Default)]
pub struct JobStore(Arc<Mutex<Registry>>);

impl JobStore {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(&self, kind: JobKind) -> Result<JobTicket, String> {
        let mut registry = self.registry();
        if registry
            .entries
            .back()
            .is_some_and(|last| last.status.state == JobState::Running)
        {
            return Err("Дождитесь завершения текущей операции или отмените её.".into());
        }
        let id = registry
            .next_id
            .checked_add(1)
            .ok_or("Перезапустите приложение, чтобы продолжить.")?;
        registry.next_id = id;
        let cancel = Arc::new(AtomicBool::new(false));
        registry.entries.push_back(Entry {
            status: JobStatus {
                id,
                kind,
                state: JobState::Running,
                processed: 0,
                total: None,
                message: kind.label().into(),
                result: None,
            },
            cancel: cancel.clone(),
        });
        if registry.entries.len() > RETAINED_JOBS {
            registry.entries.pop_front();
        }
        Ok(JobTicket { id, cancel })
    }

    pub fn status(&self, id: u32) -> Result<JobStatus, String> {
        self.registry()
            .entries
            .iter()
            .find(|entry| entry.status.id == id)
            .map(|entry| entry.status.clone())
            .ok_or_else(|| "Операция не найдена. Обновите данные.".into())
    }

    pub fn cancel(&self, id: u32) -> Result<(), String> {
        let mut registry = self.registry();
        let entry = registry
            .entries
            .back_mut()
            .filter(|entry| entry.status.id == id && entry.status.state == JobState::Running)
            .ok_or("Эта операция уже завершилась.")?;
        entry.cancel.store(true, Ordering::Release);
        entry.status.message = "Отменяем операцию…".into();
        Ok(())
    }

    pub fn progress(&self, id: u32, progress: Progress) {
        let mut registry = self.registry();
        if let Some(entry) = registry
            .entries
            .back_mut()
            .filter(|entry| entry.status.id == id && entry.status.state == JobState::Running)
        {
            entry.status.processed = progress.processed;
            entry.status.total = progress.total;
            if !entry.cancel.load(Ordering::Acquire) {
                entry.status.message = progress.message;
            }
        }
    }

    pub fn finish(&self, id: u32, state: JobState, message: String, result: Option<JobResult>) {
        let mut registry = self.registry();
        if let Some(entry) = registry
            .entries
            .back_mut()
            .filter(|entry| entry.status.id == id && entry.status.state == JobState::Running)
        {
            if let Some(result) = &result {
                entry.status.processed = result.rows;
                entry.status.total = Some(result.rows);
            }
            entry.status.state = state;
            entry.status.message = message;
            entry.status.result = result;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn cancellation_keeps_the_job_reserved_until_the_worker_finishes() {
        let jobs = JobStore::default();
        let first = jobs.start(JobKind::Export).unwrap();
        assert!(jobs.start(JobKind::Import).is_err());

        jobs.cancel(first.id).unwrap();
        assert!(first.cancel.load(Ordering::Acquire));
        assert_eq!(jobs.status(first.id).unwrap().state, JobState::Running);
        assert!(jobs.start(JobKind::Migration).is_err());

        jobs.finish(
            first.id,
            JobState::Cancelled,
            "Операция отменена".into(),
            None,
        );
        let next = jobs.start(JobKind::Import).unwrap();
        assert_ne!(first.id, next.id);
        assert_eq!(jobs.status(first.id).unwrap().state, JobState::Cancelled);
        assert!(jobs.cancel(first.id).is_err());
        assert!(!next.cancel.load(Ordering::Acquire));
    }

    #[test]
    fn late_progress_cannot_change_a_completed_job() {
        let jobs = JobStore::default();
        let work = jobs.start(JobKind::Export).unwrap();
        jobs.progress(
            work.id,
            Progress {
                processed: 5,
                total: Some(9),
                message: "Запись".into(),
            },
        );
        jobs.finish(
            work.id,
            JobState::Complete,
            "Готово".into(),
            Some(JobResult {
                rows: 9,
                skipped_sheets: 0,
                files: vec![],
                path: None,
            }),
        );
        jobs.progress(
            work.id,
            Progress {
                processed: 1,
                total: None,
                message: "Старый ответ".into(),
            },
        );
        let status = jobs.status(work.id).unwrap();
        assert_eq!(status.state, JobState::Complete);
        assert_eq!(status.processed, 9);
        assert_eq!(status.message, "Готово");
    }
}
