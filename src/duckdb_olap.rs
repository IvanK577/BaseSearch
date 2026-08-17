use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use duckdb::{AccessMode, Config, Connection as DuckConnection, params as duck_params};
use rusqlite::types::ValueRef;
use rusqlite::{Connection as SqliteConnection, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::db::{
    Analytics, AnalyticsCurrencyTotal, AnalyticsFilterAction, AnalyticsFilterField,
    AnalyticsGroupRow, AnalyticsMeasureExclusions, AnalyticsMeasures, AnalyticsMonthRow,
    AnalyticsOverview, AnalyticsPriceCohort, AnalyticsPriceMetric, AnalyticsScope,
    AnalyticsSection, AnalyticsSectionKind, AnalyticsUsdCompatibility, AnalyticsValuePerWeight,
    AnalyticsWeightTotal, PriceMetricKind, Query, RecordScope,
};
use crate::domain::table::{SemanticField, SourceColumn, SourceSchema, TableShape};
use crate::olap::{OlapBenchmarkOptions, OlapBenchmarkReport, OlapScenarioReport};
use crate::storage::analytics_columns::{AnalyticsColumns, UNKNOWN_CURRENCY_KEY, UNKNOWN_UNIT_KEY};
// The SQLite analytics repository owns the inheritance rule for group and month
// measures and the length of the month series. Both are imported rather than
// re-implemented: a second copy of either rule is a second thing to keep in
// sync, and the whole point of the projection is that it answers identically.
use crate::storage::analytics_repo::{MONTH_SERIES_LIMIT, SubsetTotals, inherited_measures};
use crate::storage::{
    connection as storage_connection, effective_rows, source_schemas, table_shape,
};

fn projection_usd_compat(measures: &AnalyticsMeasures) -> Option<AnalyticsUsdCompatibility> {
    let total_value_usd = measures.compatible_usd_total()?;
    Some(AnalyticsUsdCompatibility {
        total_value_usd,
        avg_value_per_net_kg: measures.compatible_usd_per_net_kg(),
    })
}

fn inherited_projection_usd(
    query_is_usd: bool,
    total_value: f64,
    paired_value: f64,
    paired_weight_kg: f64,
) -> Option<AnalyticsUsdCompatibility> {
    query_is_usd.then(|| AnalyticsUsdCompatibility {
        total_value_usd: total_value,
        avg_value_per_net_kg: (paired_weight_kg > 0.0).then(|| paired_value / paired_weight_kg),
    })
}

// Bumped from 7: the projected `year` column now derives the year the way the
// SQLite query plan does. An existing projection built under the old rule would
// still look "current" and would quietly answer year filters differently.
pub const PROJECTION_SCHEMA_VERSION: &str = "8";
// Bumped from 2: `rollup_monthly` and `rollup_sections` gained the per-subset
// counters and source-unit sums that measure inheritance needs. Reading them
// from an older projection would fail on a missing column, so the version must
// invalidate it instead.
pub const ROLLUP_SCHEMA_VERSION: &str = "3";
pub const ROLLUP_RULES_VERSION: &str = "2";

const ROLLUP_CONTRACT: &str = concat!(
    "overview:v2;monthly:v3;sections:v3;currency:v2;price_per_kg:v2;",
    "scope:canonical|occurrences;years:all|calendar;",
    "money:currency-partitioned|usd-compat-only;weight:source-partitioned|normalized-kg;",
    "schema-context:per-row;hs:2-8|10;r7-quantiles"
);

const SOURCE_TRACKING_VERSION: &str = "1";
const SOURCE_STATE_TABLE: &str = "base_search_olap_state";
const SOURCE_TRIGGER_INSERT: &str = "base_search_olap_records_insert";
const SOURCE_TRIGGER_UPDATE: &str = "base_search_olap_records_update";
const SOURCE_TRIGGER_DELETE: &str = "base_search_olap_records_delete";
const SOURCE_TRIGGERS: [&str; 3] = [
    SOURCE_TRIGGER_DELETE,
    SOURCE_TRIGGER_INSERT,
    SOURCE_TRIGGER_UPDATE,
];
static BUILD_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PROJECTION_ACCESS: RwLock<()> = RwLock::new(());

struct ProjectionReadConnection {
    connection: DuckConnection,
    _guard: std::sync::RwLockReadGuard<'static, ()>,
}

impl std::ops::Deref for ProjectionReadConnection {
    type Target = DuckConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

#[derive(Debug, Clone)]
pub struct DuckProjectionBuild {
    pub projection_path: PathBuf,
    pub rows: u64,
    pub max_record_id: u64,
    pub schema_version: String,
    pub source_generation: String,
    pub source_fingerprint: String,
    pub rollup_schema_version: String,
    pub rollup_rules_version: String,
    pub rollup_fingerprint: String,
    pub built_at: String,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone)]
pub struct DuckProjectionMeta {
    pub source_sqlite: String,
    pub rows: u64,
    pub max_record_id: u64,
    pub schema_version: String,
    pub source_generation: String,
    pub source_fingerprint: String,
    pub rollup_schema_version: String,
    pub rollup_rules_version: String,
    pub rollup_fingerprint: String,
    pub built_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuckAnalyticsSource {
    Detail,
    Rollup,
}

struct SourceContract {
    generation: String,
    fingerprint: String,
}

pub fn default_projection_path(sqlite_path: &Path) -> PathBuf {
    let mut path = sqlite_path.to_path_buf();
    path.set_extension("duckdb");
    path
}

fn open_projection_read_only(path: &Path) -> Result<ProjectionReadConnection, String> {
    let guard = PROJECTION_ACCESS
        .read()
        .map_err(|_| "DuckDB projection access lock is poisoned.".to_string())?;
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)
        .map_err(|err| format!("Could not configure DuckDB read-only access: {err}"))?;
    let connection =
        DuckConnection::open_with_flags(path, config).map_err(|err| err.to_string())?;
    Ok(ProjectionReadConnection {
        connection,
        _guard: guard,
    })
}

pub fn build_projection_atomic(
    sqlite_path: &Path,
    projection_path: &Path,
) -> Result<DuckProjectionBuild, String> {
    build_projection_replacing(sqlite_path, projection_path, "cli")
}

fn temporary_projection_path(projection_path: &Path, label: &str) -> PathBuf {
    let sequence = BUILD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = projection_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "projection.duckdb".into());
    projection_path.with_file_name(format!(
        "{file_name}.{label}.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
}

fn replace_file_preserving_backup(
    source: &Path,
    destination: &Path,
    label: &str,
) -> Result<(), String> {
    let backup = destination.with_file_name(format!(
        "{}.{label}.backup",
        destination
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| "projection.duckdb".into())
    ));
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|err| format!("Could not remove stale backup {}: {err}", backup.display()))?;
    }
    let had_destination = destination.exists();
    if had_destination {
        std::fs::rename(destination, &backup).map_err(|err| {
            format!(
                "Could not move existing projection {} to backup {}: {err}",
                destination.display(),
                backup.display()
            )
        })?;
    }
    match std::fs::rename(source, destination) {
        Ok(()) => {
            if had_destination {
                let _ = std::fs::remove_file(&backup);
            }
            Ok(())
        }
        Err(err) => {
            if had_destination {
                std::fs::rename(&backup, destination).map_err(|restore_err| {
                    format!(
                        "Could not install DuckDB projection {}: {err}; the previous projection \
                         could not be restored from {}: {restore_err}",
                        destination.display(),
                        backup.display()
                    )
                })?;
            }
            Err(format!(
                "Could not install DuckDB projection {}: {err}",
                destination.display()
            ))
        }
    }
}

pub fn build_projection(
    sqlite_path: &Path,
    projection_path: &Path,
) -> Result<DuckProjectionBuild, String> {
    build_projection_replacing(sqlite_path, projection_path, "direct")
}

fn build_projection_replacing(
    sqlite_path: &Path,
    projection_path: &Path,
    label: &str,
) -> Result<DuckProjectionBuild, String> {
    let temp_path = temporary_projection_path(projection_path, label);
    if temp_path.exists() {
        std::fs::remove_file(&temp_path).map_err(|err| {
            format!(
                "Could not remove stale temporary DuckDB projection {}: {err}",
                temp_path.display()
            )
        })?;
    }
    let build = match build_projection_file(sqlite_path, &temp_path) {
        Ok(build) => build,
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err);
        }
    };
    match projection_is_current(sqlite_path, &temp_path) {
        Ok(true) => {}
        Ok(false) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(
                "SQLite changed while the DuckDB projection was being built; keeping the \
                 previous projection."
                    .to_string(),
            );
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!(
                "Could not validate the new DuckDB projection: {err}"
            ));
        }
    }
    let _projection_write = PROJECTION_ACCESS
        .write()
        .map_err(|_| "DuckDB projection access lock is poisoned.".to_string())?;
    if let Err(err) = replace_file_preserving_backup(&temp_path, projection_path, label) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(DuckProjectionBuild {
        projection_path: projection_path.to_path_buf(),
        ..build
    })
}

fn build_projection_file(
    sqlite_path: &Path,
    projection_path: &Path,
) -> Result<DuckProjectionBuild, String> {
    if projection_path.exists() {
        std::fs::remove_file(projection_path).map_err(|err| {
            format!(
                "Could not replace DuckDB projection {}: {err}",
                projection_path.display()
            )
        })?;
    }
    let started = Instant::now();
    let mut sqlite = storage_connection::open(sqlite_path)?;
    install_source_tracking(&sqlite)?;
    let transaction = sqlite
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("Could not lock SQLite for projection build: {err}"))?;
    prepare_source_generation(&transaction)?;
    let source_contract = source_contract(&transaction, sqlite_path)?;
    let duck = DuckConnection::open(projection_path).map_err(|err| err.to_string())?;
    prepare_projection_schema(&duck)?;

    let (inserted, max_record_id) = {
        let sql = projection_select_sql(&transaction)?;
        let mut stmt = transaction
            .prepare(&sql)
            .map_err(|err| format!("Could not read projection columns from SQLite: {err}"))?;
        let mut rows = stmt.query([]).map_err(|err| err.to_string())?;
        let mut appender = duck.appender("records").map_err(|err| err.to_string())?;
        let mut inserted = 0u64;
        let mut max_record_id = 0u64;
        while let Some(row) = rows.next().map_err(|err| err.to_string())? {
            let id: i64 = row.get(0).map_err(|err| err.to_string())?;
            max_record_id = max_record_id.max(id.max(0) as u64);
            let year: Option<i64> = row.get(1).map_err(|err| err.to_string())?;
            let declaration_number: Option<String> = row.get(2).map_err(|err| err.to_string())?;
            let sender_label: Option<String> = row.get(3).map_err(|err| err.to_string())?;
            let sender_text: Option<String> = row.get(4).map_err(|err| err.to_string())?;
            let recipient_label: Option<String> = row.get(5).map_err(|err| err.to_string())?;
            let recipient_text: Option<String> = row.get(6).map_err(|err| err.to_string())?;
            let edrpou_label: Option<String> = row.get(7).map_err(|err| err.to_string())?;
            let edrpou_key: Option<String> = row.get(8).map_err(|err| err.to_string())?;
            let product_code: Option<String> = row.get(9).map_err(|err| err.to_string())?;
            let product_code_text: Option<String> = row.get(10).map_err(|err| err.to_string())?;
            let description: Option<String> = row.get(11).map_err(|err| err.to_string())?;
            let description_text: Option<String> = row.get(12).map_err(|err| err.to_string())?;
            let trademark_label: Option<String> = row.get(13).map_err(|err| err.to_string())?;
            let trademark_key: Option<String> = row.get(14).map_err(|err| err.to_string())?;
            let origin_key: Option<String> = row.get(15).map_err(|err| err.to_string())?;
            let dispatch_key: Option<String> = row.get(16).map_err(|err| err.to_string())?;
            let trade_key: Option<String> = row.get(17).map_err(|err| err.to_string())?;
            let month: Option<String> = row.get(18).map_err(|err| err.to_string())?;
            let value_num: Option<f64> = row.get(19).map_err(|err| err.to_string())?;
            let net_kg_num: Option<f64> = row.get(20).map_err(|err| err.to_string())?;
            let gross_kg_num: Option<f64> = row.get(21).map_err(|err| err.to_string())?;
            let quantity_num: Option<f64> = row.get(22).map_err(|err| err.to_string())?;
            let rfv_num: Option<f64> = row.get(23).map_err(|err| err.to_string())?;
            let rmv_net_num: Option<f64> = row.get(24).map_err(|err| err.to_string())?;
            let rmv_extra_num: Option<f64> = row.get(25).map_err(|err| err.to_string())?;
            let rmv_gross_num: Option<f64> = row.get(26).map_err(|err| err.to_string())?;
            let min_base_num: Option<f64> = row.get(27).map_err(|err| err.to_string())?;
            let currency_key: Option<String> = row.get(28).map_err(|err| err.to_string())?;
            let weight_unit_key: Option<String> = row.get(29).map_err(|err| err.to_string())?;
            let dup_first_file: Option<String> = row.get(30).map_err(|err| err.to_string())?;
            appender
                .append_row(duck_params![
                    id,
                    year,
                    declaration_number,
                    sender_label,
                    sender_text,
                    recipient_label,
                    recipient_text,
                    edrpou_label,
                    edrpou_key,
                    product_code,
                    product_code_text,
                    description,
                    description_text,
                    trademark_label,
                    trademark_key,
                    origin_key,
                    dispatch_key,
                    trade_key,
                    month,
                    value_num,
                    net_kg_num,
                    gross_kg_num,
                    quantity_num,
                    rfv_num,
                    rmv_net_num,
                    rmv_extra_num,
                    rmv_gross_num,
                    min_base_num,
                    currency_key,
                    weight_unit_key,
                    dup_first_file,
                ])
                .map_err(|err| err.to_string())?;
            inserted += 1;
        }
        appender.flush().map_err(|err| err.to_string())?;
        (inserted, max_record_id)
    };
    build_rollups(&duck)?;
    let built_at = chrono::Utc::now().to_rfc3339();
    write_projection_meta(
        &duck,
        sqlite_path,
        inserted,
        max_record_id,
        &source_contract,
        &built_at,
    )?;
    drop(duck);
    transaction
        .commit()
        .map_err(|err| format!("Could not finish the SQLite projection snapshot: {err}"))?;
    Ok(DuckProjectionBuild {
        projection_path: projection_path.to_path_buf(),
        rows: inserted,
        max_record_id,
        schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
        source_generation: source_contract.generation,
        source_fingerprint: source_contract.fingerprint,
        rollup_schema_version: ROLLUP_SCHEMA_VERSION.to_string(),
        rollup_rules_version: ROLLUP_RULES_VERSION.to_string(),
        rollup_fingerprint: rollup_contract_fingerprint(),
        built_at,
        elapsed_ms: round_ms(started.elapsed().as_secs_f64() * 1000.0),
    })
}

