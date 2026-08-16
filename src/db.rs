//! Public SQLite facade.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::domain::table::{
    ImportSource, SemanticField, SourceSchema, SourceSchemaField, TableShape, validate_fixed_value,
};
use crate::search::{
    FieldInfo, field_catalog_for_context, field_catalog_for_source_fields,
    result_field_catalog_for_context, result_field_catalog_for_source_fields,
};
use crate::storage::extra::{parse_extra, remember_extra_header};
use crate::storage::normalize::normalize_text_key;
use crate::storage::{
    analytics_repo, connection as storage_connection, fts_index, import_log, maintenance, meta,
    migrations, query_plan, record_writer, result_repo, source_mapping_profiles, source_schemas,
    table_shape,
};

pub use crate::db_types::*;
pub use crate::storage::maintenance::{
    DatabaseStorageInfo, DuplicateCompactionOptions, DuplicateCompactionProgress,
    DuplicateCompactionReport, WalCheckpointInfo,
};
pub use crate::storage::normalize::{
    NumberStyle, extract_year, parse_number, parse_number_grouped, parse_number_styled,
};
pub use crate::storage::records::canonical_record_hash;
pub use crate::storage::search_text::{build_fts_query, contains_ci, fts_prefix_terms};
pub use crate::storage::source_mapping_profiles::{
    SourceMappingColumn, SourceMappingProfile, SourceMappingProfileCollection,
    SourceMappingProfileCorruption, SourceMappingProfileError, SourceMappingProfileUpsert,
    source_mapping_signature,
};

