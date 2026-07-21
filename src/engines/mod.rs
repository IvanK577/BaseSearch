use std::path::{Path, PathBuf};

use crate::db::{Analytics, AnalyticsScope, Db, PivotDim, PivotLimits, PivotMetric, Query};
use crate::search::FieldInfo;

#[derive(Clone, Debug)]
pub struct EngineSchema {
    pub search_fields: Vec<FieldInfo>,
    pub result_fields: Vec<FieldInfo>,
}

#[derive(Clone, Debug)]
pub struct EngineSearchPage {
    pub fields: Vec<FieldInfo>,
    pub ids: Vec<i64>,
    pub rows: Vec<Vec<String>>,
    pub duplicate_of: Vec<Option<String>>,
    pub has_next: bool,
}

pub trait SearchEngine {
    fn engine_id(&self) -> &'static str;
    fn schema(&self) -> Result<EngineSchema, String>;
    fn count(&self, query: &Query) -> Result<u64, String>;
    fn search_page(
        &self,
        query: &Query,
        limit: u64,
        offset: u64,
    ) -> Result<EngineSearchPage, String>;
}

pub trait AnalyticsEngine {
    fn engine_id(&self) -> &'static str;
    fn analytics(
        &self,
        query: &Query,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
    ) -> Result<Analytics, String>;
    fn pivot(
        &self,
        query: &Query,
        row_dim: PivotDim,
        col_dim: PivotDim,
        metric: PivotMetric,
        limits: PivotLimits,
        others_label: &str,
    ) -> Result<crate::db::PivotResult, String>;
}

pub trait ImportSink {
    fn engine_id(&self) -> &'static str;
}

#[derive(Clone, Debug)]
pub struct SqliteEngine {
    db_path: PathBuf,
}

impl SqliteEngine {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn open(&self) -> Result<Db, String> {
        Db::open(&self.db_path)
    }
}

impl SearchEngine for SqliteEngine {
    fn engine_id(&self) -> &'static str {
        "sqlite"
    }

    fn schema(&self) -> Result<EngineSchema, String> {
        let db = self.open()?;
        Ok(EngineSchema {
            search_fields: db.field_catalog_cached(),
            result_fields: db.result_fields_cached(),
        })
    }

    fn count(&self, query: &Query) -> Result<u64, String> {
        self.open()?.count(query).map_err(|err| err.to_string())
    }

    fn search_page(
        &self,
        query: &Query,
        limit: u64,
        offset: u64,
    ) -> Result<EngineSearchPage, String> {
        let (fields, mut ids, mut rows, mut duplicate_of) = self
            .open()?
            .search_page_dynamic(query, limit.saturating_add(1), offset)
            .map_err(|err| err.to_string())?;
        let has_next = rows.len() as u64 > limit;
        if has_next {
            ids.truncate(limit as usize);
            rows.truncate(limit as usize);
            duplicate_of.truncate(limit as usize);
        }
        Ok(EngineSearchPage {
            fields,
            ids,
            rows,
            duplicate_of,
            has_next,
        })
    }
}

impl AnalyticsEngine for SqliteEngine {
    fn engine_id(&self) -> &'static str {
        "sqlite"
    }

    fn analytics(
        &self,
        query: &Query,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
    ) -> Result<Analytics, String> {
        self.open()?
            .analytics_scoped(query, limit, scope, hs_level)
            .map_err(|err| err.to_string())
    }

    fn pivot(
        &self,
        query: &Query,
        row_dim: PivotDim,
        col_dim: PivotDim,
        metric: PivotMetric,
        limits: PivotLimits,
        others_label: &str,
    ) -> Result<crate::db::PivotResult, String> {
        self.open()?
            .pivot(query, row_dim, col_dim, metric, limits, others_label)
            .map_err(|err| err.to_string())
    }
}

impl ImportSink for SqliteEngine {
    fn engine_id(&self) -> &'static str {
        "sqlite"
    }
}

#[cfg(feature = "duckdb-olap")]
#[derive(Clone, Debug)]
pub struct DuckDbAnalyticsEngine {
    sqlite_path: PathBuf,
    projection_path: PathBuf,
}

#[cfg(feature = "duckdb-olap")]
impl DuckDbAnalyticsEngine {
    pub fn new(sqlite_path: impl Into<PathBuf>, projection_path: impl Into<PathBuf>) -> Self {
        Self {
            sqlite_path: sqlite_path.into(),
            projection_path: projection_path.into(),
        }
    }

    pub fn projection_path(&self) -> &Path {
        &self.projection_path
    }

    pub fn is_current(&self) -> Result<bool, String> {
        crate::duckdb_olap::projection_is_current(&self.sqlite_path, &self.projection_path)
    }

    fn ensure_usable(&self, query: &Query) -> Result<(), String> {
        if !query.text.trim().is_empty() {
            return Err("DuckDB analytics does not execute FTS text queries.".to_string());
        }
        if !crate::duckdb_olap::supports_projection_query(query) {
            return Err("DuckDB analytics does not support this advanced query.".to_string());
        }
        if !self.projection_path.exists() || !self.is_current()? {
            return Err("DuckDB analytics projection is missing or stale.".to_string());
        }
        Ok(())
    }
}

#[cfg(feature = "duckdb-olap")]
impl AnalyticsEngine for DuckDbAnalyticsEngine {
    fn engine_id(&self) -> &'static str {
        "duckdb"
    }

    fn analytics(
        &self,
        query: &Query,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
    ) -> Result<Analytics, String> {
        self.ensure_usable(query)?;
        crate::duckdb_olap::analytics_scoped(&self.projection_path, query, limit, scope, hs_level)
    }

    fn pivot(
        &self,
        query: &Query,
        row_dim: PivotDim,
        col_dim: PivotDim,
        metric: PivotMetric,
        limits: PivotLimits,
        others_label: &str,
    ) -> Result<crate::db::PivotResult, String> {
        // Pivot is not accelerated until an exact DuckDB implementation exists.
        // Delegating to the source-of-truth adapter is the safe contract fallback.
        AnalyticsEngine::pivot(
            &SqliteEngine::new(&self.sqlite_path),
            query,
            row_dim,
            col_dim,
            metric,
            limits,
            others_label,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_engine_exposes_schema_and_empty_count() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("engine.db");
        Db::open(&db_path).unwrap();

        let engine = SqliteEngine::new(&db_path);
        assert_eq!(SearchEngine::engine_id(&engine), "sqlite");
        let schema = engine.schema().unwrap();
        assert!(schema.search_fields.iter().any(|field| field.id == "year"));
        assert!(schema.result_fields.len() >= 10);
        assert_eq!(engine.count(&Query::default()).unwrap(), 0);
    }
}