pub fn read_projection_meta(projection_path: &Path) -> Result<DuckProjectionMeta, String> {
    let conn = open_projection_read_only(projection_path)?;
    require_rollup_schema(&conn)?;
    let source_sqlite = read_projection_meta_value(&conn, "source_sqlite")?;
    let rows = read_projection_meta_value(&conn, "rows")?
        .parse::<u64>()
        .map_err(|err| format!("Invalid projection row count: {err}"))?;
    let max_record_id = read_projection_meta_value(&conn, "max_record_id")
        .or_else(|_| read_projection_meta_value(&conn, "max_id"))?
        .parse::<u64>()
        .map_err(|err| format!("Invalid projection max record id: {err}"))?;
    let schema_version = read_projection_meta_value(&conn, "schema_version")?;
    let source_generation = read_projection_meta_value(&conn, "source_generation")?;
    let source_fingerprint = read_projection_meta_value(&conn, "source_fingerprint")?;
    let rollup_schema_version = read_projection_meta_value(&conn, "rollup_schema_version")?;
    let rollup_rules_version = read_projection_meta_value(&conn, "rollup_rules_version")?;
    let rollup_fingerprint = read_projection_meta_value(&conn, "rollup_fingerprint")?;
    let built_at = read_projection_meta_value(&conn, "built_at")?;
    Ok(DuckProjectionMeta {
        source_sqlite,
        rows,
        max_record_id,
        schema_version,
        source_generation,
        source_fingerprint,
        rollup_schema_version,
        rollup_rules_version,
        rollup_fingerprint,
        built_at,
    })
}

pub fn projection_is_current(sqlite_path: &Path, projection_path: &Path) -> Result<bool, String> {
    let meta = read_projection_meta(projection_path)?;
    if meta.schema_version != PROJECTION_SCHEMA_VERSION
        || meta.rollup_schema_version != ROLLUP_SCHEMA_VERSION
        || meta.rollup_rules_version != ROLLUP_RULES_VERSION
        || meta.rollup_fingerprint != rollup_contract_fingerprint()
    {
        return Ok(false);
    }
    let sqlite = storage_connection::open_runtime(sqlite_path)?;
    let source = source_contract(&sqlite, sqlite_path)?;
    Ok(
        meta.source_generation == source.generation
            && meta.source_fingerprint == source.fingerprint,
    )
}

fn install_source_tracking(conn: &SqliteConnection) -> Result<(), String> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {SOURCE_STATE_TABLE} (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            generation TEXT NOT NULL,
            dirty INTEGER NOT NULL CHECK(dirty IN (0, 1)),
            tracking_version TEXT NOT NULL
        );
        INSERT OR IGNORE INTO {SOURCE_STATE_TABLE}
            (singleton, generation, dirty, tracking_version)
        VALUES (1, lower(hex(randomblob(16))), 1, '{SOURCE_TRACKING_VERSION}');
        UPDATE {SOURCE_STATE_TABLE}
        SET generation = lower(hex(randomblob(16))),
            dirty = 1,
            tracking_version = '{SOURCE_TRACKING_VERSION}'
        WHERE singleton = 1 AND tracking_version <> '{SOURCE_TRACKING_VERSION}';
        DROP TRIGGER IF EXISTS {SOURCE_TRIGGER_INSERT};
        DROP TRIGGER IF EXISTS {SOURCE_TRIGGER_UPDATE};
        DROP TRIGGER IF EXISTS {SOURCE_TRIGGER_DELETE};
        CREATE TRIGGER {SOURCE_TRIGGER_INSERT} AFTER INSERT ON records BEGIN
            UPDATE {SOURCE_STATE_TABLE}
            SET generation = lower(hex(randomblob(16))), dirty = 1
            WHERE singleton = 1 AND dirty = 0;
        END;
        CREATE TRIGGER {SOURCE_TRIGGER_UPDATE} AFTER UPDATE ON records BEGIN
            UPDATE {SOURCE_STATE_TABLE}
            SET generation = lower(hex(randomblob(16))), dirty = 1
            WHERE singleton = 1 AND dirty = 0;
        END;
        CREATE TRIGGER {SOURCE_TRIGGER_DELETE} AFTER DELETE ON records BEGIN
            UPDATE {SOURCE_STATE_TABLE}
            SET generation = lower(hex(randomblob(16))), dirty = 1
            WHERE singleton = 1 AND dirty = 0;
        END;"
    ))
    .map_err(|err| format!("Could not install SQLite projection tracking: {err}"))
}

