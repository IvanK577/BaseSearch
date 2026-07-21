//! Field catalog, result columns, and the editable source-column mapping.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::domain::table::{SemanticField, SourceColumn};
use crate::server::dto::{FieldDto, field_dtos};
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

#[derive(Serialize)]
pub struct SchemaDto {
    /// Fields usable in filters / advanced query (includes Year).
    search_fields: Vec<FieldDto>,
    /// Columns shown in the results table, in display order.
    result_fields: Vec<FieldDto>,
    /// Editable source columns with their inferred semantics, when a table
    /// shape has been learned from imports.
    columns: Vec<SourceColumn>,
    has_shape: bool,
    total_rows: u64,
}

pub async fn schema(State(state): State<AppState>) -> Result<Json<SchemaDto>, ApiError> {
    let dto = blocking("schema", move || {
        let db = state.open_read()?;
        let shape = db.table_shape();
        Ok(SchemaDto {
            search_fields: field_dtos(&db.field_catalog_cached()),
            result_fields: field_dtos(&db.result_fields_cached()),
            columns: shape
                .as_ref()
                .map(|s| s.columns.clone())
                .unwrap_or_default(),
            has_shape: shape.is_some(),
            total_rows: db.total_rows(),
        })
    })
    .await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
pub struct SemanticRequest {
    /// New semantic meaning, or `null` to clear it.
    #[serde(default)]
    semantic: Option<SemanticField>,
}

#[derive(Serialize)]
pub struct SemanticResponse {
    ok: bool,
    columns: Vec<SourceColumn>,
}

pub async fn set_semantic(
    State(state): State<AppState>,
    Path(column_id): Path<String>,
    Json(req): Json<SemanticRequest>,
) -> Result<Json<SemanticResponse>, ApiError> {
    let dto = blocking("set column semantic", move || {
        // A small metadata write; opened read-write through the migrating path.
        let db =
            Db::open(state.db_path()).map_err(|err| ApiError::internal("open database", err))?;
        if !db.set_column_semantic(&column_id, req.semantic) {
            return Err(ApiError::not_found(format!(
                "No source column with id '{column_id}'"
            )));
        }
        let columns = db
            .table_shape()
            .map(|shape| shape.columns)
            .unwrap_or_default();
        Ok(SemanticResponse { ok: true, columns })
    })
    .await?;
    Ok(Json(dto))
}
