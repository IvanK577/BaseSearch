//! Analytics: scoped overview/sections, pivot cross-tab, undervaluation, and
//! the single-company dossier. All aggregation happens in the core (SQLite);
//! the browser only renders the numbers it is given.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::db::{
    Analytics, AnalyticsScope, CompanyProfile, PivotDim, PivotLimits, PivotMetric, PivotResult,
    Query, Undervaluation,
};
#[cfg(feature = "duckdb-olap")]
use crate::engines::AnalyticsEngine;
use crate::server::DB_STATEMENT_TIMEOUT;
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

fn default_hs_level() -> u8 {
    10
}

fn default_top() -> u64 {
    10
}

fn default_engine() -> String {
    "auto".to_string()
}

#[derive(Deserialize)]
pub struct AnalyticsRequest {
    #[serde(default)]
    query: Query,
    /// `null` returns only the overview + monthly dynamics.
    #[serde(default)]
    scope: Option<AnalyticsScope>,
    #[serde(default = "default_hs_level")]
    hs_level: u8,
    #[serde(default = "default_top")]
    limit: u64,
    /// "auto" (default), "duckdb", or "sqlite". Auto uses the DuckDB projection
    /// when it exists, is fresh, and the query is projection-compatible.
    #[serde(default = "default_engine")]
    engine: String,
    /// When true, return the Overview tab preview cards: the leading section of
    /// each scope, computed from one shared basis. `scope` and `full` are ignored.
    #[serde(default)]
    previews: bool,
    /// When true, compute every section at once (companies, goods, countries,
    /// and prices) for the Report view. Always answered by SQLite so the numbers
    /// are the trusted source of truth; `scope` is ignored.
    #[serde(default)]
    full: bool,
    /// When true the caller reads only `*_sections` and the month series is
    /// skipped, saving one whole aggregate scan per request.
    #[serde(default)]
    sections_only: bool,
}

/// Analytics plus the engine that produced it, so the UI can show whether the
/// fast columnar path (DuckDB) or the row store (SQLite) answered.
#[derive(Serialize)]
pub struct AnalyticsEnvelope {
    engine: &'static str,
    data: Analytics,
}

pub async fn analytics(
    State(state): State<AppState>,
    Json(req): Json<AnalyticsRequest>,
) -> Result<Json<AnalyticsEnvelope>, ApiError> {
    run_analytics(state, req).await.map(Json)
}

#[derive(Deserialize)]
pub struct AnalyticsOverviewRequest {
    #[serde(default)]
    query: Query,
    #[serde(default = "default_top")]
    limit: u64,
    #[serde(default = "default_engine")]
    engine: String,
}

pub async fn overview(
    State(state): State<AppState>,
    Json(req): Json<AnalyticsOverviewRequest>,
) -> Result<Json<AnalyticsEnvelope>, ApiError> {
    run_analytics(
        state,
        AnalyticsRequest {
            query: req.query,
            scope: None,
            hs_level: default_hs_level(),
            limit: req.limit,
            engine: req.engine,
            full: false,
            // This endpoint returns the overview and months and nothing else.
            sections_only: false,
            previews: false,
        },
    )
    .await
    .map(Json)
}

#[derive(Deserialize)]
pub struct AnalyticsSectionRequest {
    #[serde(default)]
    query: Query,
    scope: AnalyticsScope,
    #[serde(default = "default_hs_level")]
    hs_level: u8,
    #[serde(default = "default_top")]
    limit: u64,
    #[serde(default = "default_engine")]
    engine: String,
    /// Set by the Overview previews, which render only the section rows.
    #[serde(default)]
    sections_only: bool,
}

pub async fn section(
    State(state): State<AppState>,
    Json(req): Json<AnalyticsSectionRequest>,
) -> Result<Json<AnalyticsEnvelope>, ApiError> {
    run_analytics(
        state,
        AnalyticsRequest {
            query: req.query,
            scope: Some(req.scope),
            hs_level: req.hs_level,
            limit: req.limit,
            engine: req.engine,
            full: false,
            sections_only: req.sections_only,
            previews: false,
        },
    )
    .await
    .map(Json)
}

#[derive(Deserialize)]
pub struct CompareSideRequest {
    label: String,
    #[serde(default)]
    query: Query,
}

