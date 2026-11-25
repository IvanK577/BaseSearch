//! Liveness and workspace status.

use axum::Json;
use axum::extract::{Extension, State};
use serde::Serialize;

use crate::server::auth::Identity;
use crate::server::dto::StorageDto;
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
pub struct HealthDto {
    status: &'static str,
    name: &'static str,
    version: &'static str,
}

pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok",
        name: "Base Search",
        version: VERSION,
    })
}

#[derive(Serialize)]
pub struct StatusDto {
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    db_path: Option<String>,
    total_rows: u64,
    unindexed_rows: u64,
    has_data: bool,
    has_shape: bool,
    lan_exposed: bool,
    storage: StorageDto,
    extra_headers: Vec<String>,
}

pub async fn status(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<StatusDto>, ApiError> {
    let reveal_local_paths = identity.role.is_privileged();
    let dto = blocking("status", move || {
        let db = state.open_read()?;
        let storage = db
            .storage_info(state.db_path())
            .map_err(|err| ApiError::internal("storage info", err))?;
        let total_rows = db.total_rows();
        Ok(StatusDto {
            version: VERSION,
            db_path: reveal_local_paths.then(|| state.db_path().display().to_string()),
            total_rows,
            unindexed_rows: db.unindexed_rows(),
            has_data: total_rows > 0,
            has_shape: db.table_shape().is_some(),
            lan_exposed: state.lan_exposed,
            storage: StorageDto::from(&storage),
            extra_headers: db.cached_extra_headers(),
        })
    })
    .await?;
    Ok(Json(dto))
}