fn prepare_source_generation(conn: &SqliteConnection) -> Result<(), String> {
    let changed = conn
        .execute(
            &format!(
                "UPDATE {SOURCE_STATE_TABLE}
                 SET generation = CASE
                        WHEN dirty = 1 THEN lower(hex(randomblob(16)))
                        ELSE generation
                     END,
                     dirty = 0
                 WHERE singleton = 1 AND tracking_version = ?"
            ),
            [SOURCE_TRACKING_VERSION],
        )
        .map_err(|err| format!("Could not prepare the SQLite projection generation: {err}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("SQLite projection tracking state is missing or incompatible.".to_string())
    }
}

fn source_contract(conn: &SqliteConnection, sqlite_path: &Path) -> Result<SourceContract, String> {
    let (generation, dirty, tracking_version): (String, i64, String) = conn
        .query_row(
            &format!(
                "SELECT generation, dirty, tracking_version
                 FROM {SOURCE_STATE_TABLE} WHERE singleton = 1"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| format!("Could not read SQLite projection generation: {err}"))?;
    if tracking_version != SOURCE_TRACKING_VERSION {
        return Err("SQLite projection generation is incompatible.".to_string());
    }

    let records_schema = schema_sql(conn, "table", "records")?;
    let state_schema = schema_sql(conn, "table", SOURCE_STATE_TABLE)?;
    let source_schemas_schema = schema_sql(conn, "table", "source_schemas")?;
    let source_columns_schema = schema_sql(conn, "table", "source_columns")?;
    let import_sources_schema = schema_sql(conn, "table", "import_sources")?;
    let mut trigger_stmt = conn
        .prepare(
            "SELECT name, COALESCE(sql, '')
             FROM sqlite_schema
             WHERE type = 'trigger' AND tbl_name = 'records'
               AND name LIKE 'base_search_olap_records_%'
             ORDER BY name",
        )
        .map_err(|err| err.to_string())?;
    let trigger_rows = trigger_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| err.to_string())?;
    let triggers = trigger_rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    let trigger_names = triggers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    if trigger_names != SOURCE_TRIGGERS {
        return Err("SQLite projection tracking triggers are missing or incompatible.".to_string());
    }

    let semantic_mapping = conn
        .query_row(
            "SELECT COALESCE((SELECT value FROM meta WHERE key = ? LIMIT 1), '')",
            [table_shape::TABLE_SHAPE_KEY],
            |row| row.get::<_, String>(0),
        )
        .map_err(|err| format!("Could not read SQLite semantic mapping: {err}"))?;
    let source_identity = std::fs::canonicalize(sqlite_path)
        .unwrap_or_else(|_| sqlite_path.to_path_buf())
        .display()
        .to_string();

    let mut hasher = Sha256::new();
    hash_part(&mut hasher, "projection_schema", PROJECTION_SCHEMA_VERSION);
    hash_part(&mut hasher, "tracking_schema", SOURCE_TRACKING_VERSION);
    hash_part(&mut hasher, "source", &source_identity);
    hash_part(&mut hasher, "generation", &generation);
    hash_part(&mut hasher, "dirty", &dirty.to_string());
    hash_part(&mut hasher, "records_schema", &records_schema);
    hash_part(&mut hasher, "state_schema", &state_schema);
    hash_part(&mut hasher, "source_schemas_schema", &source_schemas_schema);
    hash_part(&mut hasher, "source_columns_schema", &source_columns_schema);
    hash_part(&mut hasher, "import_sources_schema", &import_sources_schema);
    hash_part(&mut hasher, "semantic_mapping", &semantic_mapping);
    hash_query_rows(
        &mut hasher,
        conn,
        "source_schemas",
        "SELECT id, public_id, fingerprint, fingerprint_version,
                fixed_currency, fixed_weight_unit, created_at
         FROM source_schemas ORDER BY id",
    )?;
    hash_query_rows(
        &mut hasher,
        conn,
        "source_columns",
        "SELECT id, schema_id, field_id, source_index, raw_header, display_header,
                normalized_header, role, semantic, storage_kind, storage_name
         FROM source_columns ORDER BY schema_id, source_index, id",
    )?;
    hash_query_rows(
        &mut hasher,
        conn,
        "import_sources",
        "SELECT id, public_id, schema_id, source_file, table_name,
                import_fingerprint, imported_at
         FROM import_sources ORDER BY id",
    )?;
    for (name, sql) in triggers {
        hash_part(&mut hasher, &name, &sql);
    }
    let fingerprint = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(SourceContract {
        generation,
        fingerprint,
    })
}

fn schema_sql(conn: &SqliteConnection, kind: &str, name: &str) -> Result<String, String> {
    conn.query_row(
        "SELECT COALESCE(sql, '') FROM sqlite_schema WHERE type = ? AND name = ? LIMIT 1",
        [kind, name],
        |row| row.get(0),
    )
    .map_err(|err| format!("Could not read SQLite schema for {name}: {err}"))
}

fn hash_part(hasher: &mut Sha256, label: &str, value: &str) {
    hasher.update((label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_query_rows(
    hasher: &mut Sha256,
    conn: &SqliteConnection,
    label: &str,
    sql: &str,
) -> Result<(), String> {
    hash_part(hasher, "table_state", label);
    let mut statement = conn
        .prepare(sql)
        .map_err(|err| format!("Could not prepare {label} projection state: {err}"))?;
    let column_count = statement.column_count();
    let mut rows = statement
        .query([])
        .map_err(|err| format!("Could not read {label} projection state: {err}"))?;
    while let Some(row) = rows
        .next()
        .map_err(|err| format!("Could not iterate {label} projection state: {err}"))?
    {
        hasher.update([0xff]);
        for index in 0..column_count {
            hasher.update((index as u64).to_le_bytes());
            match row
                .get_ref(index)
                .map_err(|err| format!("Could not read {label} column {index}: {err}"))?
            {
                ValueRef::Null => hasher.update([0]),
                ValueRef::Integer(value) => {
                    hasher.update([1]);
                    hasher.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hasher.update([2]);
                    hasher.update(value.to_bits().to_le_bytes());
                }
                ValueRef::Text(value) => {
                    hasher.update([3]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value);
                }
                ValueRef::Blob(value) => {
                    hasher.update([4]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value);
                }
            }
        }
    }
    Ok(())
}

pub fn rollup_contract_fingerprint() -> String {
    let mut hasher = Sha256::new();
    hash_part(&mut hasher, "schema", ROLLUP_SCHEMA_VERSION);
    hash_part(&mut hasher, "rules", ROLLUP_RULES_VERSION);
    hash_part(&mut hasher, "contract", ROLLUP_CONTRACT);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy)]
enum ProjectionValue {
    Label(SemanticField),
    Text(SemanticField),
    Country(SemanticField),
    Number(SemanticField),
    Month(SemanticField),
    CurrencyKey,
    WeightUnitKey,
}

impl ProjectionValue {
    fn semantic(self) -> Option<SemanticField> {
        match self {
            Self::Label(field)
            | Self::Text(field)
            | Self::Country(field)
            | Self::Number(field)
            | Self::Month(field) => Some(field),
            Self::CurrencyKey => Some(SemanticField::Currency),
            Self::WeightUnitKey => Some(SemanticField::WeightUnit),
        }
    }
}

fn projection_value_expression(
    columns: &AnalyticsColumns,
    value: ProjectionValue,
) -> Option<String> {
    match value {
        ProjectionValue::Label(field) => columns.label(field),
        ProjectionValue::Text(field) => columns.text(field),
        ProjectionValue::Country(field) => columns.country_key(field),
        ProjectionValue::Number(field) => columns.number(field),
        ProjectionValue::Month(field) => columns.month(field),
        ProjectionValue::CurrencyKey => Some(columns.measures().currency_key),
        ProjectionValue::WeightUnitKey => Some(columns.measures().weight_unit_key),
    }
}

fn source_schema_shape(schema: &SourceSchema) -> TableShape {
    TableShape {
        columns: schema
            .columns
            .iter()
            .map(|field| SourceColumn {
                id: field.field_id.clone(),
                header: field.header.clone(),
                source_index: field.source_index,
                role: field.role,
                semantic: field.semantic,
                storage: field.storage.clone(),
            })
            .collect(),
    }
}

fn schema_aware_projection_value(
    conn: &SqliteConnection,
    value: ProjectionValue,
    missing: &str,
) -> Result<String, String> {
    let legacy_columns =
        AnalyticsColumns::for_alias(table_shape::effective(conn), effective_rows::PAYLOAD_ALIAS);
    let legacy =
        projection_value_expression(&legacy_columns, value).unwrap_or_else(|| missing.to_string());
    let schemas = source_schemas::list(conn)
        .map_err(|err| format!("Could not read source schemas for DuckDB projection: {err}"))?;
    if schemas.is_empty() {
        return Ok(legacy);
    }

    let mut branches = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let mapped = value.semantic().is_some_and(|semantic| {
            schema
                .columns
                .iter()
                .any(|field| field.semantic == Some(semantic))
        });
        let columns = AnalyticsColumns::for_alias(
            Some(source_schema_shape(&schema)),
            effective_rows::PAYLOAD_ALIAS,
        )
        .with_schema_fixed_values(
            schema.fixed_currency.as_deref().map(sql_string),
            schema.fixed_weight_unit.as_deref().map(sql_string),
        );
        let expression = match value {
            ProjectionValue::CurrencyKey | ProjectionValue::WeightUnitKey => {
                projection_value_expression(&columns, value)
            }
            _ if mapped => projection_value_expression(&columns, value),
            _ => None,
        }
        .unwrap_or_else(|| missing.to_string());
        branches.push(format!("WHEN {} THEN {expression}", schema.id));
    }
    Ok(format!(
        "CASE {}.schema_id {} ELSE {legacy} END",
        effective_rows::PAYLOAD_ALIAS,
        branches.join(" ")
    ))
}

fn projection_select_sql(conn: &SqliteConnection) -> Result<String, String> {
    let label = |field| schema_aware_projection_value(conn, ProjectionValue::Label(field), "''");
    let text = |field| schema_aware_projection_value(conn, ProjectionValue::Text(field), "''");
    let country =
        |field| schema_aware_projection_value(conn, ProjectionValue::Country(field), "''");
    let number =
        |field| schema_aware_projection_value(conn, ProjectionValue::Number(field), "NULL");
    let month =
        schema_aware_projection_value(conn, ProjectionValue::Month(SemanticField::Date), "''")?;
    // WHY: `query_plan.rs` resolves a year filter as
    // `year = ? OR (year IS NULL AND <year from the month key> = ?)` — the
    // stored year wins and the date string is only the fallback. This derived
    // the year the other way round, and consulted the stored year for legacy
    // rows only, so any row whose date parses to a different year than the one
    // recorded at import — and every schema-backed row with a date the month
    // key cannot read — appeared under one year on SQLite and another (or none)
    // on DuckDB. COALESCE in this order is exactly the SQLite predicate: a
    // stored year wins, a NULL one falls back to the month key.
    let year = format!(
        "COALESCE({payload}.year, CAST(NULLIF(SUBSTR({month}, 1, 4), '') AS INTEGER))",
        payload = effective_rows::PAYLOAD_ALIAS
    );
    let declaration = label(SemanticField::DeclarationNumber)?;
    let sender_label = label(SemanticField::Sender)?;
    let sender_text = text(SemanticField::Sender)?;
    let recipient_label = label(SemanticField::Recipient)?;
    let recipient_text = text(SemanticField::Recipient)?;
    let edrpou_label = label(SemanticField::CompanyCode)?;
    let edrpou_text = text(SemanticField::CompanyCode)?;
    let product_code = label(SemanticField::ProductCode)?;
    let product_code_text = text(SemanticField::ProductCode)?;
    let description = label(SemanticField::Description)?;
    let description_text = text(SemanticField::Description)?;
    let trademark_label = label(SemanticField::Trademark)?;
    let trademark_text = text(SemanticField::Trademark)?;
    let origin = country(SemanticField::OriginCountry)?;
    let dispatch = country(SemanticField::DispatchCountry)?;
    let trade = country(SemanticField::TradeCountry)?;
    let value = number(SemanticField::Value)?;
    let net = number(SemanticField::NetWeight)?;
    let gross = number(SemanticField::GrossWeight)?;
    let quantity = number(SemanticField::Quantity)?;
    let currency =
        schema_aware_projection_value(conn, ProjectionValue::CurrencyKey, "'__unknown__'")?;
    let weight_unit =
        schema_aware_projection_value(conn, ProjectionValue::WeightUnitKey, "'__unknown__'")?;

    Ok(format!(
        "SELECT
            r.id,
            {year},
            {declaration},
            {sender_label},
            {sender_text},
            {recipient_label},
            {recipient_text},
            {edrpou_label},
            text_key({edrpou_text}),
            {product_code},
            {product_code_text},
            {description},
            {description_text},
            {trademark_label},
            text_key({trademark_text}),
            {origin},
            {dispatch},
            {trade},
            {month},
            {value},
            {net},
            {gross},
            {quantity},
            p.rfv_num,
            p.rmv_net_num,
            p.rmv_extra_num,
            p.rmv_gross_num,
            p.min_base_num,
            {currency},
            {weight_unit},
            r.dup_first_file
         FROM records r{}
         ORDER BY r.id",
        effective_rows::payload_join()
    ))
}

pub fn supports_projection_query(query: &Query) -> bool {
    query.advanced.as_ref().is_none_or(|expr| expr.is_empty())
}

pub fn analytics_scoped(
    projection_path: &Path,
    query: &Query,
    limit: u64,
    scope: Option<AnalyticsScope>,
    hs_level: u8,
) -> Result<Analytics, String> {
    analytics_scoped_with_source(projection_path, query, limit, scope, hs_level)
        .map(|(analytics, _)| analytics)
}

pub fn analytics_scoped_with_source(
    projection_path: &Path,
    query: &Query,
    limit: u64,
    scope: Option<AnalyticsScope>,
    hs_level: u8,
) -> Result<(Analytics, DuckAnalyticsSource), String> {
    if let Some(selector) = RollupSelector::from_query(query) {
        let conn = open_projection_read_only(projection_path)?;
        if rollup_semantics_are_safe(&conn, selector)? {
            let analytics = analytics_from_rollups(&conn, query, selector, limit, scope, hs_level)?;
            return Ok((analytics, DuckAnalyticsSource::Rollup));
        }
    }
    analytics_scoped_detail(projection_path, query, limit, scope, hs_level)
        .map(|analytics| (analytics, DuckAnalyticsSource::Detail))
}

pub fn analytics_scoped_detail(
    projection_path: &Path,
    query: &Query,
    limit: u64,
    scope: Option<AnalyticsScope>,
    hs_level: u8,
) -> Result<Analytics, String> {
    if !supports_projection_query(query) {
        return Err(
            "DuckDB projection does not support advanced query expressions yet.".to_string(),
        );
    }
    let conn = open_projection_read_only(projection_path)?;
    let filter = DuckFilter::from_query(query);
    let overview = projection_overview(&conn, &filter)?;
    // The month rows inherit the query-level currency and weight buckets, so
    // they need the measures themselves, not just "is this USD".
    let months = projection_months(&conn, &filter, &overview.measures)?;
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
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::Edrpou,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::Recipients,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
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
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::ProductCodes,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::Trademarks,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
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
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::OriginCountries,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
                    AnalyticsSectionKind::DispatchCountries,
                    hs_level,
                    limit,
                    overview,
                )?,
                projection_section(
                    &conn,
                    &filter,
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
            analytics.price_sections = projection_price_metrics(&conn, &filter)?;
        }
    }
    Ok(analytics)
}

/// Rows the projection's own filter selects, with no analytics computed.
///
/// One aggregate scan, so projection verification can afford to ask the same
/// question twice — for instance to prove that two spellings of a needle fold
/// to the same match — without paying for a full analytics answer each time.
pub fn projection_row_count(projection_path: &Path, query: &Query) -> Result<u64, String> {
    if !supports_projection_query(query) {
        return Err(
            "DuckDB projection does not support advanced query expressions yet.".to_string(),
        );
    }
    let conn = open_projection_read_only(projection_path)?;
    let filter = DuckFilter::from_query(query);
    query_count(
        &conn,
        &format!("SELECT COUNT(*) FROM records {}", filter.where_sql()),
    )
}

pub fn analytics_section(
    projection_path: &Path,
    query: &Query,
    kind: AnalyticsSectionKind,
    hs_level: u8,
    limit: u64,
) -> Result<AnalyticsSection, String> {
    if !supports_projection_query(query) {
        return Err(
            "DuckDB projection does not support advanced query expressions yet.".to_string(),
        );
    }
    let conn = open_projection_read_only(projection_path)?;
    let filter = DuckFilter::from_query(query);
    let overview = projection_overview(&conn, &filter)?;
    projection_section(&conn, &filter, kind, hs_level, limit, &overview)
}

#[derive(Clone, Copy)]
struct RollupSelector {
    record_scope: &'static str,
    year_key: i64,
}

impl RollupSelector {
    fn from_query(query: &Query) -> Option<Self> {
        if !query.text.trim().is_empty()
            || query.advanced.as_ref().is_some_and(|expr| !expr.is_empty())
        {
            return None;
        }
        let mut non_year_filters = query.filters.clone();
        let year = std::mem::take(&mut non_year_filters.year);
        if !non_year_filters.is_empty() {
            return None;
        }
        let year_key = if year.trim().is_empty() {
            0
        } else {
            let parsed = year.trim().parse::<i64>().ok()?;
            if parsed <= 0 {
                return None;
            }
            parsed
        };
        Some(Self {
            record_scope: match query.record_scope {
                RecordScope::Canonical => "canonical",
                RecordScope::Occurrences => "occurrences",
            },
            year_key,
        })
    }
}

pub fn supports_rollup_query(query: &Query) -> bool {
    RollupSelector::from_query(query).is_some()
}

fn rollup_semantics_are_safe(
    conn: &DuckConnection,
    selector: RollupSelector,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT monetary_mode, weight_mode FROM rollup_overview
         WHERE record_scope = ? AND year_key = ? LIMIT 1",
        duck_params![selector.record_scope, selector.year_key],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .map(|(money, weight)| {
        matches!(money.as_str(), "single_usd" | "empty")
            && matches!(weight.as_str(), "normalized_kg" | "empty")
    })
    .or_else(|err| {
        if matches!(err, duckdb::Error::QueryReturnedNoRows) {
            Ok(true)
        } else {
            Err(err)
        }
    })
    .map_err(|err| format!("Could not inspect DuckDB rollup semantic modes: {err}"))
}

fn analytics_from_rollups(
    conn: &DuckConnection,
    query: &Query,
    selector: RollupSelector,
    limit: u64,
    scope: Option<AnalyticsScope>,
    hs_level: u8,
) -> Result<Analytics, String> {
    let filter = DuckFilter::from_query(query);
    let measures = projection_measures(conn, &filter)?;
    let overview = rollup_overview(conn, selector, measures)?;
    let months = rollup_months(conn, selector, &overview.measures)?;
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
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::Edrpou,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::Recipients,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
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
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::ProductCodes,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::Trademarks,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
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
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::OriginCountries,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
                    AnalyticsSectionKind::DispatchCountries,
                    hs_level,
                    limit,
                    overview,
                )?,
                rollup_section(
                    conn,
                    selector,
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
            let filter = DuckFilter::from_query(query);
            analytics.price_sections = projection_price_metrics(conn, &filter)?;
        }
    }
    Ok(analytics)
}

fn rollup_overview(
    conn: &DuckConnection,
    selector: RollupSelector,
    measures: AnalyticsMeasures,
) -> Result<AnalyticsOverview, String> {
    let mut overview = conn
        .query_row(
            "SELECT
            row_count,
            declaration_count,
            distinct_senders,
            distinct_recipients,
            distinct_edrpou,
            distinct_trademarks,
            distinct_product_codes,
            distinct_origin_countries,
            distinct_dispatch_countries,
            distinct_trade_countries,
            total_quantity
         FROM rollup_overview
         WHERE record_scope = ? AND year_key = ? LIMIT 1",
            duck_params![selector.record_scope, selector.year_key],
            |row| {
                Ok(AnalyticsOverview {
                    row_count: row.get::<_, i64>(0)?.max(0) as u64,
                    declaration_count: row.get::<_, i64>(1)?.max(0) as u64,
                    distinct_senders: row.get::<_, i64>(2)?.max(0) as u64,
                    distinct_recipients: row.get::<_, i64>(3)?.max(0) as u64,
                    distinct_edrpou: row.get::<_, i64>(4)?.max(0) as u64,
                    distinct_trademarks: row.get::<_, i64>(5)?.max(0) as u64,
                    distinct_product_codes: row.get::<_, i64>(6)?.max(0) as u64,
                    distinct_origin_countries: row.get::<_, i64>(7)?.max(0) as u64,
                    distinct_dispatch_countries: row.get::<_, i64>(8)?.max(0) as u64,
                    distinct_trade_countries: row.get::<_, i64>(9)?.max(0) as u64,
                    total_quantity: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                    ..Default::default()
                })
            },
        )
        .or_else(|err| {
            if matches!(err, duckdb::Error::QueryReturnedNoRows) {
                Ok(AnalyticsOverview::default())
            } else {
                Err(err)
            }
        })
        .map_err(|err| format!("Could not read DuckDB overview rollup: {err}"))?;
    overview.total_value_usd = measures.compatible_usd_total().unwrap_or(0.0);
    overview.total_net_kg = measures.total_net_kg();
    overview.total_gross_kg = measures.total_gross_kg();
    overview.avg_value_per_net_kg = measures.compatible_usd_per_net_kg().unwrap_or(0.0);
    overview.compatible_usd = projection_usd_compat(&measures);
    overview.measures = measures;
    Ok(overview)
}

fn rollup_months(
    conn: &DuckConnection,
    selector: RollupSelector,
    query_measures: &AnalyticsMeasures,
) -> Result<Vec<AnalyticsMonthRow>, String> {
    let query_is_usd = query_measures.compatible_usd_total().is_some();
    // WHY the limit is shared: the period caption is derived from the rows that
    // come back, so a hard 48 made a ten-year archive describe itself as a
    // four-year one — and made the two engines return different month series
    // for the same database.
    let mut statement = conn
        .prepare(&format!(
            "SELECT month, rows_count, declarations_count, total_value_usd, total_net_kg,
                    paired_value, paired_net_kg, total_value, valued_rows, net_rows,
                    net_source_total, paired_row_count, paired_source_value, paired_source_net
             FROM rollup_monthly
             WHERE record_scope = ? AND year_key = ?
             ORDER BY month DESC LIMIT {month_limit}",
            month_limit = MONTH_SERIES_LIMIT
        ))
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map(
            duck_params![selector.record_scope, selector.year_key],
            |row| {
                let total_value_usd = row.get::<_, Option<f64>>(3)?.unwrap_or(0.0);
                let total_net_kg = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
                let compatible_usd = inherited_projection_usd(
                    query_is_usd,
                    total_value_usd,
                    row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                );
                Ok(AnalyticsMonthRow {
                    month: row.get(0)?,
                    rows: row.get::<_, i64>(1)?.max(0) as u64,
                    declarations: row.get::<_, i64>(2)?.max(0) as u64,
                    total_value_usd: compatible_usd
                        .as_ref()
                        .map(|compatibility| compatibility.total_value_usd)
                        .unwrap_or(0.0),
                    total_net_kg,
                    compatible_usd,
                    // WHY: `total_value_usd` is `#[serde(skip)]` on this row, so
                    // an empty `AnalyticsMeasures` is the difference between a
                    // monthly figure and an em dash in the browser.
                    measures: inherited_measures(
                        query_measures,
                        SubsetTotals {
                            valued_rows: row.get::<_, i64>(8)?.max(0) as u64,
                            total_value: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                            net_rows: row.get::<_, i64>(9)?.max(0) as u64,
                            total_net_source: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                            gross: None,
                            paired_rows: row.get::<_, i64>(11)?.max(0) as u64,
                            paired_value: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                            paired_net_source: row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
                            // The projection does not group by currency, so its rows keep the
                            // query-level bucket they have always used. Passing anything else
                            // here would make this engine and SQLite report different money.
                            own_currencies: Vec::new(),
                        },
                    ),
                })
            },
        )
        .map_err(|err| err.to_string())?;
    let mut months = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    months.reverse();
    Ok(months)
}