#[derive(Deserialize)]
pub struct CompareRequest {
    left: CompareSideRequest,
    right: CompareSideRequest,
    #[serde(default)]
    scope: Option<AnalyticsScope>,
    #[serde(default = "default_hs_level")]
    hs_level: u8,
    #[serde(default = "default_top")]
    limit: u64,
    #[serde(default = "default_engine")]
    engine: String,
}

#[derive(Serialize)]
pub struct CompareSideEnvelope {
    label: String,
    query: Query,
    engine: &'static str,
    data: Analytics,
}

#[derive(Serialize)]
pub struct CompareEnvelope {
    left: CompareSideEnvelope,
    right: CompareSideEnvelope,
}

pub async fn compare(
    State(state): State<AppState>,
    Json(req): Json<CompareRequest>,
) -> Result<Json<CompareEnvelope>, ApiError> {
    let left_label = validated_compare_label(req.left.label)?;
    let right_label = validated_compare_label(req.right.label)?;
    let left_query = req.left.query;
    let right_query = req.right.query;

    let left = run_analytics(
        state.clone(),
        AnalyticsRequest {
            query: left_query.clone(),
            scope: req.scope,
            hs_level: req.hs_level,
            limit: req.limit,
            engine: req.engine.clone(),
            full: false,
            // Compare renders the headline totals, so it needs the months.
            sections_only: false,
            previews: false,
        },
    )
    .await?;
    let right = run_analytics(
        state,
        AnalyticsRequest {
            query: right_query.clone(),
            scope: req.scope,
            hs_level: req.hs_level,
            limit: req.limit,
            engine: req.engine,
            full: false,
            sections_only: false,
            previews: false,
        },
    )
    .await?;

    Ok(Json(CompareEnvelope {
        left: CompareSideEnvelope {
            label: left_label,
            query: left_query,
            engine: left.engine,
            data: left.data,
        },
        right: CompareSideEnvelope {
            label: right_label,
            query: right_query,
            engine: right.engine,
            data: right.data,
        },
    }))
}

fn validated_compare_label(label: String) -> Result<String, ApiError> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err(ApiError::bad_request(
            "Each comparison side requires a non-empty label.",
        ));
    }
    if label.chars().count() > 120 {
        return Err(ApiError::bad_request(
            "Comparison labels must be at most 120 characters.",
        ));
    }
    Ok(label)
}

async fn run_analytics(
    state: AppState,
    req: AnalyticsRequest,
) -> Result<AnalyticsEnvelope, ApiError> {
    let AnalyticsRequest {
        query,
        scope,
        hs_level,
        limit,
        engine,
        full,
        sections_only,
        previews,
    } = req;
    let _ = &engine; // read under the duckdb-olap feature only
    // Sections can be pulled in bulk for the "see all" tables, not just a top-N.
    let limit = limit.clamp(1, 1000);
    let (data, used) = blocking("analytics", move || {
        // Overview preview cards: one shared basis instead of three scoped
        // requests that each recomputed it and discarded most of their work.
        if previews {
            let db = state.open_read()?;
            let analytics = db
                .with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
                    db.analytics_previews(&query, limit, hs_level)
                        .map_err(|err| err.to_string())
                })
                .map_err(|err| ApiError::from_db("analytics", err))?;
            return Ok((analytics, "sqlite"));
        }
        // Report view: every section at once, always on SQLite (trusted totals).
        if full {
            let db = state.open_read()?;
            let analytics = db
                .with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
                    db.analytics(&query, limit).map_err(|err| err.to_string())
                })
                .map_err(|err| ApiError::from_db("analytics", err))?;
            return Ok((analytics, "sqlite"));
        }
        // Fast path: the DuckDB projection, when available, fresh, and the query
        // has no free text (FTS is SQLite-only) or advanced expression.
        #[cfg(feature = "duckdb-olap")]
        if engine.as_str() != "sqlite"
            && query.text.trim().is_empty()
            && crate::duckdb_olap::supports_projection_query(&query)
            && let Some(duckdb) = crate::server::olap::ready_engine(&state)?
        {
            match duckdb.analytics(&query, limit, scope, hs_level) {
                Ok(analytics) => return Ok((analytics, "duckdb")),
                Err(err) => {
                    eprintln!("[base-search] DuckDB analytics fell back to SQLite: {err}");
                }
            }
        }
        let db = state.open_read()?;
        let analytics = db
            .with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
                match (sections_only, scope) {
                    // The caller reads only the section rows, so the month
                    // series would be a full aggregate scan it discards.
                    (true, Some(scope)) => {
                        db.analytics_sections_only(&query, limit, scope, hs_level)
                    }
                    _ => db.analytics_scoped(&query, limit, scope, hs_level),
                }
                .map_err(|err| err.to_string())
            })
            .map_err(|err| ApiError::from_db("analytics", err))?;
        Ok((analytics, "sqlite"))
    })
    .await?;
    Ok(AnalyticsEnvelope { engine: used, data })
}

