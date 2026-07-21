//! Result page and total-count endpoints.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::db::{Query, ResultSort};
use crate::server::dto::{FieldDto, field_dtos};
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

const MAX_LIMIT: u64 = 200;

fn default_limit() -> u64 {
    50
}

#[derive(Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    query: Query,
    #[serde(default = "default_limit")]
    limit: u64,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    sort: Option<ResultSort>,
}

#[derive(Serialize)]
pub struct RowDto {
    id: i64,
    values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplicate_of: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    fields: Vec<FieldDto>,
    rows: Vec<RowDto>,
    offset: u64,
    limit: u64,
    has_next: bool,
}

pub async fn search(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let limit = req.limit.clamp(1, MAX_LIMIT);
    let offset = req.offset;
    let query = req.query;
    let sort = req.sort;

    let response = blocking("search", move || {
        let db = state.open_read()?;
        // Fetch one extra row to learn whether a next page exists without a
        // second count query.
        let page = db
            .with_statement_deadline(crate::server::DB_STATEMENT_TIMEOUT, |db| {
                db.search_page_dynamic_sorted(&query, limit + 1, offset, sort.clone())
                    .map_err(|err| err.to_string())
            })
            .map_err(|err| ApiError::from_db("search", err))?;
        let (fields, ids, mut rows, mut duplicate_of) = page;
        let has_next = rows.len() as u64 > limit;
        if has_next {
            rows.truncate(limit as usize);
            duplicate_of.truncate(limit as usize);
        }
        let row_dtos = ids
            .into_iter()
            .zip(rows)
            .zip(duplicate_of)
            .map(|((id, values), duplicate_of)| RowDto {
                id,
                values,
                duplicate_of,
            })
            .collect();
        Ok(SearchResponse {
            fields: field_dtos(&fields),
            rows: row_dtos,
            offset,
            limit,
            has_next,
        })
    })
    .await?;
    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct CountRequest {
    #[serde(default)]
    query: Query,
}

#[derive(Serialize)]
pub struct CountResponse {
    total: u64,
}

pub async fn count(
    State(state): State<AppState>,
    Json(req): Json<CountRequest>,
) -> Result<Json<CountResponse>, ApiError> {
    let query = req.query;
    let response = blocking("count", move || {
        let db = state.open_read()?;
        let total = db
            .with_statement_deadline(crate::server::DB_STATEMENT_TIMEOUT, |db| {
                db.count(&query).map_err(|err| err.to_string())
            })
            .map_err(|err| ApiError::from_db("count", err))?;
        Ok(CountResponse { total })
    })
    .await?;
    Ok(Json(response))
}