fn rollup_section(
    conn: &DuckConnection,
    selector: RollupSelector,
    kind: AnalyticsSectionKind,
    hs_level: u8,
    limit: u64,
    overview: &AnalyticsOverview,
) -> Result<AnalyticsSection, String> {
    let (kind_key, stored_hs_level, filter_field) = rollup_section_contract(kind, hs_level);
    // Same basis as the detail path, for the same reason: SQLite ranks and
    // shares group rows by the plain `SUM(value)`, never by the USD-compatible
    // total. Reading `total_value_usd` happens to agree here — a rollup is only
    // consulted when the whole cohort is one USD bucket or carries no money at
    // all, and in both of those cases the two columns hold the same number — but
    // that is a property of the gate in `rollup_semantics_are_safe`, not of this
    // query. Stating the rule once means a future widening of that gate cannot
    // quietly give the two engines two different top-N lists.
    let share_total_value: f64 = overview
        .measures
        .currency_totals
        .iter()
        .map(|total| total.total_value)
        .sum();
    let mut statement = conn
        .prepare(
            "SELECT label, rows_count, declarations_count, companies_count,
                    total_value_usd, total_net_kg, total_gross_kg, total_quantity,
                    paired_value, paired_net_kg, total_value, valued_rows, net_rows,
                    gross_rows, net_source_total, gross_source_total,
                    paired_row_count, paired_source_value, paired_source_net
             FROM rollup_sections
             WHERE record_scope = ? AND year_key = ? AND kind = ? AND hs_level = ?
             ORDER BY total_value DESC, total_net_kg DESC,
                      rows_count DESC, label
             LIMIT ?",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map(
            duck_params![
                selector.record_scope,
                selector.year_key,
                kind_key,
                stored_hs_level,
                limit.clamp(1, 20_000) as i64
            ],
            |row| {
                let label: String = row.get(0)?;
                let rows_count = row.get::<_, i64>(1)?.max(0) as u64;
                let total_value_usd = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
                let total_net_kg = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
                let total_gross_kg = row.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
                let total_quantity = row.get::<_, Option<f64>>(7)?.unwrap_or(0.0);
                let total_value = row.get::<_, Option<f64>>(10)?.unwrap_or(0.0);
                let compatible_usd = inherited_projection_usd(
                    overview.compatible_usd.is_some(),
                    total_value_usd,
                    row.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                    row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                );
                let share_base = if share_total_value > 0.0 {
                    share_total_value
                } else if overview.total_net_kg > 0.0 {
                    overview.total_net_kg
                } else {
                    overview.row_count as f64
                };
                let share_value = if share_total_value > 0.0 {
                    total_value
                } else if overview.total_net_kg > 0.0 {
                    total_net_kg
                } else {
                    rows_count as f64
                };
                Ok(AnalyticsGroupRow {
                    filter_action: filter_field.map(|field| AnalyticsFilterAction {
                        field,
                        value: label.clone(),
                    }),
                    label,
                    rows: rows_count,
                    declarations: row.get::<_, i64>(2)?.max(0) as u64,
                    companies: row.get::<_, i64>(3)?.max(0) as u64,
                    total_value_usd: compatible_usd
                        .as_ref()
                        .map(|compatibility| compatibility.total_value_usd)
                        .unwrap_or(0.0),
                    total_net_kg,
                    total_gross_kg,
                    total_quantity,
                    share_percent: ratio(share_value * 100.0, share_base),
                    avg_value_per_net_kg: compatible_usd
                        .as_ref()
                        .and_then(|compatibility| compatibility.avg_value_per_net_kg)
                        .unwrap_or(0.0),
                    compatible_usd,
                    // WHY: same contract as the detail path — a group row
                    // publishes its money and weight only through `measures`,
                    // so a default one renders as an em dash.
                    measures: inherited_measures(
                        &overview.measures,
                        SubsetTotals {
                            valued_rows: row.get::<_, i64>(11)?.max(0) as u64,
                            total_value,
                            net_rows: row.get::<_, i64>(12)?.max(0) as u64,
                            total_net_source: row.get::<_, Option<f64>>(14)?.unwrap_or(0.0),
                            gross: Some((
                                row.get::<_, i64>(13)?.max(0) as u64,
                                row.get::<_, Option<f64>>(15)?.unwrap_or(0.0),
                            )),
                            paired_rows: row.get::<_, i64>(16)?.max(0) as u64,
                            paired_value: row.get::<_, Option<f64>>(17)?.unwrap_or(0.0),
                            paired_net_source: row.get::<_, Option<f64>>(18)?.unwrap_or(0.0),
                            // The projection does not group by currency, so its rows keep the
                            // query-level bucket they have always used. Passing anything else
                            // here would make this engine and SQLite report different money.
                            own_currencies: Vec::new(),
                        },
                    ),
                })
            },
        )
        .map_err(|err| err.to_string())?;
    Ok(AnalyticsSection {
        kind,
        rows: rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?,
    })
}

fn rollup_section_contract(
    kind: AnalyticsSectionKind,
    hs_level: u8,
) -> (&'static str, i64, Option<AnalyticsFilterField>) {
    match kind {
        AnalyticsSectionKind::Recipients => {
            ("recipients", 0, Some(AnalyticsFilterField::Recipient))
        }
        AnalyticsSectionKind::Senders => ("senders", 0, Some(AnalyticsFilterField::Sender)),
        AnalyticsSectionKind::Edrpou => ("edrpou", 0, Some(AnalyticsFilterField::Edrpou)),
        AnalyticsSectionKind::ProductCodes => (
            "product_codes",
            if hs_level >= 10 {
                10
            } else {
                i64::from(hs_level.clamp(2, 8))
            },
            Some(AnalyticsFilterField::ProductCode),
        ),
        AnalyticsSectionKind::Trademarks => {
            ("trademarks", 0, Some(AnalyticsFilterField::Trademark))
        }
        AnalyticsSectionKind::ProductGroups => {
            ("product_groups", 0, Some(AnalyticsFilterField::Description))
        }
        AnalyticsSectionKind::OriginCountries => (
            "origin_countries",
            0,
            Some(AnalyticsFilterField::OriginCountry),
        ),
        AnalyticsSectionKind::DispatchCountries => (
            "dispatch_countries",
            0,
            Some(AnalyticsFilterField::DispatchCountry),
        ),
        AnalyticsSectionKind::TradeCountries => (
            "trade_countries",
            0,
            Some(AnalyticsFilterField::TradeCountry),
        ),
    }
}