fn default_pivot_rows() -> usize {
    20
}

fn default_pivot_cols() -> usize {
    12
}

fn default_others_label() -> String {
    "Other".to_string()
}

#[derive(Deserialize)]
pub struct PivotRequest {
    #[serde(default)]
    query: Query,
    row_dim: PivotDim,
    col_dim: PivotDim,
    metric: PivotMetric,
    #[serde(default = "default_pivot_rows")]
    rows: usize,
    #[serde(default = "default_pivot_cols")]
    cols: usize,
    #[serde(default = "default_others_label")]
    others_label: String,
}

pub async fn pivot(
    State(state): State<AppState>,
    Json(req): Json<PivotRequest>,
) -> Result<Json<PivotResult>, ApiError> {
    let limits = PivotLimits {
        rows: req.rows.clamp(1, 100),
        cols: req.cols.clamp(1, 100),
    };
    let PivotRequest {
        query,
        row_dim,
        col_dim,
        metric,
        others_label,
        ..
    } = req;
    let result = blocking("pivot", move || {
        let db = state.open_read()?;
        db.with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
            db.pivot(&query, row_dim, col_dim, metric, limits, &others_label)
                .map_err(|err| err.to_string())
        })
        .map_err(|err| ApiError::from_db("pivot", err))
    })
    .await?;
    Ok(Json(result))
}

fn default_threshold() -> f64 {
    0.5
}

fn default_min_samples() -> u64 {
    20
}

#[derive(Deserialize)]
pub struct UndervaluationRequest {
    #[serde(default)]
    query: Query,
    #[serde(default = "default_threshold")]
    threshold: f64,
    #[serde(default = "default_min_samples")]
    min_samples: u64,
    #[serde(default = "default_top")]
    limit: u64,
}

pub async fn undervaluation(
    State(state): State<AppState>,
    Json(req): Json<UndervaluationRequest>,
) -> Result<Json<Undervaluation>, ApiError> {
    let UndervaluationRequest {
        query,
        threshold,
        min_samples,
        limit,
    } = req;
    let threshold = threshold.clamp(0.01, 1.0);
    let min_samples = min_samples.clamp(20, 1_000);
    let limit = limit.clamp(1, 500);
    let result = blocking("undervaluation", move || {
        let db = state.open_read()?;
        db.with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
            db.undervaluation(&query, threshold, min_samples, limit)
                .map_err(|err| err.to_string())
        })
        .map_err(|err| ApiError::from_db("undervaluation", err))
    })
    .await?;
    Ok(Json(result))
}

fn default_company_limit() -> u64 {
    10
}

#[derive(Deserialize)]
pub struct CompanyQuery {
    #[serde(default = "default_company_limit")]
    limit: u64,
}

pub async fn company(
    State(state): State<AppState>,
    Path(edrpou): Path<String>,
    axum::extract::Query(params): axum::extract::Query<CompanyQuery>,
) -> Result<Json<CompanyProfile>, ApiError> {
    let limit = params.limit.clamp(1, 100);
    let result = blocking("company profile", move || {
        let db = state.open_read()?;
        db.with_statement_deadline(DB_STATEMENT_TIMEOUT, |db| {
            db.company_profile(&edrpou, limit)
                .map_err(|err| err.to_string())
        })
        .map_err(|err| ApiError::from_db("company profile", err))
    })
    .await?;
    Ok(Json(result))
}
