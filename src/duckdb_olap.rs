use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use duckdb::{AccessMode, Config, Connection as DuckConnection, params as duck_params};
use rusqlite::{Connection as SqliteConnection, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::db::{
    Analytics, AnalyticsFilterAction, AnalyticsFilterField, AnalyticsGroupRow, AnalyticsMeasures,
    AnalyticsMonthRow, AnalyticsOverview, AnalyticsPriceMetric, AnalyticsScope, AnalyticsSection,
    AnalyticsSectionKind, AnalyticsUsdCompatibility, PriceMetricKind, Query, RecordScope,
};
use crate::domain::table::SemanticField;
use crate::olap::{OlapBenchmarkOptions, OlapBenchmarkReport, OlapScenarioReport};
use crate::storage::analytics_columns::AnalyticsColumns;
use crate::storage::{connection as storage_connection, effective_rows, table_shape};

/// The projection materializes the legacy USD-denominated value column, so its
/// aggregates form a single known USD cohort by construction. The trust guard
/// additionally refuses the projection whenever its overview cannot reproduce
/// the SQLite totals.
fn projection_usd_compat(
    total_value_usd: f64,
    total_net_kg: f64,
) -> Option<AnalyticsUsdCompatibility> {
    Some(AnalyticsUsdCompatibility {
        total_value_usd,
        avg_value_per_net_kg: (total_net_kg > 0.0).then(|| total_value_usd / total_net_kg),
    })
}

pub const PROJECTION_SCHEMA_VERSION: &str = "6";
pub const ROLLUP_SCHEMA_VERSION: &str = "1";
pub const ROLLUP_RULES_VERSION: &str = "1";

const ROLLUP_CONTRACT: &str = concat!(
    "overview:v1;monthly:v1;sections:v1;currency:v1;price_per_kg:v1;",
    "scope:canonical|occurrences;years:all|calendar;",
    "money:usd-only-public|partitioned-storage;hs:2-8|10;r7-quantiles"
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
        let sql = projection_select_sql(&transaction);
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
    hash_part(&mut hasher, "semantic_mapping", &semantic_mapping);
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

fn projection_select_sql(conn: &SqliteConnection) -> String {
    let columns =
        AnalyticsColumns::for_alias(table_shape::effective(conn), effective_rows::PAYLOAD_ALIAS);
    let label = |field| columns.label(field).unwrap_or_else(|| "''".to_string());
    let text = |field| columns.text(field).unwrap_or_else(|| "''".to_string());
    let country = |field| {
        columns
            .country_key(field)
            .unwrap_or_else(|| "''".to_string())
    };
    let number = |field| columns.number(field).unwrap_or_else(|| "NULL".to_string());
    let month = columns
        .month(SemanticField::Date)
        .unwrap_or_else(|| "''".to_string());
    let year = format!(
        "COALESCE({}.year, CAST(NULLIF(SUBSTR({month}, 1, 4), '') AS INTEGER))",
        effective_rows::PAYLOAD_ALIAS
    );
    let declaration = label(SemanticField::DeclarationNumber);
    let sender_label = label(SemanticField::Sender);
    let sender_text = text(SemanticField::Sender);
    let recipient_label = label(SemanticField::Recipient);
    let recipient_text = text(SemanticField::Recipient);
    let edrpou_label = label(SemanticField::CompanyCode);
    let edrpou_text = text(SemanticField::CompanyCode);
    let product_code = label(SemanticField::ProductCode);
    let product_code_text = text(SemanticField::ProductCode);
    let description = label(SemanticField::Description);
    let description_text = text(SemanticField::Description);
    let trademark_label = label(SemanticField::Trademark);
    let trademark_text = text(SemanticField::Trademark);
    let origin = country(SemanticField::OriginCountry);
    let dispatch = country(SemanticField::DispatchCountry);
    let trade = country(SemanticField::TradeCountry);
    let value = number(SemanticField::Value);
    let net = number(SemanticField::NetWeight);
    let gross = number(SemanticField::GrossWeight);
    let quantity = number(SemanticField::Quantity);
    let currency = text(SemanticField::Currency);
    let weight_unit = text(SemanticField::WeightUnit);

    format!(
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
            UPPER(text_key({currency})),
            UPPER(text_key({weight_unit})),
            r.dup_first_file
         FROM records r{}
         ORDER BY r.id",
        effective_rows::payload_join()
    )
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
    let months = projection_months(&conn, &filter)?;
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
            && matches!(weight.as_str(), "kg" | "empty")
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
    let overview = rollup_overview(conn, selector)?;
    let months = rollup_months(conn, selector)?;
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
) -> Result<AnalyticsOverview, String> {
    conn.query_row(
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
            total_value_usd,
            total_gross_kg,
            total_net_kg,
            total_quantity
         FROM rollup_overview
         WHERE record_scope = ? AND year_key = ? LIMIT 1",
        duck_params![selector.record_scope, selector.year_key],
        |row| {
            let total_value_usd = row.get::<_, Option<f64>>(10)?.unwrap_or(0.0);
            let total_net_kg = row.get::<_, Option<f64>>(12)?.unwrap_or(0.0);
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
                total_value_usd,
                total_gross_kg: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
                total_net_kg,
                total_quantity: row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
                avg_value_per_net_kg: ratio(total_value_usd, total_net_kg),
                compatible_usd: projection_usd_compat(total_value_usd, total_net_kg),
                measures: AnalyticsMeasures::default(),
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
    .map_err(|err| format!("Could not read DuckDB overview rollup: {err}"))
}

fn rollup_months(
    conn: &DuckConnection,
    selector: RollupSelector,
) -> Result<Vec<AnalyticsMonthRow>, String> {
    let mut statement = conn
        .prepare(
            "SELECT month, rows_count, declarations_count, total_value_usd, total_net_kg
             FROM rollup_monthly
             WHERE record_scope = ? AND year_key = ?
             ORDER BY month DESC LIMIT 48",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map(
            duck_params![selector.record_scope, selector.year_key],
            |row| {
                let total_value_usd = row.get::<_, Option<f64>>(3)?.unwrap_or(0.0);
                let total_net_kg = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
                Ok(AnalyticsMonthRow {
                    month: row.get(0)?,
                    rows: row.get::<_, i64>(1)?.max(0) as u64,
                    declarations: row.get::<_, i64>(2)?.max(0) as u64,
                    total_value_usd,
                    total_net_kg,
                    compatible_usd: projection_usd_compat(total_value_usd, total_net_kg),
                    measures: AnalyticsMeasures::default(),
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
    let mut statement = conn
        .prepare(
            "SELECT label, rows_count, declarations_count, companies_count,
                    total_value_usd, total_net_kg, total_gross_kg, total_quantity
             FROM rollup_sections
             WHERE record_scope = ? AND year_key = ? AND kind = ? AND hs_level = ?
             ORDER BY total_value_usd DESC NULLS LAST, total_net_kg DESC,
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
                let share_base = if overview.total_value_usd > 0.0 {
                    overview.total_value_usd
                } else if overview.total_net_kg > 0.0 {
                    overview.total_net_kg
                } else {
                    overview.row_count as f64
                };
                let share_value = if overview.total_value_usd > 0.0 {
                    total_value_usd
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
                    total_value_usd,
                    total_net_kg,
                    total_gross_kg,
                    total_quantity,
                    share_percent: ratio(share_value * 100.0, share_base),
                    avg_value_per_net_kg: ratio(total_value_usd, total_net_kg),
                    compatible_usd: projection_usd_compat(total_value_usd, total_net_kg),
                    measures: AnalyticsMeasures::default(),
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

fn build_rollups(conn: &DuckConnection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE VIEW rollup_records AS
            SELECT 'occurrences'::VARCHAR AS record_scope, records.* FROM records
            UNION ALL
            SELECT 'canonical'::VARCHAR AS record_scope, records.*
            FROM records WHERE dup_first_file IS NULL;

         CREATE VIEW rollup_expanded AS
            SELECT rollup_records.*, 0::BIGINT AS year_key FROM rollup_records
            UNION ALL
            SELECT rollup_records.*, year AS year_key
            FROM rollup_records WHERE year IS NOT NULL;

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
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') = ''
                    ) > 0 THEN NULL
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 'USD' THEN SUM(value_num)
                    ELSE NULL
                END AS total_value_usd,
                COALESCE(SUM(gross_kg_num), 0.0) AS total_gross_kg,
                COALESCE(SUM(net_kg_num), 0.0) AS total_net_kg,
                COALESCE(SUM(quantity_num), 0.0) AS total_quantity,
                CASE
                    WHEN COUNT(value_num) = 0 THEN 'empty'
                    WHEN COUNT(*) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') = ''
                    ) > 0 THEN 'unavailable'
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 'USD' THEN 'single_usd'
                    ELSE 'partitioned'
                END AS monetary_mode,
                CASE
                    WHEN COUNT(net_kg_num) = 0 THEN 'empty'
                    WHEN COUNT(*) FILTER (
                        WHERE net_kg_num IS NOT NULL AND COALESCE(weight_unit_key, '') = ''
                    ) > 0 THEN 'unavailable'
                    WHEN COUNT(DISTINCT weight_unit_key) FILTER (
                        WHERE net_kg_num IS NOT NULL AND COALESCE(weight_unit_key, '') <> ''
                    ) = 1 AND MIN(weight_unit_key) FILTER (
                        WHERE net_kg_num IS NOT NULL AND COALESCE(weight_unit_key, '') <> ''
                    ) = 'KG' THEN 'kg'
                    ELSE 'partitioned'
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
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') = ''
                    ) > 0 THEN NULL
                    WHEN COUNT(DISTINCT currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 1 AND MIN(currency_key) FILTER (
                        WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                    ) = 'USD' THEN SUM(value_num)
                    ELSE NULL
                END AS total_value_usd,
                COALESCE(SUM(net_kg_num), 0.0) AS total_net_kg
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
            total_quantity DOUBLE
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
                    net_kg_num,
                    value_num / net_kg_num AS price_per_kg
                FROM rollup_expanded
                WHERE value_num IS NOT NULL
                  AND net_kg_num IS NOT NULL
                  AND net_kg_num > 0
                  AND COALESCE(currency_key, '') <> ''
                  AND COALESCE(weight_unit_key, '') <> ''
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
                SUM(value_num) / NULLIF(SUM(net_kg_num), 0) AS weighted_average,
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
                    WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') = ''
                ) > 0 THEN NULL
                WHEN COUNT(DISTINCT currency_key) FILTER (
                    WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                ) = 1 AND MIN(currency_key) FILTER (
                    WHERE value_num IS NOT NULL AND COALESCE(currency_key, '') <> ''
                ) = 'USD' THEN SUM(value_num)
                ELSE NULL
            END,
            COALESCE(SUM(net_kg_num), 0.0),
            COALESCE(SUM(gross_kg_num), 0.0),
            COALESCE(SUM(quantity_num), 0.0)
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

fn projection_overview(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<AnalyticsOverview, String> {
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
            COALESCE(SUM(value_num), 0.0),
            COALESCE(SUM(gross_kg_num), 0.0),
            COALESCE(SUM(net_kg_num), 0.0),
            COALESCE(SUM(quantity_num), 0.0)
         FROM records {}",
        filter.where_sql()
    );
    let overview = conn
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
                total_value_usd: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                total_gross_kg: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
                total_net_kg: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                total_quantity: row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
                avg_value_per_net_kg: 0.0,
                compatible_usd: None,
                measures: AnalyticsMeasures::default(),
            })
        })
        .map_err(|err| err.to_string())?;
    Ok(AnalyticsOverview {
        avg_value_per_net_kg: ratio(overview.total_value_usd, overview.total_net_kg),
        compatible_usd: projection_usd_compat(overview.total_value_usd, overview.total_net_kg),
        ..overview
    })
}

fn projection_months(
    conn: &DuckConnection,
    filter: &DuckFilter,
) -> Result<Vec<AnalyticsMonthRow>, String> {
    let sql = format!(
        "SELECT
            month,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF(declaration_number, '')) AS declarations_count,
            COALESCE(SUM(value_num), 0.0) AS total_value_usd,
            COALESCE(SUM(net_kg_num), 0.0) AS total_net_kg
         FROM records
         {} month IS NOT NULL AND month <> ''
         GROUP BY month
         ORDER BY month DESC
         LIMIT 48",
        filter.where_extra_sql()
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let total_value_usd = row.get::<_, Option<f64>>(3)?.unwrap_or(0.0);
            let total_net_kg = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
            Ok(AnalyticsMonthRow {
                month: row.get(0)?,
                rows: row.get::<_, i64>(1)?.max(0) as u64,
                declarations: row.get::<_, i64>(2)?.max(0) as u64,
                total_value_usd,
                total_net_kg,
                compatible_usd: projection_usd_compat(total_value_usd, total_net_kg),
                measures: AnalyticsMeasures::default(),
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
    let sql = format!(
        "SELECT
            {label_sql} AS label,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF(declaration_number, '')) AS declarations_count,
            COUNT(DISTINCT NULLIF(edrpou_label, '')) AS companies_count,
            COALESCE(SUM(value_num), 0.0) AS total_value_usd,
            COALESCE(SUM(net_kg_num), 0.0) AS total_net_kg,
            COALESCE(SUM(gross_kg_num), 0.0) AS total_gross_kg,
            COALESCE(SUM(quantity_num), 0.0) AS total_quantity
         FROM records
         {} {label_sql} IS NOT NULL AND {label_sql} <> ''
         GROUP BY {label_sql}
         ORDER BY total_value_usd DESC, total_net_kg DESC, rows_count DESC, label
         LIMIT {}",
        filter.where_extra_sql(),
        limit.clamp(1, 20_000)
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let label: String = row.get(0)?;
            let rows_count = row.get::<_, i64>(1)?.max(0) as u64;
            let total_value_usd: f64 = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
            let total_net_kg: f64 = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
            let total_gross_kg: f64 = row.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
            let total_quantity: f64 = row.get::<_, Option<f64>>(7)?.unwrap_or(0.0);
            let share_base = if overview.total_value_usd > 0.0 {
                overview.total_value_usd
            } else if overview.total_net_kg > 0.0 {
                overview.total_net_kg
            } else {
                overview.row_count as f64
            };
            let share_value = if overview.total_value_usd > 0.0 {
                total_value_usd
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
                total_value_usd,
                total_net_kg,
                total_gross_kg,
                total_quantity,
                share_percent: ratio(share_value * 100.0, share_base),
                avg_value_per_net_kg: ratio(total_value_usd, total_net_kg),
                compatible_usd: projection_usd_compat(total_value_usd, total_net_kg),
                measures: AnalyticsMeasures::default(),
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
    Ok(vec![
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::ValuePerNetKg,
            "CASE
                WHEN value_num IS NOT NULL
                    AND net_kg_num IS NOT NULL
                    AND net_kg_num > 0
                THEN value_num / net_kg_num
             END",
            "net_kg_num",
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::RfvUsdKg,
            "rfv_num",
            "net_kg_num",
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::RmvNetUsdKg,
            "rmv_net_num",
            "net_kg_num",
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
            "gross_kg_num",
        )?,
        projection_price_metric(
            conn,
            filter,
            PriceMetricKind::MinBaseUsdKg,
            "min_base_num",
            "net_kg_num",
        )?,
    ])
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
            // The projection is single-cohort USD by construction; per-cohort
            // detail is not computed on this path.
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
            let needle = sql_string(&text.to_ascii_lowercase());
            conditions.push(format!(
                "contains(lower(coalesce(description, '') || ' ' || coalesce(sender_label, '') ||
                 ' ' || coalesce(recipient_label, '') || ' ' || coalesce(product_code, '') || ' '
                 || coalesce(trademark_label, '')), {needle})"
            ));
        }
        let filters = &query.filters;
        push_eq_i64(&mut conditions, "year", &filters.year);
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

fn push_eq_i64(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if let Ok(parsed) = value.parse::<i64>() {
        conditions.push(format!("{field} = {parsed}"));
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

fn push_contains(conditions: &mut Vec<String>, field: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        conditions.push(format!(
            "contains(lower(coalesce({field}, '')), {})",
            sql_string(&value.to_ascii_lowercase())
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
        assert_eq!(
            projected.overview.total_value_usd,
            sqlite.overview.total_value_usd
        );
        assert_eq!(
            projected.overview.total_net_kg,
            sqlite.overview.total_net_kg
        );
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
        assert_eq!(
            projected_prices.price_sections[0].count,
            sqlite_prices.price_sections[0].count
        );
        assert_eq!(
            projected_prices.price_sections[0].median,
            sqlite_prices.price_sections[0].median
        );
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