pub fn run_duckdb_benchmark(
    projection_path: &Path,
    query: &Query,
    options: &OlapBenchmarkOptions,
) -> Result<OlapBenchmarkReport, String> {
    let conn = open_projection_read_only(projection_path)?;
    let total_database_rows = query_count(&conn, "SELECT COUNT(*) FROM records")?;
    let filter = DuckFilter::from_query(query);
    let mut scenarios = Vec::new();
    scenarios.push(measure_duck_scenario(
        options,
        "Search count",
        "search",
        "Counts rows matching the projection filter. Text search is LIKE-based, not FTS.",
        || {
            query_count(
                &conn,
                &format!("SELECT COUNT(*) FROM records {}", filter.where_sql()),
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "First result page",
        "search",
        "Reads the first projected rows for a matching filter.",
        || {
            query_count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM (SELECT id FROM records {} ORDER BY id LIMIT {})",
                    filter.where_sql(),
                    options.page_limit.clamp(1, 500)
                ),
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Analytics overview",
        "olap",
        "Computes headline totals and distinct counts from the DuckDB projection.",
        || {
            query_count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM (
                    SELECT
                        COUNT(*),
                        COUNT(DISTINCT declaration_number),
                        COUNT(DISTINCT recipient_label),
                        COUNT(DISTINCT sender_label),
                        COUNT(DISTINCT edrpou_label),
                        SUM(value_num),
                        SUM(net_kg_num),
                        SUM(gross_kg_num),
                        SUM(quantity_num)
                    FROM records {}
                )",
                    filter.where_sql()
                ),
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Companies aggregation",
        "olap",
        "Groups by recipient, sender, and company id using columnar scans.",
        || {
            Ok(
                count_group_rows(&conn, &filter, "recipient_label", options.section_limit)?
                    + count_group_rows(&conn, &filter, "sender_label", options.section_limit)?
                    + count_group_rows(&conn, &filter, "edrpou_label", options.section_limit)?,
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Products aggregation",
        "olap",
        "Groups by product code and trademark using columnar scans.",
        || {
            Ok(
                count_group_rows(&conn, &filter, "product_code", options.section_limit)?
                    + count_group_rows(&conn, &filter, "trademark_label", options.section_limit)?,
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Countries aggregation",
        "olap",
        "Groups by origin, dispatch, and trade countries.",
        || {
            Ok(
                count_group_rows(&conn, &filter, "origin_key", options.section_limit)?
                    + count_group_rows(&conn, &filter, "dispatch_key", options.section_limit)?
                    + count_group_rows(&conn, &filter, "trade_key", options.section_limit)?,
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Price metrics",
        "olap",
        "Calculates available price-per-weight metrics from projected numeric columns.",
        || {
            query_count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM (
                    SELECT AVG(value_num / NULLIF(net_kg_num, 0))
                    FROM records
                    {} value_num IS NOT NULL AND net_kg_num IS NOT NULL AND net_kg_num > 0
                )",
                    filter.where_extra_sql()
                ),
            )
        },
    )?);
    scenarios.push(measure_duck_scenario(
        options,
        "Pivot: recipient by month",
        "olap",
        "Builds a compact recipient/month value matrix from grouped rows.",
        || {
            query_count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM (
                    SELECT recipient_label, month, SUM(value_num)
                    FROM records
                    {} recipient_label IS NOT NULL AND recipient_label <> ''
                      AND month IS NOT NULL AND month <> ''
                    GROUP BY recipient_label, month
                    LIMIT {}
                )",
                    filter.where_extra_sql(),
                    options
                        .pivot_rows
                        .saturating_mul(options.pivot_cols)
                        .clamp(1, 10_000)
                ),
            )
        },
    )?);

    Ok(OlapBenchmarkReport {
        backend: "duckdb",
        total_database_rows,
        unindexed_rows: 0,
        query: query.clone(),
        query_is_empty: query.is_empty(),
        scenarios,
    })
}

fn prepare_projection_schema(conn: &DuckConnection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE records (
            id BIGINT,
            year BIGINT,
            declaration_number VARCHAR,
            sender_label VARCHAR,
            sender_text VARCHAR,
            recipient_label VARCHAR,
            recipient_text VARCHAR,
            edrpou_label VARCHAR,
            edrpou_key VARCHAR,
            product_code VARCHAR,
            product_code_text VARCHAR,
            description VARCHAR,
            description_text VARCHAR,
            trademark_label VARCHAR,
            trademark_key VARCHAR,
            origin_key VARCHAR,
            dispatch_key VARCHAR,
            trade_key VARCHAR,
            month VARCHAR,
            value_num DOUBLE,
            net_kg_num DOUBLE,
            gross_kg_num DOUBLE,
            quantity_num DOUBLE,
            rfv_num DOUBLE,
            rmv_net_num DOUBLE,
            rmv_extra_num DOUBLE,
            rmv_gross_num DOUBLE,
            min_base_num DOUBLE,
            currency_key VARCHAR,
            weight_unit_key VARCHAR,
            dup_first_file VARCHAR
        );
        CREATE TABLE projection_meta(key VARCHAR PRIMARY KEY, value VARCHAR);",
    )
    .map_err(|err| err.to_string())
}

/// Builds the persisted rollups.
///
/// Besides the published totals, `rollup_monthly` and `rollup_sections` carry a
/// second family of columns — `total_value`, `valued_rows`, `net_rows`,
/// `gross_rows`, `*_source_total`, `paired_row_count`, `paired_source_*`. They
/// exist because a row's `measures` inherit the query's currency and weight
/// buckets and then relabel the subset's OWN sums onto them, which needs the
/// plain per-subset sums in the source unit plus the row counts behind them.
/// The published `total_value_usd` cannot stand in: it is deliberately NULL
/// unless the whole cohort is a single known USD bucket. Neither can the `*_kg`
/// sums: they are already converted, while the inherited bucket carries the
/// conversion factor and applies it itself.
///
/// `year_key = 0` is the reserved "all years" bucket, which is why the per-year
/// branch of `rollup_expanded` excludes a literal year 0. A row can carry one:
/// the projected year falls back to the month key, and a date such as
/// "01.05.0000" yields the month "0000-05" and therefore the year 0. Without the
/// exclusion that row lands in the all-years bucket twice, `validate_rollups`
/// sees more rollup rows than detail rows, and the whole projection build fails.
/// Nothing is lost: `RollupSelector` already refuses a year filter of 0, so such
/// a query is answered by the detail scan, which reads the year column directly.
fn build_rollups(conn: &DuckConnection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE VIEW rollup_records AS
            SELECT
                'occurrences'::VARCHAR AS record_scope,
                records.*,
                CASE weight_unit_key
                    WHEN 'kg' THEN net_kg_num
                    WHEN 'g' THEN net_kg_num * 0.001
                    WHEN 'tonne' THEN net_kg_num * 1000.0
                    WHEN 'lb' THEN net_kg_num * 0.45359237
                END AS net_weight_kg,
                CASE weight_unit_key
                    WHEN 'kg' THEN gross_kg_num
                    WHEN 'g' THEN gross_kg_num * 0.001
                    WHEN 'tonne' THEN gross_kg_num * 1000.0
                    WHEN 'lb' THEN gross_kg_num * 0.45359237
                END AS gross_weight_kg
            FROM records
            UNION ALL
            SELECT
                'canonical'::VARCHAR AS record_scope,
                records.*,
                CASE weight_unit_key
                    WHEN 'kg' THEN net_kg_num
                    WHEN 'g' THEN net_kg_num * 0.001
                    WHEN 'tonne' THEN net_kg_num * 1000.0
                    WHEN 'lb' THEN net_kg_num * 0.45359237
                END AS net_weight_kg,
                CASE weight_unit_key
                    WHEN 'kg' THEN gross_kg_num
                    WHEN 'g' THEN gross_kg_num * 0.001
                    WHEN 'tonne' THEN gross_kg_num * 1000.0
                    WHEN 'lb' THEN gross_kg_num * 0.45359237
                END AS gross_weight_kg
            FROM records WHERE dup_first_file IS NULL;

         CREATE VIEW rollup_expanded AS
            SELECT rollup_records.*, 0::BIGINT AS year_key FROM rollup_records
            UNION ALL
            SELECT rollup_records.*, year AS year_key
            FROM rollup_records WHERE year IS NOT NULL AND year <> 0;

         CREATE TABLE rollup_overview AS
            SELECT
                record_scope,
                year_key,
                COUNT(*)::BIGINT AS row_count,
                COUNT(DISTINCT NULLIF(declaration_number, ''))::BIGINT AS declaration_count,
                COUNT(DISTINCT NULLIF(sender_label, ''))::BIGINT AS distinct_senders,
                COUNT(DISTINCT NULLIF(recipient_label, ''))::BIGINT AS distinct_recipients,
                COUNT(DISTINCT NULLIF(edrpou_label, ''))::BIGINT AS distinct_edrpou,
                COUNT(DISTINCT NULLIF(trademark_label, ''))::BIGINT AS distinct_trademarks,
                COUNT(DISTINCT NULLIF(product_code, ''))::BIGINT AS distinct_product_codes,
                COUNT(DISTINCT NULLIF(origin_key, ''))::BIGINT AS distinct_origin_countries,
                COUNT(DISTINCT NULLIF(dispatch_key, ''))::BIGINT AS distinct_dispatch_countries,
                COUNT(DISTINCT NULLIF(trade_key, ''))::BIGINT AS distinct_trade_countries,
                CASE
                    WHEN COUNT(value_num) = 0 THEN 0.0
                    WHEN COUNT(*) FILTER (
                        WHERE value_num IS NOT NULL AND (
                            COALESCE(currency_key, '') = ''
                            OR starts_with(currency_key, '__unknown__')
                        )
                    ) > 0 THEN NULL
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 'USD' THEN SUM(value_num)
                    ELSE NULL
                END AS total_value_usd,
                COALESCE(SUM(gross_weight_kg), 0.0) AS total_gross_kg,
                COALESCE(SUM(net_weight_kg), 0.0) AS total_net_kg,
                COALESCE(SUM(quantity_num), 0.0) AS total_quantity,
                CASE
                    WHEN COUNT(value_num) = 0 THEN 'empty'
                    WHEN COUNT(*) FILTER (
                        WHERE value_num IS NOT NULL AND (
                            COALESCE(currency_key, '') = ''
                            OR starts_with(currency_key, '__unknown__')
                        )
                    ) > 0 THEN 'unavailable'
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 'USD' THEN 'single_usd'
                    ELSE 'partitioned'
                END AS monetary_mode,
                CASE
                    WHEN COUNT(net_kg_num) = 0 THEN 'empty'
                    WHEN COUNT(*) FILTER (WHERE net_kg_num IS NOT NULL
                                           AND net_weight_kg IS NULL) > 0
                        THEN 'unavailable'
                    ELSE 'normalized_kg'
                END AS weight_mode
            FROM rollup_expanded
            GROUP BY record_scope, year_key;

         INSERT INTO rollup_overview
            SELECT
                empty_scope.record_scope,
                0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0.0, 0.0, 0.0, 0.0,
                'empty', 'empty'
            FROM (VALUES ('canonical'), ('occurrences')) AS empty_scope(record_scope)
            WHERE NOT EXISTS (
                SELECT 1 FROM rollup_overview existing
                WHERE existing.record_scope = empty_scope.record_scope
                  AND existing.year_key = 0
            );

         CREATE TABLE rollup_monthly AS
            SELECT
                record_scope,
                year_key,
                month,
                COUNT(*)::BIGINT AS rows_count,
                COUNT(DISTINCT NULLIF(declaration_number, ''))::BIGINT AS declarations_count,
                CASE
                    WHEN COUNT(value_num) = 0 THEN 0.0
                    WHEN COUNT(*) FILTER (
                        WHERE value_num IS NOT NULL AND (
                            COALESCE(currency_key, '') = ''
                            OR starts_with(currency_key, '__unknown__')
                        )
                    ) > 0 THEN NULL
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL
                          AND NOT starts_with(currency_key, '__unknown__')
                    ) = 'USD' THEN SUM(value_num)
                    ELSE NULL
                END AS total_value_usd,
                COALESCE(SUM(net_weight_kg), 0.0) AS total_net_kg,
                COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_weight_kg > 0
                    THEN value_num ELSE 0.0 END), 0.0) AS paired_value,
                COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_weight_kg > 0
                    THEN net_weight_kg ELSE 0.0 END), 0.0) AS paired_net_kg,
                COALESCE(SUM(value_num), 0.0) AS total_value,
                COUNT(value_num) AS valued_rows,
                COUNT(net_kg_num) AS net_rows,
                COALESCE(SUM(net_kg_num), 0.0) AS net_source_total,
                COUNT(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                    THEN 1 END) AS paired_row_count,
                COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                    THEN value_num END), 0.0) AS paired_source_value,
                COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                    THEN net_kg_num END), 0.0) AS paired_source_net
            FROM rollup_expanded
            WHERE month IS NOT NULL AND month <> ''
            GROUP BY record_scope, year_key, month;

         CREATE TABLE rollup_currency_totals AS
            SELECT
                record_scope,
                year_key,
                COALESCE(currency_key, '') AS currency_key,
                COUNT(value_num)::BIGINT AS valued_rows,
                COALESCE(SUM(value_num), 0.0) AS total_value
            FROM rollup_expanded
            WHERE value_num IS NOT NULL
            GROUP BY record_scope, year_key, COALESCE(currency_key, '');

         CREATE TABLE rollup_sections (
            record_scope VARCHAR,
            year_key BIGINT,
            kind VARCHAR,
            hs_level INTEGER,
            label VARCHAR,
            rows_count BIGINT,
            declarations_count BIGINT,
            companies_count BIGINT,
            total_value_usd DOUBLE,
            total_net_kg DOUBLE,
            total_gross_kg DOUBLE,
            total_quantity DOUBLE,
            paired_value DOUBLE,
            paired_net_kg DOUBLE,
            total_value DOUBLE,
            valued_rows BIGINT,
            net_rows BIGINT,
            gross_rows BIGINT,
            net_source_total DOUBLE,
            gross_source_total DOUBLE,
            paired_row_count BIGINT,
            paired_source_value DOUBLE,
            paired_source_net DOUBLE
         );",
    )
    .map_err(|err| format!("Could not create DuckDB rollup foundation: {err}"))?;

    for (kind, expression) in [
        ("recipients", "recipient_label"),
        ("senders", "sender_label"),
        ("edrpou", "edrpou_label"),
        ("trademarks", "trademark_label"),
        ("product_groups", "SUBSTR(description, 1, 80)"),
        ("origin_countries", "origin_key"),
        ("dispatch_countries", "dispatch_key"),
        ("trade_countries", "trade_key"),
    ] {
        insert_section_rollup(conn, kind, 0, expression)?;
    }
    for hs_level in [2u8, 3, 4, 5, 6, 7, 8, 10] {
        let expression = if hs_level == 10 {
            "product_code".to_string()
        } else {
            format!("SUBSTR(product_code, 1, {hs_level})")
        };
        insert_section_rollup(conn, "product_codes", hs_level, &expression)?;
    }

    conn.execute_batch(
        "CREATE TABLE rollup_price_per_kg_baselines AS
            WITH priced AS (
                SELECT
                    record_scope,
                    year_key,
                    COALESCE(currency_key, '') AS currency_key,
                    COALESCE(weight_unit_key, '') AS weight_unit_key,
                    product_code,
                    trademark_label,
                    origin_key,
                    value_num,
                    net_weight_kg,
                    value_num / net_weight_kg AS price_per_kg
                FROM rollup_expanded
                WHERE value_num IS NOT NULL
                  AND net_weight_kg IS NOT NULL
                  AND net_weight_kg > 0
                  AND COALESCE(currency_key, '') <> ''
                  AND NOT starts_with(currency_key, '__unknown__')
            ), cohorts AS (
                SELECT *, 'all'::VARCHAR AS cohort_kind, ''::VARCHAR AS cohort_label
                FROM priced
                UNION ALL
                SELECT *, 'product_code', product_code FROM priced
                WHERE product_code IS NOT NULL AND product_code <> ''
                UNION ALL
                SELECT *, 'trademark', trademark_label FROM priced
                WHERE trademark_label IS NOT NULL AND trademark_label <> ''
                UNION ALL
                SELECT *, 'origin_country', origin_key FROM priced
                WHERE origin_key IS NOT NULL AND origin_key <> ''
            )
            SELECT
                record_scope,
                year_key,
                cohort_kind,
                cohort_label,
                currency_key,
                weight_unit_key,
                COUNT(*)::BIGINT AS sample_count,
                AVG(price_per_kg) AS average,
                MIN(price_per_kg) AS minimum,
                MAX(price_per_kg) AS maximum,
                SUM(value_num) / NULLIF(SUM(net_weight_kg), 0) AS weighted_average,
                quantile_cont(price_per_kg, 0.25) AS p25,
                quantile_cont(price_per_kg, 0.5) AS median,
                quantile_cont(price_per_kg, 0.75) AS p75
            FROM cohorts
            GROUP BY record_scope, year_key, cohort_kind, cohort_label,
                     currency_key, weight_unit_key;

         ANALYZE rollup_overview;
         ANALYZE rollup_monthly;
         ANALYZE rollup_sections;
         ANALYZE rollup_currency_totals;
         ANALYZE rollup_price_per_kg_baselines;",
    )
    .map_err(|err| format!("Could not create DuckDB price rollups: {err}"))?;

    validate_rollups(conn)
}

fn insert_section_rollup(
    conn: &DuckConnection,
    kind: &str,
    hs_level: u8,
    label_expression: &str,
) -> Result<(), String> {
    let sql = format!(
        "INSERT INTO rollup_sections
         WITH labeled AS (
            SELECT
                record_scope,
                year_key,
                {label_expression} AS label,
                declaration_number,
                edrpou_label,
                value_num,
                net_weight_kg,
                gross_weight_kg,
                net_kg_num,
                gross_kg_num,
                quantity_num,
                currency_key
            FROM rollup_expanded
         )
         SELECT
            record_scope,
            year_key,
            {},
            {hs_level},
            label,
            COUNT(*)::BIGINT,
            COUNT(DISTINCT NULLIF(declaration_number, ''))::BIGINT,
            COUNT(DISTINCT NULLIF(edrpou_label, ''))::BIGINT,
            CASE
                WHEN COUNT(value_num) = 0 THEN 0.0
                WHEN COUNT(*) FILTER (
                    WHERE value_num IS NOT NULL AND (
                        COALESCE(currency_key, '') = ''
                        OR starts_with(currency_key, '__unknown__')
                    )
                ) > 0 THEN NULL
                WHEN COUNT(DISTINCT currency_key) FILTER (
                    WHERE value_num IS NOT NULL
                      AND NOT starts_with(currency_key, '__unknown__')
                ) = 1 AND MIN(currency_key) FILTER (
                    WHERE value_num IS NOT NULL
                      AND NOT starts_with(currency_key, '__unknown__')
                ) = 'USD' THEN SUM(value_num)
                ELSE NULL
            END,
            COALESCE(SUM(net_weight_kg), 0.0),
            COALESCE(SUM(gross_weight_kg), 0.0),
            COALESCE(SUM(quantity_num), 0.0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_weight_kg > 0
                THEN value_num ELSE 0.0 END), 0.0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_weight_kg > 0
                THEN net_weight_kg ELSE 0.0 END), 0.0),
            COALESCE(SUM(value_num), 0.0),
            COUNT(value_num),
            COUNT(net_kg_num),
            COUNT(gross_kg_num),
            COALESCE(SUM(net_kg_num), 0.0),
            COALESCE(SUM(gross_kg_num), 0.0),
            COUNT(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0 THEN 1 END),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN value_num END), 0.0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN net_kg_num END), 0.0)
         FROM labeled
         WHERE label IS NOT NULL AND label <> ''
         GROUP BY record_scope, year_key, label",
        sql_string(kind)
    );
    conn.execute_batch(&sql)
        .map_err(|err| format!("Could not create DuckDB {kind} rollup: {err}"))
}

fn validate_rollups(conn: &DuckConnection) -> Result<(), String> {
    let detail_rows = query_count(conn, "SELECT COUNT(*) FROM records")?;
    let occurrence_rows = query_count(
        conn,
        "SELECT row_count FROM rollup_overview
         WHERE record_scope = 'occurrences' AND year_key = 0",
    )?;
    let canonical_detail_rows = query_count(
        conn,
        "SELECT COUNT(*) FROM records WHERE dup_first_file IS NULL",
    )?;
    let canonical_rollup_rows = query_count(
        conn,
        "SELECT row_count FROM rollup_overview
         WHERE record_scope = 'canonical' AND year_key = 0",
    )?;
    if detail_rows != occurrence_rows || canonical_detail_rows != canonical_rollup_rows {
        return Err(format!(
            "DuckDB rollup validation failed: detail={detail_rows}/{canonical_detail_rows}, \
             rollup={occurrence_rows}/{canonical_rollup_rows}."
        ));
    }
    Ok(())
}

