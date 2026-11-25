//! Full record card by id.

use axum::Json;
use axum::extract::{Path, State};

use crate::server::dto::RecordDto;
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

pub async fn record(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RecordDto>, ApiError> {
    let dto = blocking("record card", move || {
        let db = state.open_read()?;
        let card = db.record_card(id).map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::not_found(format!("No record with id {id}"))
            }
            other => ApiError::internal("record card", other),
        })?;
        Ok(RecordDto::from_card(id, card))
    })
    .await?;
    Ok(Json(dto))
}
