use std::path::PathBuf;

use crate::db::{
    Analytics, AnalyticsScope, AnalyticsSection, AnalyticsSectionKind, CompanyProfile, Db,
    PivotDim, PivotMetric, PivotResult, Query, StartupPhase, Undervaluation,
};
use crate::export::ExportError;
use crate::import::{FileSummary, ImportPhase};
use crate::search::FieldInfo;

pub struct StartupData {
    pub db: Box<Db>,
    pub lang_code: Option<String>,
    pub theme: Option<String>,
    pub zoom: Option<String>,
    pub hidden_cols: Option<String>,
    pub recent_queries_v1: Option<String>,
    pub saved_queries_v1: Option<String>,
    pub recent_queries_v2: Option<String>,
    pub saved_queries_v2: Option<String>,
    pub first_run: bool,
    pub result_fields: Vec<FieldInfo>,
    pub search_fields: Vec<FieldInfo>,
    pub total_rows: u64,
    pub unindexed_rows: u64,
}

pub enum WorkerReq {
    Search {
        q: Box<Query>,
        page: u64,
        generation: u64,
    },
    /// One analytics category for the current query; cheap enough to request
    /// lazily as the user switches tabs. `scope = None` loads only the overview
    /// and monthly dynamics.
    Analytics {
        q: Box<Query>,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
        generation: u64,
    },
    /// Full grouped list for one analytics card; loaded on demand for
    /// drill-down.
    AnalyticsSection {
        q: Box<Query>,
        kind: AnalyticsSectionKind,
        limit: u64,
        hs_level: u8,
        generation: u64,
    },
    /// Company dossier for one EDRPOU.
    Profile {
        edrpou: String,
        generation: u64,
    },
    /// Cross-tab of the current query.
    Pivot {
        q: Box<Query>,
        row_dim: PivotDim,
        col_dim: PivotDim,
        metric: PivotMetric,
        others_label: String,
        generation: u64,
    },
    /// Full analytics for the comparison side of Compare Mode.
    Compare {
        q: Box<Query>,
        generation: u64,
    },
    /// Undervaluation scan over the current query.
    Underpricing {
        q: Box<Query>,
        threshold: f64,
        generation: u64,
    },
    Stats,
}

#[derive(Clone)]
pub struct ImportEvent {
    pub file_idx: usize,
    pub file_count: usize,
    pub file_name: String,
    pub phase: ImportPhase,
    pub done: u64,
    pub total: u64,
}

pub enum Msg {
    SearchPage {
        generation: u64,
        fields: Vec<FieldInfo>,
        ids: Vec<i64>,
        rows: Vec<Vec<String>>,
        /// Per row: Some(first file) if it is a kept duplicate, else None.
        dups: Vec<Option<String>>,
        has_next: bool,
        ms: u64,
    },
    SearchCount {
        generation: u64,
        total: u64,
    },
    SearchError {
        generation: u64,
        message: String,
    },
    AnalyticsDone {
        generation: u64,
        scope: Option<AnalyticsScope>,
        analytics: Box<Analytics>,
    },
    AnalyticsSectionDone {
        generation: u64,
        section: Box<AnalyticsSection>,
    },
    ProfileDone {
        generation: u64,
        profile: Box<CompanyProfile>,
    },
    PivotDone {
        generation: u64,
        pivot: Box<PivotResult>,
    },
    CompareDone {
        generation: u64,
        query: Box<Query>,
        analytics: Box<Analytics>,
    },
    CompareError {
        generation: u64,
        message: String,
    },
    UnderpricingDone {
        generation: u64,
        result: Box<Undervaluation>,
    },
    Stats(u64),
    Import(ImportEvent),
    ImportDone(Vec<FileSummary>, u64),
    ExportProgress(u64, u64),
    ExportDone(Result<(u64, PathBuf), ExportError>),
    DbCleared(Result<(), String>),
    MaintenanceDone(Result<String, String>),
    /// What opening the database is doing, so a first-open upgrade shows its
    /// phase instead of an unexplained spinner.
    StartupProgress(StartupPhase),
    StartupDone(Result<StartupData, String>),
    Fatal(String),
}

pub const PAGE_SIZE: u64 = 100;
