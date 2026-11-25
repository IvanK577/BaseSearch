//! Analytics engine status: whether the DuckDB columnar projection is
//! available and fresh, so the UI can show the active engine and offer to
//! (re)build the projection.

use axum::Json;
use axum::extract::{Extension, State};
use serde::Serialize;

use crate::server::auth::Identity;
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

#[derive(Serialize)]
pub struct ProjectionInfo {
    rows: u64,
    max_record_id: u64,
    built_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize)]
pub struct EngineStatus {
    /// True when this binary was compiled with the `duckdb-olap` feature.
    duckdb_available: bool,
    db_rows: u64,
    db_max_record_id: u64,
    projection: Option<ProjectionInfo>,
    /// True when a projection exists but is behind the live database.
    projection_stale: bool,
    /// True when the fresh projection reproduces the SQLite totals (and is thus
    /// safe to serve analytics from).
    projection_trusted: bool,
    /// Engine `auto` would use for a projection-compatible query right now.
    default_analytics_engine: &'static str,
}

pub async fn status(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<EngineStatus>, ApiError> {
    let reveal_local_paths = identity.role.is_privileged();
    #[cfg(not(feature = "duckdb-olap"))]
    let _ = reveal_local_paths;
    let dto = blocking("engine status", move || {
        let db = state.open_read()?;
        let db_rows = db.total_rows();
        let db_max_record_id = db.max_record_id();
        drop(db);

        let duckdb_available = cfg!(feature = "duckdb-olap");

        #[cfg(feature = "duckdb-olap")]
        let (projection, projection_stale) = {
            let path = crate::duckdb_olap::default_projection_path(state.db_path());
            match path
                .exists()
                .then(|| crate::duckdb_olap::read_projection_meta(&path))
            {
                Some(Ok(meta)) => {
                    let stale = !crate::duckdb_olap::projection_is_current(state.db_path(), &path)
                        .unwrap_or(false);
                    (
                        Some(ProjectionInfo {
                            rows: meta.rows,
                            max_record_id: meta.max_record_id,
                            built_at: meta.built_at,
                            path: reveal_local_paths.then(|| path.display().to_string()),
                        }),
                        stale,
                    )
                }
                _ => (None, false),
            }
        };
        #[cfg(not(feature = "duckdb-olap"))]
        let (projection, projection_stale): (Option<ProjectionInfo>, bool) = (None, false);

        // Trusted means fresh AND value-consistent with SQLite.
        #[cfg(feature = "duckdb-olap")]
        let projection_trusted = crate::server::olap::projection_trusted(&state)?;
        #[cfg(not(feature = "duckdb-olap"))]
        let projection_trusted = false;

        let default_analytics_engine = if projection_trusted {
            "duckdb"
        } else {
            "sqlite"
        };

        Ok(EngineStatus {
            duckdb_available,
            db_rows,
            db_max_record_id,
            projection,
            projection_stale,
            projection_trusted,
            default_analytics_engine,
        })
    })
    .await?;
    Ok(Json(dto))
}