fn write_projection_meta(
    conn: &DuckConnection,
    sqlite_path: &Path,
    rows: u64,
    max_record_id: u64,
    source_contract: &SourceContract,
    built_at: &str,
) -> Result<(), String> {
    let source = sqlite_path.display().to_string();
    let rows = rows.to_string();
    let max_record_id = max_record_id.to_string();
    conn.execute(
        "INSERT INTO projection_meta VALUES
            ('source_sqlite', ?),
            ('rows', ?),
            ('max_record_id', ?),
            ('schema_version', ?),
            ('source_generation', ?),
            ('source_fingerprint', ?),
            ('rollup_schema_version', ?),
            ('rollup_rules_version', ?),
            ('rollup_fingerprint', ?),
            ('built_at', ?)",
        duck_params![
            source,
            rows,
            max_record_id,
            PROJECTION_SCHEMA_VERSION,
            source_contract.generation,
            source_contract.fingerprint,
            ROLLUP_SCHEMA_VERSION,
            ROLLUP_RULES_VERSION,
            rollup_contract_fingerprint(),
            built_at
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn read_projection_meta_value(conn: &DuckConnection, key: &str) -> Result<String, String> {
    conn.query_row(
        "SELECT value FROM projection_meta WHERE key = ? LIMIT 1",
        duck_params![key],
        |row| row.get::<_, String>(0),
    )
    .map_err(|err| format!("Could not read projection metadata {key}: {err}"))
}

fn require_rollup_schema(conn: &DuckConnection) -> Result<(), String> {
    const TABLES: [&str; 5] = [
        "rollup_overview",
        "rollup_monthly",
        "rollup_sections",
        "rollup_currency_totals",
        "rollup_price_per_kg_baselines",
    ];
    for table in TABLES {
        let exists = conn
            .query_row(
                "SELECT COUNT(*) FROM information_schema.tables
                 WHERE table_schema = 'main' AND table_name = ?",
                duck_params![table],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|err| format!("Could not inspect DuckDB rollup schema: {err}"))?;
        if exists != 1 {
            return Err(format!("DuckDB rollup table {table} is missing."));
        }
    }
    Ok(())
}

fn currency_is_known(currency: &str) -> bool {
    !currency.is_empty() && !currency.starts_with(UNKNOWN_CURRENCY_KEY)
}

fn weight_factor(unit: &str) -> Option<f64> {
    match unit {
        "kg" => Some(1.0),
        "g" => Some(0.001),
        "tonne" => Some(1_000.0),
        "lb" => Some(0.453_592_37),
        _ => None,
    }
}

fn normalized_weight_sql(weight: &str) -> String {
    format!(
        "CASE weight_unit_key
            WHEN 'kg' THEN {weight}
            WHEN 'g' THEN {weight} * 0.001
            WHEN 'tonne' THEN {weight} * 1000.0
            WHEN 'lb' THEN {weight} * 0.45359237
         END"
    )
}

fn projection_weight_totals(
    conn: &DuckConnection,
    filter: &DuckFilter,
    weight: &str,
) -> Result<Vec<AnalyticsWeightTotal>, String> {
    let normalized = normalized_weight_sql(weight);
    let sql = format!(
        "SELECT
            COALESCE(NULLIF(weight_unit_key, ''), '{UNKNOWN_UNIT_KEY}') AS source_unit,
            COUNT(*) AS weighted_rows,
            COALESCE(SUM({weight}), 0.0) AS source_total,
            SUM({normalized}) AS kg_total
         FROM records
         {} {weight} IS NOT NULL
         GROUP BY source_unit
         ORDER BY source_total DESC, source_unit",
        filter.where_extra_sql()
    );
    let mut statement = conn.prepare(&sql).map_err(|err| err.to_string())?;
    statement
        .query_map([], |row| {
            let source_unit: String = row.get(0)?;
            let factor = weight_factor(&source_unit);
            Ok(AnalyticsWeightTotal {
                source_unit,
                known: factor.is_some(),
                normalized_unit: factor.map(|_| "kg".to_string()),
                factor_to_kg: factor,
                weighted_rows: row.get::<_, i64>(1)?.max(0) as u64,
                total_source_weight: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                total_kg: row.get(3)?,
            })
        })
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())
}

fn projection_measures(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<AnalyticsMeasures, String> {
    let currency_totals = {
        let sql = format!(
            "SELECT
                COALESCE(NULLIF(currency_key, ''), '{UNKNOWN_CURRENCY_KEY}') AS currency,
                COUNT(*) AS valued_rows,
                COALESCE(SUM(value_num), 0.0) AS total_value
             FROM records
             {} value_num IS NOT NULL
             GROUP BY currency
             ORDER BY total_value DESC, currency",
            filter.where_extra_sql()
        );
        let mut statement = conn.prepare(&sql).map_err(|err| err.to_string())?;
        statement
            .query_map([], |row| {
                let currency: String = row.get(0)?;
                Ok(AnalyticsCurrencyTotal {
                    known: currency_is_known(&currency),
                    currency,
                    valued_rows: row.get::<_, i64>(1)?.max(0) as u64,
                    total_value: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                })
            })
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };

    let net_weight_totals = projection_weight_totals(conn, filter, "net_kg_num")?;
    let gross_weight_totals = projection_weight_totals(conn, filter, "gross_kg_num")?;
    let net_kg = normalized_weight_sql("net_kg_num");
    let value_per_net_weight = {
        let sql = format!(
            "SELECT
                COALESCE(NULLIF(currency_key, ''), '{UNKNOWN_CURRENCY_KEY}') AS currency,
                COALESCE(NULLIF(weight_unit_key, ''), '{UNKNOWN_UNIT_KEY}') AS source_unit,
                COUNT(*) AS paired_rows,
                COALESCE(SUM(value_num), 0.0) AS total_value,
                COALESCE(SUM({net_kg}), 0.0) AS total_weight
             FROM records
             {} value_num IS NOT NULL
               AND net_kg_num IS NOT NULL
               AND ({net_kg}) > 0
             GROUP BY currency, source_unit
             ORDER BY currency, source_unit",
            filter.where_extra_sql()
        );
        let mut statement = conn.prepare(&sql).map_err(|err| err.to_string())?;
        let mut rows = statement.query([]).map_err(|err| err.to_string())?;
        let mut totals = BTreeMap::<String, AnalyticsValuePerWeight>::new();
        while let Some(row) = rows.next().map_err(|err| err.to_string())? {
            let currency: String = row.get(0).map_err(|err| err.to_string())?;
            let source_unit: String = row.get(1).map_err(|err| err.to_string())?;
            let entry = totals
                .entry(currency.clone())
                .or_insert_with(|| AnalyticsValuePerWeight {
                    currency,
                    normalized_weight_unit: "kg".to_string(),
                    ..Default::default()
                });
            entry.source_weight_units.push(source_unit);
            entry.paired_rows += row.get::<_, i64>(2).map_err(|err| err.to_string())?.max(0) as u64;
            entry.total_value += row
                .get::<_, Option<f64>>(3)
                .map_err(|err| err.to_string())?
                .unwrap_or(0.0);
            entry.total_weight += row
                .get::<_, Option<f64>>(4)
                .map_err(|err| err.to_string())?
                .unwrap_or(0.0);
        }
        totals
            .into_values()
            .map(|mut total| {
                total.source_weight_units.sort();
                total.source_weight_units.dedup();
                total.value_per_weight =
                    (total.total_weight > 0.0).then(|| total.total_value / total.total_weight);
                total
            })
            .collect::<Vec<_>>()
    };

    let unknown_currency = format!(
        "(COALESCE(currency_key, '') = '' OR starts_with(currency_key, '{UNKNOWN_CURRENCY_KEY}'))"
    );
    let unknown_unit = format!(
        "(COALESCE(weight_unit_key, '') = '' OR starts_with(weight_unit_key, '{UNKNOWN_UNIT_KEY}'))"
    );
    let exclusion_sql = format!(
        "SELECT
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND {unknown_currency}
                THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN net_kg_num IS NOT NULL AND {unknown_unit}
                THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN gross_kg_num IS NOT NULL AND {unknown_unit}
                THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num IS NOT NULL
                AND {unknown_currency} THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num IS NOT NULL
                AND {unknown_unit} THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL
                AND (net_kg_num IS NULL OR net_kg_num <= 0) THEN 1 ELSE 0 END), 0)
         FROM records {}",
        filter.where_sql()
    );
    let exclusions = conn
        .query_row(&exclusion_sql, [], |row| {
            Ok(AnalyticsMeasureExclusions {
                value_without_known_currency: row.get::<_, i64>(0)?.max(0) as u64,
                net_weight_without_known_unit: row.get::<_, i64>(1)?.max(0) as u64,
                gross_weight_without_known_unit: row.get::<_, i64>(2)?.max(0) as u64,
                ratio_without_known_currency: row.get::<_, i64>(3)?.max(0) as u64,
                ratio_without_known_weight_unit: row.get::<_, i64>(4)?.max(0) as u64,
                ratio_with_zero_or_missing_weight: row.get::<_, i64>(5)?.max(0) as u64,
            })
        })
        .map_err(|err| err.to_string())?;

    let compatible_value_total = (currency_totals.len() == 1 && currency_totals[0].known)
        .then(|| currency_totals[0].clone());
    let compatible_value_per_net_weight = compatible_value_total.as_ref().and_then(|total| {
        value_per_net_weight
            .iter()
            .find(|pair| pair.currency == total.currency)
            .cloned()
    });
    Ok(AnalyticsMeasures {
        currency_totals,
        net_weight_totals,
        gross_weight_totals,
        value_per_net_weight,
        compatible_value_total,
        compatible_value_per_net_weight,
        exclusions,
    })
}

fn projection_overview(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<AnalyticsOverview, String> {
    let measures = projection_measures(conn, filter)?;
    let sql = format!(
        "SELECT
            COUNT(*),
            COUNT(DISTINCT NULLIF(declaration_number, '')),
            COUNT(DISTINCT NULLIF(sender_label, '')),
            COUNT(DISTINCT NULLIF(recipient_label, '')),
            COUNT(DISTINCT NULLIF(edrpou_label, '')),
            COUNT(DISTINCT NULLIF(trademark_label, '')),
            COUNT(DISTINCT NULLIF(product_code, '')),
            COUNT(DISTINCT NULLIF(origin_key, '')),
            COUNT(DISTINCT NULLIF(dispatch_key, '')),
            COUNT(DISTINCT NULLIF(trade_key, '')),
            COALESCE(SUM(quantity_num), 0.0)
         FROM records {}",
        filter.where_sql()
    );
    let mut overview = conn
        .query_row(&sql, [], |row| {
            Ok(AnalyticsOverview {
                row_count: row.get::<_, i64>(0)?.max(0) as u64,
                declaration_count: row.get::<_, i64>(1)?.max(0) as u64,
                distinct_senders: row.get::<_, i64>(2)?.max(0) as u64,
                distinct_recipients: row.get::<_, i64>(3)?.max(0) as u64,
                distinct_edrpou: row.get::<_, i64>(4)?.max(0) as u64,
                distinct_trademarks: row.get::<_, i64>(5)?.max(0) as u64,
                distinct_product_codes: row.get::<_, i64>(6)?.max(0) as u64,
                distinct_origin_countries: row.get::<_, i64>(7)?.max(0) as u64,
                distinct_dispatch_countries: row.get::<_, i64>(8)?.max(0) as u64,
                distinct_trade_countries: row.get::<_, i64>(9)?.max(0) as u64,
                total_quantity: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                avg_value_per_net_kg: 0.0,
                compatible_usd: None,
                // Placeholder only: the query-level measures are the ones
                // computed above and are assigned to the overview below. Unlike
                // a group or month row, this one is never published empty.
                measures: AnalyticsMeasures::default(),
                ..Default::default()
            })
        })
        .map_err(|err| err.to_string())?;
    overview.total_value_usd = measures.compatible_usd_total().unwrap_or(0.0);
    overview.total_net_kg = measures.total_net_kg();
    overview.total_gross_kg = measures.total_gross_kg();
    overview.avg_value_per_net_kg = measures.compatible_usd_per_net_kg().unwrap_or(0.0);
    overview.compatible_usd = projection_usd_compat(&measures);
    overview.measures = measures;
    Ok(overview)
}

fn projection_months(
    conn: &DuckConnection,
    filter: &DuckFilter,
    query_measures: &AnalyticsMeasures,
) -> Result<Vec<AnalyticsMonthRow>, String> {
    let query_is_usd = query_measures.compatible_usd_total().is_some();
    let net_kg = normalized_weight_sql("net_kg_num");
    // The `*_source` columns are the weights exactly as the source stores them,
    // NOT converted to kilograms: that is the shape `SubsetTotals` expects,
    // because the inherited bucket carries the conversion factor and applies it
    // itself. `paired_*` covers rows that carry both a value and a positive
    // weight, matching how the query-level ratio is built.
    let sql = format!(
        "SELECT
            month,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF(declaration_number, '')) AS declarations_count,
            COALESCE(SUM(value_num), 0.0) AS total_value,
            COALESCE(SUM({net_kg}), 0.0) AS total_net_kg,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND ({net_kg}) > 0
                THEN value_num ELSE 0.0 END), 0.0) AS paired_value,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND ({net_kg}) > 0
                THEN ({net_kg}) ELSE 0.0 END), 0.0) AS paired_weight,
            COUNT(value_num) AS valued_rows,
            COUNT(net_kg_num) AS net_rows,
            COALESCE(SUM(net_kg_num), 0.0) AS net_source_total,
            COUNT(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN 1 END) AS paired_row_count,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN value_num END), 0.0) AS paired_source_value,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN net_kg_num END), 0.0) AS paired_source_net
         FROM records
         {} month IS NOT NULL AND month <> ''
         GROUP BY month
         ORDER BY month DESC
         LIMIT {month_limit}",
        filter.where_extra_sql(),
        month_limit = MONTH_SERIES_LIMIT
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let total_value = row.get::<_, Option<f64>>(3)?.unwrap_or(0.0);
            let total_net_kg = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
            let compatible_usd = inherited_projection_usd(
                query_is_usd,
                total_value,
                row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            );
            Ok(AnalyticsMonthRow {
                month: row.get(0)?,
                rows: row.get::<_, i64>(1)?.max(0) as u64,
                declarations: row.get::<_, i64>(2)?.max(0) as u64,
                total_value_usd: compatible_usd
                    .as_ref()
                    .map(|compatibility| compatibility.total_value_usd)
                    .unwrap_or(0.0),
                total_net_kg,
                compatible_usd,
                // WHY: an empty `AnalyticsMeasures` serializes as no money and
                // no weight at all — `total_value_usd` is `#[serde(skip)]` on
                // this row, so `measures` is the ONLY way a monthly figure
                // reaches the browser, and every such cell rendered as an em
                // dash on DuckDB while SQLite showed the number.
                measures: inherited_measures(
                    query_measures,
                    SubsetTotals {
                        valued_rows: row.get::<_, i64>(7)?.max(0) as u64,
                        total_value,
                        net_rows: row.get::<_, i64>(8)?.max(0) as u64,
                        total_net_source: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                        // A monthly row selects no gross-weight column, so it
                        // must not report a gross bucket, not even an empty one.
                        gross: None,
                        paired_rows: row.get::<_, i64>(10)?.max(0) as u64,
                        paired_value: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
                        paired_net_source: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                        // The projection does not group by currency, so its rows keep the
                        // query-level bucket they have always used. Passing anything else
                        // here would make this engine and SQLite report different money.
                        own_currencies: Vec::new(),
                    },
                ),
            })
        })
        .map_err(|err| err.to_string())?;
    let mut months: Vec<AnalyticsMonthRow> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    months.reverse();
    Ok(months)
}

