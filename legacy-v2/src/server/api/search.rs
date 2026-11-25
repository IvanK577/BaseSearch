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

fn resolve_snapshot(db: &crate::db::Db, requested: Option<u64>) -> Result<u64, String> {
    let current = db
        .capture_search_snapshot()
        .map_err(|err| err.to_string())?;
    Ok(requested.unwrap_or(current).min(current))
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
    /// Highest record id visible to this search session. Omitting it starts a
    /// new snapshot; clients can reuse the returned token for count and pages.
    #[serde(default)]
    snapshot: Option<u64>,
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
    total: u64,
    snapshot: u64,
}

pub async fn search(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let limit = req.limit.clamp(1, MAX_LIMIT);
    let offset = req.offset;
    let query = req.query;
    let sort = req.sort;
    let requested_snapshot = req.snapshot;

    let response = blocking("search", move || {
        let db = state.open_read()?;
        let (snapshot, page, total) = db
            .with_statement_deadline(crate::server::DB_STATEMENT_TIMEOUT, |db| {
                let snapshot = resolve_snapshot(db, requested_snapshot)?;
                let page = db
                    .search_page_dynamic_sorted_at_snapshot(
                        &query,
                        limit + 1,
                        offset,
                        sort.clone(),
                        snapshot,
                    )
                    .map_err(|err| err.to_string())?;
                let total = db
                    .count_at_snapshot(&query, snapshot)
                    .map_err(|err| err.to_string())?;
                Ok((snapshot, page, total))
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
            total,
            snapshot,
        })
    })
    .await?;
    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct CountRequest {
    #[serde(default)]
    query: Query,
    #[serde(default)]
    snapshot: Option<u64>,
}

#[derive(Serialize)]
pub struct CountResponse {
    total: u64,
    snapshot: u64,
}

pub async fn count(
    State(state): State<AppState>,
    Json(req): Json<CountRequest>,
) -> Result<Json<CountResponse>, ApiError> {
    let query = req.query;
    let requested_snapshot = req.snapshot;
    let response = blocking("count", move || {
        let db = state.open_read()?;
        let (snapshot, total) = db
            .with_statement_deadline(crate::server::DB_STATEMENT_TIMEOUT, |db| {
                let snapshot = resolve_snapshot(db, requested_snapshot)?;
                let total = db
                    .count_at_snapshot(&query, snapshot)
                    .map_err(|err| err.to_string())?;
                Ok((snapshot, total))
            })
            .map_err(|err| ApiError::from_db("count", err))?;
        Ok(CountResponse { total, snapshot })
    })
    .await?;
    Ok(Json(response))
}
