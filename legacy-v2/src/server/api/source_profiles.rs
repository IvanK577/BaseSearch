//! Reusable source-mapping profiles scoped to the current workspace database.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::db::{
    SourceMappingProfile, SourceMappingProfileCollection, SourceMappingProfileError,
    SourceMappingProfileUpsert,
};
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

pub(super) const MAX_PROFILE_BODY_BYTES: usize = 512 * 1024;
const MAX_SIGNATURE_BYTES: usize = 96;

#[derive(Debug, Deserialize)]
pub struct SuggestQuery {
    signature: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    deleted: bool,
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<SourceMappingProfileCollection>, ApiError> {
    let collection = blocking("list source mapping profiles", move || {
        let db = state.open_read()?;
        db.list_source_mapping_profiles().map_err(profile_error)
    })
    .await?;
    Ok(Json(collection))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<SourceMappingProfile>, ApiError> {
    if id <= 0 {
        return Err(ApiError::bad_request("Profile id must be positive."));
    }
    let profile = blocking("get source mapping profile", move || {
        let db = state.open_read()?;
        db.get_source_mapping_profile(id)
            .map_err(profile_error)?
            .ok_or_else(|| ApiError::not_found("Source mapping profile was not found."))
    })
    .await?;
    Ok(Json(profile))
}

pub async fn suggest(
    State(state): State<AppState>,
    Query(query): Query<SuggestQuery>,
) -> Result<Json<SourceMappingProfileCollection>, ApiError> {
    if query.signature.is_empty() || query.signature.len() > MAX_SIGNATURE_BYTES {
        return Err(ApiError::bad_request("Source signature is invalid."));
    }
    let signature = query.signature;
    let collection = blocking("suggest source mapping profiles", move || {
        let db = state.open_read()?;
        db.suggest_source_mapping_profiles(&signature)
            .map_err(profile_error)
    })
    .await?;
    Ok(Json(collection))
}

pub async fn upsert(
    State(state): State<AppState>,
    Json(input): Json<SourceMappingProfileUpsert>,
) -> Result<Json<SourceMappingProfile>, ApiError> {
    let profile = blocking("save source mapping profile", move || {
        let db = state.open_read()?;
        db.upsert_source_mapping_profile(input)
            .map_err(profile_error)
    })
    .await?;
    Ok(Json(profile))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<DeleteResponse>, ApiError> {
    if id <= 0 {
        return Err(ApiError::bad_request("Profile id must be positive."));
    }
    let deleted = blocking("delete source mapping profile", move || {
        let db = state.open_read()?;
        db.delete_source_mapping_profile(id).map_err(profile_error)
    })
    .await?;
    if !deleted {
        return Err(ApiError::not_found("Source mapping profile was not found."));
    }
    Ok(Json(DeleteResponse { deleted }))
}

pub(super) fn profile_error(error: SourceMappingProfileError) -> ApiError {
    match error {
        SourceMappingProfileError::Validation(message) => ApiError::bad_request(message),
        SourceMappingProfileError::NotFound(_) => {
            ApiError::not_found("Source mapping profile was not found.")
        }
        SourceMappingProfileError::NameConflict(message) => ApiError::new(
            StatusCode::CONFLICT,
            "profile_name_conflict",
            format!("A source mapping profile named '{message}' already exists."),
        ),
        SourceMappingProfileError::CorruptRow { .. } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "corrupt_profile",
            "This source mapping profile is corrupt and cannot be used.",
        ),
        SourceMappingProfileError::Database(error) => {
            ApiError::internal("source mapping profile database", error)
        }
    }
}
