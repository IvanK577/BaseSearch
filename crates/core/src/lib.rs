use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub mod export;
pub mod import;
pub mod legacy;
pub mod model;
pub mod query;
pub mod summary;
pub mod workspace;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("Операция отменена")]
    Cancelled,
    #[error("Данные обновились (версия {expected} → {actual}). Повторите поиск.")]
    StaleRevision { expected: i64, actual: i64 },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub processed: u64,
    pub total: Option<u64>,
    pub message: String,
}

pub struct Operation<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a (dyn Fn(Progress) + Send + Sync),
    shared_cancel: Option<Arc<AtomicBool>>,
}

impl<'a> Operation<'a> {
    pub fn new(cancel: &'a AtomicBool, progress: &'a (dyn Fn(Progress) + Send + Sync)) -> Self {
        Self {
            cancel,
            progress,
            shared_cancel: None,
        }
    }

    pub fn with_shared_cancel(
        cancel: &'a Arc<AtomicBool>,
        progress: &'a (dyn Fn(Progress) + Send + Sync),
    ) -> Self {
        Self {
            cancel,
            progress,
            shared_cancel: Some(cancel.clone()),
        }
    }

    pub fn shared_cancel(&self) -> Option<Arc<AtomicBool>> {
        self.shared_cancel.clone()
    }

    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    pub fn report(&self, processed: u64, total: Option<u64>, message: impl Into<String>) {
        (self.progress)(Progress {
            processed,
            total,
            message: message.into(),
        });
    }
}
