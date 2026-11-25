//! Shared application state handed to every request handler.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::db::Db;

use super::auth::{AuthStore, Sessions};
use super::error::ApiError;
use super::jobs::JobRegistry;

pub struct AppStateInner {
    /// Database this workspace operates on.
    pub db_path: PathBuf,
    /// Long-running operation registry.
    pub jobs: JobRegistry,
    /// Directory for uploaded import files (inside the database folder).
    pub uploads_dir: PathBuf,
    /// Directory for generated export files awaiting download.
    pub exports_dir: PathBuf,
    /// True when the server is reachable from the local network.
    pub lan_exposed: bool,
    /// True when this server enforces authentication (non-loopback bind).
    pub require_auth: bool,
    /// Local account store (argon2, separate `<db>.auth.db`).
    pub auth: AuthStore,
    /// In-memory session registry.
    pub sessions: Sessions,
    /// Cache of whether the current DuckDB projection (keyed by its `built_at`)
    /// reproduces the SQLite overview. Prevents serving zeroed aggregates from a
    /// projection that cannot see this dataset's value/weight columns.
    #[cfg_attr(not(feature = "duckdb-olap"), allow(dead_code))]
    pub projection_trust: Mutex<Option<(String, bool)>>,
}

pub type AppState = Arc<AppStateInner>;

impl AppStateInner {
    /// Opens a fast read connection (no migrations) for a single request.
    /// WAL mode allows these to run concurrently with each other and with the
    /// single active write job.
    pub fn open_read(&self) -> Result<Db, ApiError> {
        Db::open_runtime(&self.db_path).map_err(|err| ApiError::internal("open database", err))
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}