fn projection_section(
    conn: &DuckConnection,
    filter: &DuckFilter,
    kind: AnalyticsSectionKind,
    hs_level: u8,
    limit: u64,
    overview: &AnalyticsOverview,
) -> Result<AnalyticsSection, String> {
    let Some(grouping) = projection_section_grouping(kind, hs_level) else {
        return Ok(AnalyticsSection {
            kind,
            rows: Vec::new(),
        });
    };
    let label_sql = grouping.label_sql;
    let net_kg = normalized_weight_sql("net_kg_num");
    let gross_kg = normalized_weight_sql("gross_kg_num");
    let query_is_usd = overview.compatible_usd.is_some();
    // WHY the PLAIN value sum ranks and shares these rows, not the USD-compatible
    // one: `analytics_repo::section` orders by `COALESCE(SUM(value), 0.0)` and
    // computes every share against that same sum, for compatible and
    // incompatible queries alike. Reading the compatibility total here instead
    // made DuckDB rank by weight whenever the set was not one known USD cohort —
    // which is always on the customs profile, because it has no currency column
    // at all, so `compatible_usd` is None even though every row carries a value.
    // A different ranking is a different top-N as soon as `LIMIT` bites, so the
    // two engines returned different group rows for the same question, and the
    // share column was computed from money on SQLite and from weight here.
    // Ordering and shares are rankings, not published money, so using the
    // cross-currency sum for them says nothing the wire has to defend.
    let share_total_value: f64 = overview
        .measures
        .currency_totals
        .iter()
        .map(|total| total.total_value)
        .sum();
    let sql = format!(
        "SELECT
            {label_sql} AS label,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF(declaration_number, '')) AS declarations_count,
            COUNT(DISTINCT NULLIF(edrpou_label, '')) AS companies_count,
            COALESCE(SUM(value_num), 0.0) AS total_value,
            COALESCE(SUM({net_kg}), 0.0) AS total_net_kg,
            COALESCE(SUM({gross_kg}), 0.0) AS total_gross_kg,
            COALESCE(SUM(quantity_num), 0.0) AS total_quantity,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND ({net_kg}) > 0
                THEN value_num ELSE 0.0 END), 0.0) AS paired_value,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND ({net_kg}) > 0
                THEN ({net_kg}) ELSE 0.0 END), 0.0) AS paired_weight,
            COUNT(value_num) AS valued_rows,
            COUNT(net_kg_num) AS net_rows,
            COUNT(gross_kg_num) AS gross_rows,
            COALESCE(SUM(net_kg_num), 0.0) AS net_source_total,
            COALESCE(SUM(gross_kg_num), 0.0) AS gross_source_total,
            COUNT(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN 1 END) AS paired_row_count,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN value_num END), 0.0) AS paired_source_value,
            COALESCE(SUM(CASE WHEN value_num IS NOT NULL AND net_kg_num > 0
                THEN net_kg_num END), 0.0) AS paired_source_net
         FROM records
         {} {label_sql} IS NOT NULL AND {label_sql} <> ''
         GROUP BY {label_sql}
         ORDER BY total_value DESC, total_net_kg DESC, rows_count DESC, label
         LIMIT {}",
        filter.where_extra_sql(),
        limit.clamp(1, 20_000)
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let label: String = row.get(0)?;
            let rows_count = row.get::<_, i64>(1)?.max(0) as u64;
            let total_value: f64 = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
            let total_net_kg: f64 = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
            let total_gross_kg: f64 = row.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
            let total_quantity: f64 = row.get::<_, Option<f64>>(7)?.unwrap_or(0.0);
            let compatible_usd = inherited_projection_usd(
                query_is_usd,
                total_value,
                row.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
            );
            let share_base = if share_total_value > 0.0 {
                share_total_value
            } else if overview.total_net_kg > 0.0 {
                overview.total_net_kg
            } else {
                overview.row_count as f64
            };
            let share_value = if share_total_value > 0.0 {
                total_value
            } else if overview.total_net_kg > 0.0 {
                total_net_kg
            } else {
                rows_count as f64
            };
            Ok(AnalyticsGroupRow {
                filter_action: grouping.filter_field.map(|field| AnalyticsFilterAction {
                    field,
                    value: label.clone(),
                }),
                label,
                rows: rows_count,
                declarations: row.get::<_, i64>(2)?.max(0) as u64,
                companies: row.get::<_, i64>(3)?.max(0) as u64,
                total_value_usd: compatible_usd
                    .as_ref()
                    .map(|compatibility| compatibility.total_value_usd)
                    .unwrap_or(0.0),
                total_net_kg,
                total_gross_kg,
                total_quantity,
                share_percent: ratio(share_value * 100.0, share_base),
                avg_value_per_net_kg: compatible_usd
                    .as_ref()
                    .and_then(|compatibility| compatibility.avg_value_per_net_kg)
                    .unwrap_or(0.0),
                compatible_usd,
                // WHY: `total_value_usd` is `#[serde(skip)]` on a group row, so
                // an empty `AnalyticsMeasures` means the section table shows an
                // em dash for money, weight and value/kg even though every sum
                // was already computed by this same scan.
                measures: inherited_measures(
                    &overview.measures,
                    SubsetTotals {
                        valued_rows: row.get::<_, i64>(10)?.max(0) as u64,
                        total_value,
                        net_rows: row.get::<_, i64>(11)?.max(0) as u64,
                        total_net_source: row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
                        gross: Some((
                            row.get::<_, i64>(12)?.max(0) as u64,
                            row.get::<_, Option<f64>>(14)?.unwrap_or(0.0),
                        )),
                        paired_rows: row.get::<_, i64>(15)?.max(0) as u64,
                        paired_value: row.get::<_, Option<f64>>(16)?.unwrap_or(0.0),
                        paired_net_source: row.get::<_, Option<f64>>(17)?.unwrap_or(0.0),
                        // The projection does not group by currency, so its rows keep the
                        // query-level bucket they have always used. Passing anything else
                        // here would make this engine and SQLite report different money.
                        own_currencies: Vec::new(),
                    },
                ),
            })
        })
        .map_err(|err| err.to_string())?;
    Ok(AnalyticsSection {
        kind,
        rows: rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?,
    })
}

struct ProjectionSectionGrouping {
    label_sql: String,
    filter_field: Option<AnalyticsFilterField>,
}

fn projection_section_grouping(
    kind: AnalyticsSectionKind,
    hs_level: u8,
) -> Option<ProjectionSectionGrouping> {
    let grouping = |label_sql: &str, filter_field| {
        Some(ProjectionSectionGrouping {
            label_sql: label_sql.to_string(),
            filter_field: Some(filter_field),
        })
    };
    match kind {
        AnalyticsSectionKind::Recipients => {
            grouping("recipient_label", AnalyticsFilterField::Recipient)
        }
        AnalyticsSectionKind::Senders => grouping("sender_label", AnalyticsFilterField::Sender),
        AnalyticsSectionKind::Edrpou => grouping("edrpou_label", AnalyticsFilterField::Edrpou),
        AnalyticsSectionKind::ProductCodes => {
            let label_sql = if hs_level >= 10 {
                "product_code".to_string()
            } else {
                format!("SUBSTR(product_code, 1, {})", hs_level.clamp(2, 8))
            };
            Some(ProjectionSectionGrouping {
                label_sql,
                filter_field: Some(AnalyticsFilterField::ProductCode),
            })
        }
        AnalyticsSectionKind::Trademarks => {
            grouping("trademark_label", AnalyticsFilterField::Trademark)
        }
        AnalyticsSectionKind::ProductGroups => Some(ProjectionSectionGrouping {
            label_sql: "SUBSTR(description, 1, 80)".to_string(),
            filter_field: Some(AnalyticsFilterField::Description),
        }),
        AnalyticsSectionKind::OriginCountries => {
            grouping("origin_key", AnalyticsFilterField::OriginCountry)
        }
        AnalyticsSectionKind::DispatchCountries => {
            grouping("dispatch_key", AnalyticsFilterField::DispatchCountry)
        }
        AnalyticsSectionKind::TradeCountries => {
            grouping("trade_key", AnalyticsFilterField::TradeCountry)
        }
    }
}

fn projection_price_metrics(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<Vec<AnalyticsPriceMetric>, String> {
    let net_kg = normalized_weight_sql("net_kg_num");
    let gross_kg = normalized_weight_sql("gross_kg_num");
    Ok(vec![
        projection_value_per_net_weight_metric(conn, filter)?,
        projection_price_metric(conn, filter, PriceMetricKind::RfvUsdKg, "rfv_num", &net_kg)?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::RmvNetUsdKg,
            "rmv_net_num",
            &net_kg,
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::RmvUsdExtraUnit,
            "rmv_extra_num",
            "quantity_num",
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::RmvGrossUsdKg,
            "rmv_gross_num",
            &gross_kg,
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::MinBaseUsdKg,
            "min_base_num",
            &net_kg,
        )?,
    ])
}

fn projection_value_per_net_weight_metric(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<AnalyticsPriceMetric, String> {
    let net_kg = normalized_weight_sql("net_kg_num");
    let known_currency = format!(
        "COALESCE(currency_key, '') <> ''
         AND NOT starts_with(currency_key, '{UNKNOWN_CURRENCY_KEY}')"
    );
    let sql = format!(
        "WITH priced AS (
            SELECT
                currency_key,
                weight_unit_key,
                value_num,
                ({net_kg}) AS net_weight_kg,
                value_num / ({net_kg}) AS price
            FROM records
            {} value_num IS NOT NULL
              AND ({net_kg}) > 0
              AND {known_currency}
         )
         SELECT
            currency_key,
            COUNT(*) AS sample_count,
            AVG(price),
            MIN(price),
            MAX(price),
            SUM(value_num) / NULLIF(SUM(net_weight_kg), 0),
            quantile_cont(price, 0.25),
            quantile_cont(price, 0.5),
            quantile_cont(price, 0.75),
            SUM(value_num),
            SUM(net_weight_kg)
         FROM priced
         GROUP BY currency_key
         ORDER BY currency_key",
        filter.where_extra_sql()
    );
    let mut statement = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(AnalyticsPriceCohort {
                currency: row.get(0)?,
                normalized_weight_unit: "kg".to_string(),
                source_weight_units: Vec::new(),
                count: row.get::<_, i64>(1)?.max(0) as u64,
                average: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                minimum: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                maximum: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                weighted_average: row.get(5)?,
                p25: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                median: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                p75: row.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                numerator_total: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                denominator_total: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
            })
        })
        .map_err(|err| err.to_string())?;
    let mut cohorts = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;

    let units_sql = format!(
        "SELECT DISTINCT currency_key, weight_unit_key
         FROM records
         {} value_num IS NOT NULL
           AND ({net_kg}) > 0
           AND {known_currency}
         ORDER BY currency_key, weight_unit_key",
        filter.where_extra_sql()
    );
    let mut units_statement = conn.prepare(&units_sql).map_err(|err| err.to_string())?;
    let unit_rows = units_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| err.to_string())?;
    let mut units = BTreeMap::<String, Vec<String>>::new();
    for row in unit_rows {
        let (currency, unit) = row.map_err(|err| err.to_string())?;
        units.entry(currency).or_default().push(unit);
    }
    for cohort in &mut cohorts {
        cohort.source_weight_units = units.remove(&cohort.currency).unwrap_or_default();
    }

    let excluded_sql = format!(
        "SELECT COUNT(*) FROM records
         {} value_num IS NOT NULL AND (
            NOT ({known_currency}) OR net_kg_num IS NULL OR ({net_kg}) IS NULL OR ({net_kg}) <= 0
         )",
        filter.where_extra_sql()
    );
    let excluded_rows = query_count(conn, &excluded_sql)?;
    let compatible = (cohorts.len() == 1).then(|| &cohorts[0]);
    Ok(AnalyticsPriceMetric {
        kind: PriceMetricKind::ValuePerNetKg,
        count: compatible.map(|cohort| cohort.count).unwrap_or(0),
        average: compatible.map(|cohort| cohort.average).unwrap_or(0.0),
        minimum: compatible.map(|cohort| cohort.minimum).unwrap_or(0.0),
        maximum: compatible.map(|cohort| cohort.maximum).unwrap_or(0.0),
        weighted_average: compatible
            .and_then(|cohort| cohort.weighted_average)
            .unwrap_or(0.0),
        median: compatible.map(|cohort| cohort.median).unwrap_or(0.0),
        p25: compatible.map(|cohort| cohort.p25).unwrap_or(0.0),
        p75: compatible.map(|cohort| cohort.p75).unwrap_or(0.0),
        cohorts,
        excluded_rows,
    })
}

fn projection_price_metric(
    conn: &DuckConnection,
    filter: &DuckFilter,
    kind: PriceMetricKind,
    price_expr: &str,
    weight_expr: &str,
) -> Result<AnalyticsPriceMetric, String> {
    let sql = format!(
        "WITH priced AS (
            SELECT {price_expr} AS price, {weight_expr} AS weight
            FROM records {}
         ),
         agg AS (
            SELECT
                COUNT(price) AS price_count,
                AVG(price) AS price_avg,
                MIN(price) AS price_min,
                MAX(price) AS price_max,
                SUM(CASE WHEN price IS NOT NULL AND weight IS NOT NULL AND weight > 0
                    THEN price * weight ELSE 0 END) AS weighted_sum,
                SUM(CASE WHEN price IS NOT NULL AND weight IS NOT NULL AND weight > 0
                    THEN weight ELSE 0 END) AS weighted_kg,
                quantile_cont(price, 0.25) AS p25,
                quantile_cont(price, 0.5) AS p50,
                quantile_cont(price, 0.75) AS p75
            FROM priced
         )
         SELECT
            price_count,
            price_avg,
            price_min,
            price_max,
            weighted_sum,
            weighted_kg,
            p25,
            p50,
            p75
         FROM agg",
        filter.where_sql()
    );
    conn.query_row(&sql, [], |row| {
        let weighted_sum = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
        let weighted_kg = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
        Ok(AnalyticsPriceMetric {
            kind,
            count: row.get::<_, i64>(0)?.max(0) as u64,
            average: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            minimum: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            maximum: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            weighted_average: ratio(weighted_sum, weighted_kg),
            p25: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            median: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
            p75: row.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
            // These source metrics are explicitly USD-denominated by their
            // semantic contract; only value-per-weight needs currency cohorts.
            cohorts: Vec::new(),
            excluded_rows: 0,
        })
    })
    .map_err(|err| err.to_string())
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