/// Sentinel error returned by [`Db::with_statement_deadline`] when the
/// deadline interrupted the running statement.
pub const STATEMENT_DEADLINE_EXCEEDED: &str = "statement_deadline_exceeded";

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Runs `f` under a wall-clock deadline: SQLite's progress handler
    /// interrupts any statement still executing when the deadline passes.
    /// Returns [`STATEMENT_DEADLINE_EXCEEDED`] in that case, so callers can
    /// answer with an actionable message instead of blocking a worker thread
    /// for an unbounded broad-scope query.
    pub fn with_statement_deadline<T>(
        &self,
        timeout: std::time::Duration,
        f: impl FnOnce(&Db) -> Result<T, String>,
    ) -> Result<T, String> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let deadline = std::time::Instant::now() + timeout;
        let installed = self
            .conn
            .progress_handler(
                10_000,
                Some(move || {
                    if std::time::Instant::now() >= deadline {
                        flag.store(true, Ordering::Relaxed);
                        true
                    } else {
                        false
                    }
                }),
            )
            .is_ok();
        let result = f(self);
        if installed {
            let _ = self.conn.progress_handler(0, None::<fn() -> bool>);
        }
        if result.is_err() && fired.load(Ordering::Relaxed) {
            return Err(STATEMENT_DEADLINE_EXCEEDED.to_string());
        }
        result
    }

    pub fn open(path: &Path) -> Result<Db, String> {
        Ok(Db {
            conn: storage_connection::open(path)?,
        })
    }

    /// Opens a database and reports each phase of a first-open upgrade.
    ///
    /// A database carried over from an older version is rebuilt on this call,
    /// which is minutes of work on a large one. Without the callback the only
    /// account of it went to stderr, which a windowed release build does not
    /// have, so the window sat on a spinner for the whole upgrade.
    pub fn open_with_progress(
        path: &Path,
        progress: &mut dyn FnMut(StartupPhase),
    ) -> Result<Db, String> {
        Ok(Db {
            conn: storage_connection::open_with_progress(path, progress)?,
        })
    }

    pub fn open_runtime(path: &Path) -> Result<Db, String> {
        Ok(Db {
            conn: storage_connection::open_runtime(path)?,
        })
    }

    // ---------- meta ----------

    pub fn meta_get(&self, key: &str) -> Option<String> {
        meta::get(&self.conn, key)
    }

    pub fn meta_set(&self, key: &str, value: &str) {
        meta::set(&self.conn, key, value);
    }

    fn meta_get_i64(&self, key: &str) -> i64 {
        meta::get_i64(&self.conn, key)
    }

    pub fn diagnostic_execute_batch(&self, sql: &str) -> rusqlite::Result<()> {
        self.conn.execute_batch(sql)
    }

    pub fn diagnostic_execute(&self, sql: &str) -> rusqlite::Result<usize> {
        self.conn.execute(sql, [])
    }

    pub fn diagnostic_query_rows(
        &self,
        sql: &str,
        max_rows: usize,
    ) -> rusqlite::Result<Vec<Vec<String>>> {
        let mut stmt = self.conn.prepare(sql)?;
        let n_cols = stmt.column_count();
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while out.len() < max_rows {
            let Some(row) = rows.next()? else {
                break;
            };
            let mut cells = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                cells.push(sql_value_to_text(row.get::<_, Value>(i)?));
            }
            out.push(cells);
        }
        Ok(out)
    }

    // ---------- insert ----------

    pub fn begin_import_file(&mut self) -> rusqlite::Result<()> {
        record_writer::begin_import_file(&self.conn)
    }

    pub fn commit_import_file(&mut self) -> rusqlite::Result<()> {
        record_writer::commit_import_file(&self.conn)
    }

    pub fn rollback_import_file(&mut self) {
        record_writer::rollback_import_file(&self.conn);
    }

    /// Inserts a row batch that belongs to no registered source schema.
    /// Duplicates are inserted and flagged. Returns (inserted rows, duplicates).
    ///
    /// Imports use `insert_batch_for_source`; this is the schema-less path the
    /// test fixtures and pre-2.0 databases use.
    pub fn insert_batch(
        &mut self,
        source_file: &str,
        records: &[ImportRecord],
    ) -> rusqlite::Result<(u64, u64)> {
        record_writer::insert_batch(&self.conn, source_file, records)
    }

    pub(crate) fn register_import_source_schema(
        &self,
        raw_headers: &[String],
        shape: &TableShape,
        fixed_values: &std::collections::BTreeMap<SemanticField, String>,
        source_file: &str,
        table_name: &str,
        import_fingerprint: &str,
    ) -> rusqlite::Result<(SourceSchema, ImportSource)> {
        let source_schema =
            source_schemas::register_schema(&self.conn, raw_headers, shape, fixed_values)?;
        let source = source_schemas::register_import_source(
            &self.conn,
            source_schema.id,
            source_file,
            table_name,
            import_fingerprint,
        )?;
        Ok((source_schema, source))
    }

    pub(crate) fn insert_batch_for_source(
        &mut self,
        source_file: &str,
        schema_id: i64,
        source_id: i64,
        records: &[ImportRecord],
    ) -> rusqlite::Result<(u64, u64)> {
        record_writer::insert_batch_scoped(
            &self.conn,
            source_file,
            Some(schema_id),
            Some(source_id),
            records,
        )
    }

    // ---------- FTS ----------

    /// Indexes all rows with an id above the watermark.
    /// Returns (indexed rows, cancelled).
    pub fn index_fts(
        &mut self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64, u64),
    ) -> rusqlite::Result<(u64, bool)> {
        fts_index::index(&mut self.conn, cancel, &mut progress)
    }

    pub fn repair_fts(
        &mut self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<FtsRepairReport, FtsRepairError> {
        fts_index::repair(&mut self.conn, cancel, &mut progress)
    }

    /// Number of rows not yet present in the search index.
    pub fn unindexed_rows(&self) -> u64 {
        fts_index::unindexed_rows(&self.conn)
    }

    /// Searchable field catalog for the current database, including imported
    /// source columns preserved in each row's canonical fields or JSON payload.
    pub fn field_catalog(&self) -> rusqlite::Result<Vec<FieldInfo>> {
        let source_fields = source_schemas::list_fields(&self.conn, None)?;
        if !source_fields.is_empty() {
            let legacy = if source_schemas::has_legacy_rows(&self.conn)? {
                field_catalog_for_context(
                    table_shape::get(&self.conn).as_ref(),
                    self.extra_headers()?,
                )
            } else {
                Vec::new()
            };
            return Ok(field_catalog_for_source_fields(source_fields, legacy));
        }
        let extra_headers = self.extra_headers()?;
        Ok(field_catalog_for_context(
            self.table_shape().as_ref(),
            extra_headers,
        ))
    }

    pub fn result_fields(&self) -> rusqlite::Result<Vec<FieldInfo>> {
        let source_fields = source_schemas::list_fields(&self.conn, None)?;
        if !source_fields.is_empty() {
            let legacy = if source_schemas::has_legacy_rows(&self.conn)? {
                result_field_catalog_for_context(
                    table_shape::get(&self.conn).as_ref(),
                    self.extra_headers()?,
                )
            } else {
                Vec::new()
            };
            return Ok(result_field_catalog_for_source_fields(
                source_fields,
                legacy,
            ));
        }
        let extra_headers = self.extra_headers()?;
        Ok(result_field_catalog_for_context(
            self.table_shape().as_ref(),
            extra_headers,
        ))
    }

    pub fn field_catalog_cached(&self) -> Vec<FieldInfo> {
        if let Ok(source_fields) = source_schemas::list_fields(&self.conn, None)
            && !source_fields.is_empty()
        {
            let legacy = source_schemas::has_legacy_rows(&self.conn)
                .ok()
                .filter(|has_legacy| *has_legacy)
                .map(|_| {
                    field_catalog_for_context(
                        table_shape::get(&self.conn).as_ref(),
                        self.cached_extra_headers(),
                    )
                })
                .unwrap_or_default();
            return field_catalog_for_source_fields(source_fields, legacy);
        }
        let extra_headers = self.cached_extra_headers();
        field_catalog_for_context(self.table_shape().as_ref(), extra_headers)
    }

    pub fn result_fields_cached(&self) -> Vec<FieldInfo> {
        if let Ok(source_fields) = source_schemas::list_fields(&self.conn, None)
            && !source_fields.is_empty()
        {
            let legacy = source_schemas::has_legacy_rows(&self.conn)
                .ok()
                .filter(|has_legacy| *has_legacy)
                .map(|_| {
                    result_field_catalog_for_context(
                        table_shape::get(&self.conn).as_ref(),
                        self.cached_extra_headers(),
                    )
                })
                .unwrap_or_default();
            return result_field_catalog_for_source_fields(source_fields, legacy);
        }
        let extra_headers = self.cached_extra_headers();
        result_field_catalog_for_context(self.table_shape().as_ref(), extra_headers)
    }

    pub fn table_shape(&self) -> Option<TableShape> {
        source_schemas::compatibility_shape(&self.conn)
            .ok()
            .flatten()
            .or_else(|| table_shape::get(&self.conn))
    }

    pub fn remember_table_shape(&self, shape: &TableShape) -> TableShape {
        table_shape::merge(&self.conn, shape)
    }

    pub fn list_source_schemas(&self) -> rusqlite::Result<Vec<SourceSchema>> {
        source_schemas::list(&self.conn)
    }

    pub fn get_source_schema(&self, public_id: &str) -> rusqlite::Result<Option<SourceSchema>> {
        source_schemas::get(&self.conn, public_id)
    }

    pub fn list_source_fields(
        &self,
        schema_public_id: Option<&str>,
    ) -> rusqlite::Result<Vec<SourceSchemaField>> {
        source_schemas::list_fields(&self.conn, schema_public_id)
    }

    pub fn list_import_sources(&self) -> rusqlite::Result<Vec<ImportSource>> {
        source_schemas::list_import_sources(&self.conn)
    }

    pub fn get_import_source(&self, public_id: &str) -> rusqlite::Result<Option<ImportSource>> {
        source_schemas::get_import_source(&self.conn, public_id)
    }

    /// Assigns or clears the analytical meaning of a shape column by id, so the
    /// user can tell analytics which generic column is the value, country, etc.
    /// Returns true when the column existed. Used by the column-mapping UI.
    pub fn set_column_semantic(
        &self,
        column_id: &str,
        semantic: Option<crate::domain::table::SemanticField>,
    ) -> bool {
        // Registered source schemas own the shape now: write the meaning
        // through to every physical field behind the compatibility column.
        if let Ok(Some((_, backing))) = source_schemas::compatibility_shape_with_fields(&self.conn)
        {
            return match backing.get(column_id) {
                Some(field_ids) => {
                    source_schemas::set_fields_semantic(&self.conn, field_ids, semantic)
                        .unwrap_or(false)
                }
                None => false,
            };
        }
        // Legacy metadata-blob shape for databases imported before schemas.
        let Some(mut shape) = table_shape::get(&self.conn) else {
            return false;
        };
        let Some(column) = shape
            .columns
            .iter_mut()
            .find(|column| column.id == column_id)
        else {
            return false;
        };
        column.semantic = semantic;
        table_shape::set(&self.conn, &shape);
        true
    }

    /// The currency and weight unit pinned for this workspace, or `None` for a
    /// field the registered schemas do not agree on.
    pub fn workspace_fixed_values(&self) -> (Option<String>, Option<String>) {
        source_schemas::workspace_fixed_values(&self.conn).unwrap_or((None, None))
    }

    /// Pins the currency and weight unit a source does not state itself, after
    /// the data has already been imported. `None` clears one.
    ///
    /// This is the remedy the analytics hint points at: without it, a customs
    /// export — which carries an invoice amount and no currency code anywhere —
    /// could only ever be told its currency at import time, so an existing
    /// database was stuck reading "unknown currency" with no way back.
    pub fn set_workspace_fixed_values(
        &self,
        currency: Option<&str>,
        weight_unit: Option<&str>,
    ) -> Result<usize, String> {
        let currency = currency
            .map(|value| validate_fixed_value(SemanticField::Currency, value))
            .transpose()?;
        let weight_unit = weight_unit
            .map(|value| validate_fixed_value(SemanticField::WeightUnit, value))
            .transpose()?;
        source_schemas::set_workspace_fixed_values(
            &self.conn,
            currency.as_deref(),
            weight_unit.as_deref(),
        )
        .map_err(|error| error.to_string())
    }

    /// Asks the data what currency each schema's value column is in, and
    /// records the answer. Runs after an import commits, when the rows exist.
    ///
    /// Nothing here overwrites a currency a person stated: that lives in a
    /// separate column and outranks this one when analytics resolves it.
    pub fn refresh_detected_currencies(&self) {
        let Ok(schemas) = source_schemas::list(&self.conn) else {
            return;
        };
        for schema in schemas {
            match source_schemas::detect_schema_currency(&self.conn, schema.id) {
                Ok(detected) => {
                    let _ = source_schemas::set_detected_currency(
                        &self.conn,
                        schema.id,
                        detected.as_deref(),
                    );
                }
                Err(error) => {
                    eprintln!("[base-search] could not read the currency evidence: {error}");
                }
            }
        }
    }

    // ---------- reusable source mappings ----------

    pub fn list_source_mapping_profiles(
        &self,
    ) -> Result<SourceMappingProfileCollection, SourceMappingProfileError> {
        source_mapping_profiles::list(&self.conn)
    }

    pub fn get_source_mapping_profile(
        &self,
        id: i64,
    ) -> Result<Option<SourceMappingProfile>, SourceMappingProfileError> {
        source_mapping_profiles::get(&self.conn, id)
    }

    pub fn suggest_source_mapping_profiles(
        &self,
        signature: &str,
    ) -> Result<SourceMappingProfileCollection, SourceMappingProfileError> {
        source_mapping_profiles::suggest(&self.conn, signature)
    }

    pub fn upsert_source_mapping_profile(
        &self,
        profile: SourceMappingProfileUpsert,
    ) -> Result<SourceMappingProfile, SourceMappingProfileError> {
        source_mapping_profiles::upsert(&self.conn, profile)
    }

    pub fn delete_source_mapping_profile(
        &self,
        id: i64,
    ) -> Result<bool, SourceMappingProfileError> {
        source_mapping_profiles::delete(&self.conn, id)
    }

    pub fn extra_headers(&self) -> rusqlite::Result<Vec<String>> {
        if let Some(cached) = self.extra_headers_cache() {
            return Ok(cached);
        }
        let headers = self.scan_extra_headers()?;
        self.store_extra_headers(&headers);
        Ok(headers)
    }

    pub fn cached_extra_headers(&self) -> Vec<String> {
        self.extra_headers_cache().unwrap_or_default()
    }

    pub fn remember_extra_headers<I, S>(&self, headers: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let existing = self.extra_headers_cache().unwrap_or_default();
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for header in existing.iter().map(String::as_str) {
            remember_extra_header(&mut seen, &mut merged, header);
        }
        for header in headers {
            remember_extra_header(&mut seen, &mut merged, header.as_ref());
        }
        self.store_extra_headers(&merged);
    }

    fn extra_headers_cache(&self) -> Option<Vec<String>> {
        meta::get_string_vec(&self.conn, meta::EXTRA_HEADERS_KEY)
    }

    fn store_extra_headers(&self, headers: &[String]) {
        meta::set_string_vec(&self.conn, meta::EXTRA_HEADERS_KEY, headers);
    }

    fn scan_extra_headers(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT extra FROM records
             WHERE extra IS NOT NULL AND TRIM(extra) <> ''",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
        let mut seen = HashSet::new();
        let mut headers = Vec::new();
        for raw in rows {
            for (header, _) in parse_extra(raw?.as_deref()) {
                let key = normalize_text_key(&header);
                if !key.is_empty() && seen.insert(key) {
                    headers.push(header);
                }
            }
        }
        Ok(headers)
    }

    // ---------- search ----------

    fn filter_plan(&self, q: &Query) -> rusqlite::Result<query_plan::FilterPlan> {
        let shape = table_shape::effective(&self.conn);
        let source_fields = source_schemas::field_lookup(&self.conn)?;
        query_plan::build_filter_plan(
            q,
            q.record_scope == RecordScope::Canonical,
            self.meta_get_i64("fts_watermark"),
            shape.as_ref(),
            &source_fields,
        )
    }

    pub fn capture_search_snapshot(&self) -> rusqlite::Result<u64> {
        result_repo::capture_search_snapshot(&self.conn)
    }

    pub fn count(&self, q: &Query) -> rusqlite::Result<u64> {
        self.count_at_snapshot(q, self.capture_search_snapshot()?)
    }

    pub fn count_at_snapshot(&self, q: &Query, snapshot: u64) -> rusqlite::Result<u64> {
        result_repo::count(&self.conn, self.filter_plan(q)?, snapshot)
    }

    /// Legacy fixed-schema result page.
    pub fn search_page(&self, q: &Query, limit: u64, offset: u64) -> rusqlite::Result<SearchPage> {
        let snapshot = self.capture_search_snapshot()?;
        result_repo::legacy_search_page(
            &self.conn,
            q,
            self.filter_plan(q)?,
            snapshot,
            limit,
            offset,
        )
    }

    pub fn search_page_dynamic(
        &self,
        q: &Query,
        limit: u64,
        offset: u64,
    ) -> rusqlite::Result<DynamicSearchPage> {
        self.search_page_dynamic_sorted(q, limit, offset, None)
    }

    /// Result fields for a query: schema-exact when the query addresses
    /// registered source fields directly, else the folded compatibility
    /// catalog every plain search uses.
    pub fn result_fields_for_query(&self, q: &Query) -> rusqlite::Result<Vec<FieldInfo>> {
        let lookup = source_schemas::field_lookup(&self.conn)?;
        self.result_fields_for_query_with(q, &lookup)
    }

    fn result_fields_for_query_with(
        &self,
        q: &Query,
        lookup: &std::collections::HashMap<String, SourceSchemaField>,
    ) -> rusqlite::Result<Vec<FieldInfo>> {
        if let Some(advanced) = &q.advanced {
            let mut schema_ids: Vec<i64> = crate::search::source_field_ids(advanced)
                .iter()
                .filter_map(|field_id| lookup.get(field_id))
                .map(|field| field.schema_id)
                .collect();
            schema_ids.sort_unstable();
            schema_ids.dedup();
            if !schema_ids.is_empty() {
                let mut fields = Vec::new();
                for schema_id in schema_ids {
                    fields.extend(source_schemas::list_fields_by_schema_id(
                        &self.conn, schema_id,
                    )?);
                }
                return Ok(crate::search::schema_exact_field_infos(&fields));
            }
        }
        Ok(self.result_fields_cached())
    }

    /// Result page with an optional user-chosen column ordering. `sort = None`
    /// keeps the default recency order.
    pub fn search_page_dynamic_sorted(
        &self,
        q: &Query,
        limit: u64,
        offset: u64,
        sort: Option<ResultSort>,
    ) -> rusqlite::Result<DynamicSearchPage> {
        let snapshot = self.capture_search_snapshot()?;
        self.search_page_dynamic_sorted_at_snapshot(q, limit, offset, sort, snapshot)
    }

    pub fn search_page_dynamic_sorted_at_snapshot(
        &self,
        q: &Query,
        limit: u64,
        offset: u64,
        sort: Option<ResultSort>,
        snapshot: u64,
    ) -> rusqlite::Result<DynamicSearchPage> {
        let source_fields = source_schemas::field_lookup(&self.conn)?;
        let fields = self.result_fields_for_query_with(q, &source_fields)?;
        result_repo::dynamic_search_page(
            &self.conn,
            q,
            fields,
            self.filter_plan(q)?,
            snapshot,
            limit,
            offset,
            sort.as_ref(),
            &source_fields,
        )
    }

    /// Legacy fixed-schema export row batch using keyset pagination by id.
    pub fn export_batch(
        &self,
        q: &Query,
        last_id: i64,
        limit: u64,
    ) -> rusqlite::Result<(i64, Vec<Vec<String>>)> {
        result_repo::legacy_export_batch(&self.conn, self.filter_plan(q)?, last_id, limit)
    }

    pub fn export_batch_dynamic(
        &self,
        q: &Query,
        last_id: i64,
        limit: u64,
    ) -> rusqlite::Result<(Vec<FieldInfo>, i64, Vec<Vec<String>>)> {
        let fields = self.result_fields_cached();
        let (max_id, data) = self.export_batch_fields(q, last_id, limit, &fields)?;
        Ok((fields, max_id, data))
    }

    pub fn export_batch_fields(
        &self,
        q: &Query,
        last_id: i64,
        limit: u64,
        fields: &[FieldInfo],
    ) -> rusqlite::Result<(i64, Vec<Vec<String>>)> {
        let source_fields = source_schemas::field_lookup(&self.conn)?;
        result_repo::export_batch_fields(
            &self.conn,
            fields,
            self.filter_plan(q)?,
            last_id,
            limit,
            &source_fields,
        )
    }

    pub fn visit_export_rows_fields(
        &self,
        q: &Query,
        fields: &[FieldInfo],
        sort_catalog: &[FieldInfo],
        sort: Option<&ResultSort>,
        visit: impl FnMut(Vec<String>) -> bool,
    ) -> rusqlite::Result<u64> {
        let source_fields = source_schemas::field_lookup(&self.conn)?;
        result_repo::visit_export_rows_fields(
            &self.conn,
            q,
            fields,
            sort_catalog,
            self.filter_plan(q)?,
            sort,
            visit,
            &source_fields,
        )
    }

    /// Full record card by id.
    pub fn record_card(&self, id: i64) -> rusqlite::Result<RecordCard> {
        let Some(schema_id) = source_schemas::schema_id_for_record(&self.conn, id)? else {
            return result_repo::legacy_record_card(&self.conn, id);
        };
        let source_schema = source_schemas::get_by_id(&self.conn, schema_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let fields = result_field_catalog_for_source_fields(source_schema.columns, Vec::new());
        let source_fields = source_schemas::field_lookup(&self.conn)?;
        result_repo::record_card(&self.conn, fields, id, &source_fields)
    }

    // ---------- analytics ----------

    /// Full analytics across every scope (used by the CLI and tests).
    /// The GUI requests one scope at a time via [`Db::analytics_scoped`],
    /// which is several times cheaper on broad queries.
    pub fn analytics(&self, q: &Query, limit: u64) -> rusqlite::Result<Analytics> {
        // All four scopes answer the same query, so the overview and the month
        // series are identical for each one. Computing them once instead of per
        // scope removes eighteen redundant full scans from the report, without
        // changing a single number: the inputs were already the same.
        let overview = self.analytics_overview(q)?;
        let months = self.analytics_months(q, &overview.measures)?;
        let basis = (overview, months);
        let mut analytics = self.analytics_scoped_with(
            q,
            limit,
            Some(AnalyticsScope::Companies),
            10,
            Some(basis.clone()),
        )?;
        let products = self.analytics_scoped_with(
            q,
            limit,
            Some(AnalyticsScope::Products),
            10,
            Some(basis.clone()),
        )?;
        let countries = self.analytics_scoped_with(
            q,
            limit,
            Some(AnalyticsScope::Countries),
            10,
            Some(basis.clone()),
        )?;
        let prices =
            self.analytics_scoped_with(q, limit, Some(AnalyticsScope::Prices), 10, Some(basis))?;
        analytics.product_sections = products.product_sections;
        analytics.top_trademarks = products.top_trademarks;
        analytics.top_product_codes = products.top_product_codes;
        analytics.country_sections = countries.country_sections;
        analytics.top_origin_countries = countries.top_origin_countries;
        analytics.price_sections = prices.price_sections;
        Ok(analytics)
    }

    /// Overview, monthly dynamics and the sections of a single scope.
    /// `scope = None` computes only the overview and months (for the
    /// Overview tab). `hs_level` groups product codes by their first
    /// 2/4/6 digits; 10 keeps full codes.
    pub fn analytics_scoped(
        &self,
        q: &Query,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
    ) -> rusqlite::Result<Analytics> {
        self.analytics_scoped_with(q, limit, scope, hs_level, None)
    }

    /// `basis` supplies an already-computed overview and month series for this
    /// exact query, so a caller filling several scopes pays for them once.
    fn analytics_scoped_with(
        &self,
        q: &Query,
        limit: u64,
        scope: Option<AnalyticsScope>,
        hs_level: u8,
        basis: Option<(AnalyticsOverview, Vec<AnalyticsMonthRow>)>,
    ) -> rusqlite::Result<Analytics> {
        let (overview, months) = match basis {
            Some(basis) => basis,
            None => {
                let overview = self.analytics_overview(q)?;
                let months = self.analytics_months(q, &overview.measures)?;
                (overview, months)
            }
        };
        let mut analytics = Analytics {
            overview,
            months,
            ..Default::default()
        };
        let overview = &analytics.overview;
        match scope {
            None => {}
            Some(AnalyticsScope::Companies) => {
                analytics.company_sections = vec![
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::Edrpou,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::Recipients,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::Senders,
                        hs_level,
                        limit,
                        overview,
                    )?,
                ];
                analytics.top_recipients = section_rows(
                    &analytics.company_sections,
                    AnalyticsSectionKind::Recipients,
                );
                analytics.top_senders =
                    section_rows(&analytics.company_sections, AnalyticsSectionKind::Senders);
            }
            Some(AnalyticsScope::Products) => {
                analytics.product_sections = vec![
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::ProductCodes,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::Trademarks,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::ProductGroups,
                        hs_level,
                        limit,
                        overview,
                    )?,
                ];
                analytics.top_trademarks = section_rows(
                    &analytics.product_sections,
                    AnalyticsSectionKind::Trademarks,
                );
                analytics.top_product_codes = section_rows(
                    &analytics.product_sections,
                    AnalyticsSectionKind::ProductCodes,
                );
            }
            Some(AnalyticsScope::Countries) => {
                analytics.country_sections = vec![
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::OriginCountries,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::DispatchCountries,
                        hs_level,
                        limit,
                        overview,
                    )?,
                    self.analytics_section_with_overview(
                        q,
                        AnalyticsSectionKind::TradeCountries,
                        hs_level,
                        limit,
                        overview,
                    )?,
                ];
                analytics.top_origin_countries = section_rows(
                    &analytics.country_sections,
                    AnalyticsSectionKind::OriginCountries,
                );
            }
            Some(AnalyticsScope::Prices) => {
                analytics.price_sections = self.analytics_price_metrics(q)?;
            }
        }
        Ok(analytics)
    }

    /// Sections for one scope, without the month series.
    ///
    /// The Overview tab's preview cards read only `*_sections`, and the month
    /// series is a whole extra aggregate scan they discard. Callers that want
    /// the months must use [`Db::analytics_scoped`]; this returns an empty
    /// `months` on purpose rather than a partial one.
    pub fn analytics_sections_only(
        &self,
        q: &Query,
        limit: u64,
        scope: AnalyticsScope,
        hs_level: u8,
    ) -> rusqlite::Result<Analytics> {
        let overview = self.analytics_overview_basis(q)?;
        self.analytics_scoped_with(
            q,
            limit,
            Some(scope),
            hs_level,
            Some((overview, Vec::new())),
        )
    }

    /// The Overview tab's three preview cards in one pass.
    ///
    /// Each card renders the first section of one scope. Fetching them as three
    /// scoped requests made the server recompute the shared currency and weight
    /// buckets, a full overview and a month series for every one of them, and
    /// then discard two of the three sections each request produced.
    ///
    /// The company card previews recipients rather than company codes: a reader
    /// recognises "ТОВ «АЛЬФА»", not "33333333". Swap `Recipients` for `Edrpou`
    /// to go back to codes.
    pub fn analytics_previews(
        &self,
        q: &Query,
        limit: u64,
        hs_level: u8,
    ) -> rusqlite::Result<Analytics> {
        let overview = self.analytics_overview_basis(q)?;
        let company_sections = vec![self.analytics_section_with_overview(
            q,
            AnalyticsSectionKind::Recipients,
            hs_level,
            limit,
            &overview,
        )?];
        let product_sections = vec![self.analytics_section_with_overview(
            q,
            AnalyticsSectionKind::ProductCodes,
            hs_level,
            limit,
            &overview,
        )?];
        let country_sections = vec![self.analytics_section_with_overview(
            q,
            AnalyticsSectionKind::OriginCountries,
            hs_level,
            limit,
            &overview,
        )?];
        Ok(Analytics {
            overview,
            company_sections,
            product_sections,
            country_sections,
            ..Default::default()
        })
    }

    fn analytics_overview_basis(&self, q: &Query) -> rusqlite::Result<AnalyticsOverview> {
        analytics_repo::overview_basis(&self.conn, self.filter_plan(q)?)
    }

    pub fn analytics_section(
        &self,
        q: &Query,
        kind: AnalyticsSectionKind,
        hs_level: u8,
        limit: u64,
    ) -> rusqlite::Result<AnalyticsSection> {
        let overview = self.analytics_overview(q)?;
        self.analytics_section_with_overview(q, kind, hs_level, limit, &overview)
    }

    fn analytics_section_with_overview(
        &self,
        q: &Query,
        kind: AnalyticsSectionKind,
        hs_level: u8,
        limit: u64,
        overview: &AnalyticsOverview,
    ) -> rusqlite::Result<AnalyticsSection> {
        analytics_repo::section(
            &self.conn,
            self.filter_plan(q)?,
            kind,
            hs_level,
            limit,
            overview,
        )
    }

    fn analytics_overview(&self, q: &Query) -> rusqlite::Result<AnalyticsOverview> {
        analytics_repo::overview(&self.conn, self.filter_plan(q)?)
    }

    /// Import dynamics grouped by month ("YYYY-MM" from the ISO date).
    /// Returns the most recent 48 months in chronological order.
    fn analytics_months(
        &self,
        q: &Query,
        query_measures: &AnalyticsMeasures,
    ) -> rusqlite::Result<Vec<AnalyticsMonthRow>> {
        analytics_repo::months(&self.conn, self.filter_plan(q)?, query_measures)
    }

    /// Full dossier for one company (by EDRPOU): name variants, headline
    /// numbers, monthly dynamics, and the top products / suppliers / origin
    /// countries. Scoped to the company's rows, so it is fast thanks to the
    /// EDRPOU index even on a multi-million-row database.
    pub fn company_profile(&self, edrpou: &str, limit: u64) -> rusqlite::Result<CompanyProfile> {
        analytics_repo::company_profile(&self.conn, edrpou, limit)
    }

    /// Finds rows whose source value per kg is far below the median for the
    /// same product code — a classic signal of undervaluation. Only
    /// codes with at least `min_samples` priced rows are judged, so a lone
    /// single row cannot flag itself. Rows are returned most-undervalued first.
    pub fn undervaluation(
        &self,
        q: &Query,
        threshold: f64,
        min_samples: u64,
        limit: u64,
    ) -> rusqlite::Result<Undervaluation> {
        analytics_repo::undervaluation(
            &self.conn,
            self.filter_plan(q)?,
            threshold,
            min_samples,
            limit,
        )
    }

    /// Cross-tabulation of `row_dim` by `col_dim` for `metric`, over the rows
    /// matching the query. Rows are limited to the top `max_rows` by total and
    /// columns to the top `max_cols`; the remainder is folded into an "others"
    /// bucket so the matrix stays readable.
    pub fn pivot(
        &self,
        q: &Query,
        row_dim: PivotDim,
        col_dim: PivotDim,
        metric: PivotMetric,
        limits: PivotLimits,
        others_label: &str,
    ) -> rusqlite::Result<PivotResult> {
        analytics_repo::pivot(
            &self.conn,
            self.filter_plan(q)?,
            row_dim,
            col_dim,
            metric,
            limits,
            others_label,
        )
    }

    fn analytics_price_metrics(&self, q: &Query) -> rusqlite::Result<Vec<AnalyticsPriceMetric>> {
        analytics_repo::price_metrics(&self.conn, q, self.meta_get_i64("fts_watermark"))
    }

    // ---------- statistics ----------

    /// Drops the read-side indexes before a bulk load into an empty database,
    /// so several million rows are not inserted into each one individually.
    /// Safe to lose to a crash: every index is recreated on the next open.
    pub(crate) fn drop_read_indexes(&self) -> rusqlite::Result<()> {
        migrations::drop_read_indexes(&self.conn)
    }

    /// Rebuilds the read-side indexes in one sorted pass after a bulk load.
    pub(crate) fn create_read_indexes(&self) -> rusqlite::Result<()> {
        migrations::create_read_indexes(&self.conn)
    }

    pub fn total_rows(&self) -> u64 {
        record_writer::total_rows(&self.conn)
    }

    pub fn max_record_id(&self) -> u64 {
        record_writer::max_record_id(&self.conn)
    }

    pub fn add_import_log(&self, entry: ImportLogWrite<'_>) {
        import_log::add(&self.conn, entry);
    }

    pub fn storage_info(&self, db_path: &Path) -> rusqlite::Result<DatabaseStorageInfo> {
        maintenance::storage_info(&self.conn, db_path)
    }

    pub fn checkpoint_wal_truncate(&self) -> rusqlite::Result<WalCheckpointInfo> {
        maintenance::checkpoint_wal_truncate(&self.conn)
    }

    pub fn vacuum_database(&self) -> rusqlite::Result<()> {
        maintenance::vacuum_database(&self.conn)
    }

    pub fn compact_duplicate_payloads(
        &mut self,
        cancel: &AtomicBool,
        options: DuplicateCompactionOptions,
        progress: impl FnMut(DuplicateCompactionProgress),
    ) -> rusqlite::Result<DuplicateCompactionReport> {
        maintenance::compact_duplicate_payloads(&mut self.conn, cancel, options, progress)
    }

    /// Full cleanup: removes imported records and every source-shape artifact,
    /// then returns disk space via VACUUM. Workspace settings and accounts are
    /// preserved.
    pub fn clear_all(&mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> rusqlite::Result<()> {
            self.conn.execute_batch(
                "INSERT INTO records_fts(records_fts) VALUES('delete-all');
                 DELETE FROM records;
                 DELETE FROM import_log;
                 DELETE FROM import_sources;
                 DELETE FROM source_columns;
                 DELETE FROM source_schemas;",
            )?;
            meta::delete(&self.conn, meta::EXTRA_HEADERS_KEY)?;
            meta::delete(&self.conn, table_shape::TABLE_SHAPE_KEY)?;
            self.meta_set("fts_watermark", "0");
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT;")?,
            Err(err) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                return Err(err);
            }
        }
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    /// Name of a previously imported file with the same content.
    pub fn find_import_by_hash(&self, file_hash: &str) -> Option<String> {
        import_log::find_by_hash(&self.conn, file_hash)
    }

    pub fn import_log(&self, limit: u64) -> Vec<ImportLogEntry> {
        import_log::list(&self.conn, limit)
    }
}

fn section_rows(
    sections: &[AnalyticsSection],
    kind: AnalyticsSectionKind,
) -> Vec<AnalyticsGroupRow> {
    sections
        .iter()
        .find(|section| section.kind == kind)
        .map(|section| section.rows.clone())
        .unwrap_or_default()
}

fn sql_value_to_text(value: Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Integer(x) => x.to_string(),
        Value::Real(x) => x.to_string(),
        Value::Text(s) => s,
        Value::Blob(b) => format!("<blob {}>", b.len()),
    }
}
