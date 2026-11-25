//! Job status endpoints. The browser polls these to render progress bars and
//! surface results for imports, exports and maintenance tasks.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use serde::Serialize;

use crate::server::auth::Identity;
use crate::server::error::ApiError;
use crate::server::jobs::{JobAccessError, JobSnapshot};
use crate::server::state::AppState;

#[derive(Serialize)]
pub struct JobListDto {
    jobs: Vec<JobSnapshot>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Json<JobListDto> {
    Json(JobListDto {
        jobs: state.jobs.list_for(&identity),
    })
}

pub async fn get(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<u64>,
) -> Result<Json<JobSnapshot>, ApiError> {
    state
        .jobs
        .snapshot_for(&identity, id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("No job with id {id}")))
}

#[derive(Serialize)]
pub struct CancelDto {
    cancelled: bool,
}

pub async fn cancel(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<u64>,
) -> Result<Json<CancelDto>, ApiError> {
    match state.jobs.cancel_for(&identity, id) {
        Ok(cancelled) => Ok(Json(CancelDto { cancelled })),
        Err(JobAccessError::NotFound) => Err(ApiError::not_found(format!("No job with id {id}"))),
        Err(JobAccessError::Forbidden) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "You can cancel only your own jobs.",
        )),
    }
}