fn count_group_rows(
    conn: &DuckConnection,
    filter: &DuckFilter,
    field: &str,
    limit: u64,
) -> Result<u64, String> {
    query_count(
        conn,
        &format!(
            "SELECT COUNT(*) FROM (
                SELECT {field}, COUNT(*) rows_count, SUM(value_num) total_value, SUM(net_kg_num) net
                FROM records
                {} {field} IS NOT NULL AND {field} <> ''
                GROUP BY {field}
                ORDER BY total_value DESC NULLS LAST, net DESC NULLS LAST, rows_count DESC
                LIMIT {}
            )",
            filter.where_extra_sql(),
            limit.clamp(1, 200)
        ),
    )
}

fn query_count(conn: &DuckConnection, sql: &str) -> Result<u64, String> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|value| value.max(0) as u64)
        .map_err(|err| err.to_string())
}

fn measure_duck_scenario(
    options: &OlapBenchmarkOptions,
    name: &'static str,
    category: &'static str,
    note: &'static str,
    mut run: impl FnMut() -> Result<u64, String>,
) -> Result<OlapScenarioReport, String> {
    for _ in 0..options.warmups {
        run()?;
    }
    let repeat = options.repeat.max(1);
    let mut runs_ms = Vec::with_capacity(repeat);
    let mut output_rows = 0;
    for _ in 0..repeat {
        let started = Instant::now();
        output_rows = run()?;
        runs_ms.push(round_ms(started.elapsed().as_secs_f64() * 1000.0));
    }
    Ok(OlapScenarioReport {
        name,
        category,
        output_rows,
        average_ms: round_ms(runs_ms.iter().sum::<f64>() / runs_ms.len() as f64),
        minimum_ms: runs_ms.iter().copied().fold(f64::INFINITY, f64::min),
        maximum_ms: runs_ms.iter().copied().fold(0.0, f64::max),
        runs_ms,
        note,
    })
}

struct DuckFilter {
    conditions: Vec<String>,
}

impl DuckFilter {
    fn from_query(query: &Query) -> Self {
        let mut conditions = Vec::new();
        if query.record_scope == RecordScope::Canonical {
            conditions.push("dup_first_file IS NULL".to_string());
        }
        let text = query.text.trim();
        if !text.is_empty() {
            // WHY `to_lowercase` and not `to_ascii_lowercase`: the column side is
            // folded by DuckDB's `lower()`, which is Unicode-aware. ASCII-only
            // folding leaves "Відправник" untouched, so an upper- or mixed-case
            // Cyrillic needle could never equal the folded column and the query
            // matched nothing at all. SQLite folds the needle with the same
            // Unicode-aware `to_lowercase` before handing it to `cyr_contains`.
            let needle = sql_string(&text.to_lowercase());
            conditions.push(format!(
                "contains(lower(coalesce(description, '') || ' ' || coalesce(sender_label, '') ||
                 ' ' || coalesce(recipient_label, '') || ' ' || coalesce(product_code, '') || ' '
                 || coalesce(trademark_label, '')), {needle})"
            ));
        }
        let filters = &query.filters;
        push_year_eq(&mut conditions, &filters.year);
        push_prefix(&mut conditions, "product_code_text", &filters.product_code);
        // Trademark matches the SQLite semantics: exact, case-insensitive,
        // whitespace-collapsed comparison — not a substring search.
        {
            let trademark = filters.trademark.trim();
            if !trademark.is_empty() {
                let needle = sql_string(&crate::storage::normalize::normalize_text_key(trademark));
                conditions.push(format!("trademark_key = {needle}"));
            }
        }
        push_contains(&mut conditions, "description_text", &filters.description);
        push_contains(&mut conditions, "sender_text", &filters.sender);
        push_contains(&mut conditions, "recipient_text", &filters.recipient);
        // EDRPOU is an exact company-code match in SQLite, not a substring.
        push_normalized_eq(&mut conditions, "edrpou_key", &filters.edrpou);
        // Country filters compare normalized keys, so synonyms such as
        // Localized country names resolve to the same code as ISO values, matching SQLite.
        push_country_eq(&mut conditions, "trade_key", &filters.trade_country);
        push_country_eq(&mut conditions, "dispatch_key", &filters.dispatch_country);
        push_country_eq(&mut conditions, "origin_key", &filters.origin_country);
        Self { conditions }
    }

    fn where_sql(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.conditions.join(" AND "))
        }
    }

    fn where_extra_sql(&self) -> String {
        if self.conditions.is_empty() {
            "WHERE".to_string()
        } else {
            format!("WHERE {} AND", self.conditions.join(" AND "))
        }
    }
}

/// The year filter, read exactly the way `query_plan.rs` reads it.
///
/// WHY `parse_year` and not `parse::<i64>()`: SQLite accepts any value holding
/// four digits ("2024 р.", "2024-"), because that is what a user types and what
/// `search_sql` already resolves. A plain integer parse rejects those, and this
/// helper's failure mode is to push NO condition at all — so the same filter
/// narrowed the result on SQLite and returned the WHOLE database here. A filter
/// that is silently ignored is worse than one that finds nothing.
fn push_year_eq(conditions: &mut Vec<String>, value: &str) {
    if let Some(year) = crate::storage::normalize::parse_year(value) {
        conditions.push(format!("year = {year}"));
    }
}

/// Equality against a normalized country key, mirroring the SQLite filter that
/// maps synonyms and localized names onto one ISO-like code before comparing.
fn push_country_eq(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let key = crate::storage::normalize::normalize_country_key(value);
    conditions.push(format!("{field} = {}", sql_string(&key)));
}

fn push_normalized_eq(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        let key = crate::storage::normalize::normalize_text_key(value);
        conditions.push(format!("{field} = {}", sql_string(&key)));
    }
}

fn push_prefix(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        conditions.push(format!(
            "starts_with(coalesce({field}, ''), {})",
            sql_string(value)
        ));
    }
}

/// Case-insensitive substring match, the DuckDB twin of SQLite's
/// `cyr_contains(column, lowercased_needle)`.
///
/// WHY `to_lowercase`: `lower()` inside DuckDB folds the column with full
/// Unicode rules, so the needle has to be folded the same way. With
/// `to_ascii_lowercase` every Cyrillic letter stayed uppercase in the needle
/// while the column arrived lowercase, and the "Відправник", "Одержувач" and
/// "Опис" filters therefore returned zero rows for any Ukrainian company name —
/// the one thing those filters exist for. `query_plan.rs` folds the same needle
/// with Rust's Unicode-aware `to_lowercase`.
fn push_contains(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        conditions.push(format!(
            "contains(lower(coalesce({field}, '')), {})",
            sql_string(&value.to_lowercase())
        ));
    }
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() < f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

fn round_ms(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::import;
    use crate::schema::{COLUMNS, col_index};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn projection_build_writes_readable_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let sqlite_path = dir.path().join("source.db");
        let projection_path = dir.path().join("projection.duckdb");
        Db::open(&sqlite_path).unwrap();

        let build = build_projection(&sqlite_path, &projection_path).unwrap();
        assert_eq!(build.rows, 0);
        assert_eq!(build.max_record_id, 0);
        assert_eq!(build.schema_version, PROJECTION_SCHEMA_VERSION);
        assert_eq!(build.rollup_schema_version, ROLLUP_SCHEMA_VERSION);
        assert_eq!(build.rollup_rules_version, ROLLUP_RULES_VERSION);
        assert_eq!(build.rollup_fingerprint, rollup_contract_fingerprint());

        let meta = read_projection_meta(&projection_path).unwrap();
        assert_eq!(meta.rows, 0);
        assert_eq!(meta.max_record_id, 0);
        assert_eq!(meta.schema_version, PROJECTION_SCHEMA_VERSION);
        assert_eq!(meta.rollup_schema_version, ROLLUP_SCHEMA_VERSION);
        assert_eq!(meta.rollup_rules_version, ROLLUP_RULES_VERSION);
        assert_eq!(meta.rollup_fingerprint, rollup_contract_fingerprint());
        assert_eq!(meta.source_sqlite, sqlite_path.display().to_string());
        assert!(!meta.built_at.is_empty());
    }

    #[test]
    fn projection_analytics_match_sqlite_for_supported_scope() {
        let dir = tempfile::tempdir().unwrap();
        let sqlite_path = dir.path().join("source.db");
        let xlsx_path = dir.path().join("source.xlsx");
        let projection_path = dir.path().join("projection.duckdb");
        write_test_xlsx(
            &xlsx_path,
            &[
                vec![
                    ("declaration_number", "24UA100000000001U1"),
                    ("declaration_date", "15.03.2024"),
                    ("sender", "APPLE EXPORT LTD"),
                    ("edrpou", "11111111"),
                    ("recipient", "TECH IMPORT A"),
                    ("product_code", "8517120000"),
                    ("description", "Apple iPhone"),
                    ("trade_country", "CN"),
                    ("dispatch_country", "CN"),
                    ("origin_country", "CN"),
                    ("quantity", "5"),
                    ("gross_kg", "12"),
                    ("net_kg", "10"),
                    ("currency_control_value", "1000"),
                    ("rfv_usd_kg", "100"),
                    ("rmv_net_usd_kg", "110"),
                    ("rmv_usd_extra_unit", "200"),
                    ("rmv_gross_usd_kg", "90"),
                    ("min_base_usd_kg", "80"),
                    ("trademark", "APPLE"),
                ],
                vec![
                    ("declaration_number", "24UA100000000002U2"),
                    ("declaration_date", "16.03.2024"),
                    ("sender", "APPLE EXPORT LTD"),
                    ("edrpou", "22222222"),
                    ("recipient", "TECH IMPORT B"),
                    ("product_code", "8517130000"),
                    ("description", "Apple Watch"),
                    ("trade_country", "US"),
                    ("dispatch_country", "US"),
                    ("origin_country", "US"),
                    ("quantity", "2"),
                    ("gross_kg", "4"),
                    ("net_kg", "3"),
                    ("currency_control_value", "600"),
                    ("rfv_usd_kg", "200"),
                    ("rmv_net_usd_kg", "220"),
                    ("rmv_usd_extra_unit", "300"),
                    ("rmv_gross_usd_kg", "180"),
                    ("min_base_usd_kg", "160"),
                    ("trademark", "APPLE"),
                ],
            ],
        );
        let mut db = Db::open(&sqlite_path).unwrap();
        let cancel = AtomicBool::new(false);
        let summary = import::import_file(&mut db, &xlsx_path, &cancel, &mut |_, _, _| {});
        assert_eq!(summary.error, None);
        assert_eq!(summary.imported, 2);
        build_projection(&sqlite_path, &projection_path).unwrap();

        let query = Query {
            text: "Apple".to_string(),
            ..Default::default()
        };
        let sqlite = db
            .analytics_scoped(&query, 10, Some(AnalyticsScope::Products), 10)
            .unwrap();
        let projected = analytics_scoped(
            &projection_path,
            &query,
            10,
            Some(AnalyticsScope::Products),
            10,
        )
        .unwrap();

        assert_eq!(projected.overview.row_count, sqlite.overview.row_count);
        assert_eq!(
            projected.overview.declaration_count,
            sqlite.overview.declaration_count
        );
        assert_eq!(
            projected.overview.distinct_trademarks,
            sqlite.overview.distinct_trademarks
        );
        assert_eq!(projected.overview.measures, sqlite.overview.measures);
        assert_eq!(projected.top_trademarks.len(), sqlite.top_trademarks.len());
        assert_eq!(
            projected.top_product_codes.len(),
            sqlite.top_product_codes.len()
        );

        let sqlite_prices = db
            .analytics_scoped(&query, 10, Some(AnalyticsScope::Prices), 10)
            .unwrap();
        let projected_prices = analytics_scoped(
            &projection_path,
            &query,
            10,
            Some(AnalyticsScope::Prices),
            10,
        )
        .unwrap();
        assert_eq!(
            projected_prices.price_sections.len(),
            sqlite_prices.price_sections.len()
        );
        if projected_prices.price_sections[0].cohorts.len() == 1 {
            assert_eq!(
                projected_prices.price_sections[0].count,
                sqlite_prices.price_sections[0].count
            );
            assert_eq!(
                projected_prices.price_sections[0].median,
                sqlite_prices.price_sections[0].median
            );
        } else {
            assert_eq!(projected_prices.price_sections[0].count, 0);
        }
        for kind in [
            PriceMetricKind::RfvUsdKg,
            PriceMetricKind::RmvNetUsdKg,
            PriceMetricKind::RmvUsdExtraUnit,
            PriceMetricKind::RmvGrossUsdKg,
            PriceMetricKind::MinBaseUsdKg,
        ] {
            let projected = projected_prices
                .price_sections
                .iter()
                .find(|metric| metric.kind == kind)
                .unwrap();
            let sqlite = sqlite_prices
                .price_sections
                .iter()
                .find(|metric| metric.kind == kind)
                .unwrap();
            assert_eq!(projected.weighted_average, sqlite.weighted_average);
        }
    }

    fn write_test_xlsx(path: &Path, rows: &[Vec<(&str, &str)>]) {
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet = workbook.add_worksheet();
        for (col, def) in COLUMNS.iter().enumerate() {
            sheet.write_string(0, col as u16, def.header).unwrap();
        }
        for (row_index, row) in rows.iter().enumerate() {
            for (name, value) in row {
                let column = col_index(name).unwrap() as u16;
                sheet
                    .write_string(row_index as u32 + 1, column, *value)
                    .unwrap();
            }
        }
        workbook.save(path).unwrap();
    }
}
