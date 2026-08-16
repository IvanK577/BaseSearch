use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::db_types::StartupPhase;
use crate::storage::extra::{extra_value_for_header, parse_extra};
use crate::storage::maintenance;
use crate::storage::migrations;
use crate::storage::normalize::{
    clean_label_value, month_key, normalize_country_key, normalize_text_key, parse_number,
    parse_number_grouped,
};
use crate::storage::search_text::contains_ci;

/// Upper bound for the WAL file after a successful checkpoint. SQLite recycles
/// WAL frames on its own, but without this limit the WAL FILE never shrinks:
/// one multi-gigabyte import leaves a multi-gigabyte -wal sitting on disk
/// forever. With the limit set, every completed checkpoint truncates the file
/// back to this size.
const WAL_SIZE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

/// A WAL this much over the limit at open time is healed with a truncating
/// checkpoint before the connection is handed out.
const WAL_HEAL_THRESHOLD_BYTES: u64 = 4 * WAL_SIZE_LIMIT_BYTES;

const GIBIBYTE: u64 = 1024 * 1024 * 1024;
const MIN_UPGRADE_HEADROOM_BYTES: u64 = GIBIBYTE;

pub(crate) fn open(path: &Path) -> Result<Connection, String> {
    open_with_progress(path, &mut |_| {})
}

/// Opens a database, reporting each phase of a first-open upgrade.
///
/// The callback runs on the calling thread, before the connection is returned,
/// so a UI has to be driven from somewhere else — the desktop opens the
/// database on a worker thread and forwards these into its startup screen.
pub(crate) fn open_with_progress(
    path: &Path,
    progress: &mut dyn FnMut(StartupPhase),
) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let database_existed = std::fs::metadata(path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    initialize(&conn, path, database_existed, progress)?;
    let heal_started = std::time::Instant::now();
    heal_oversized_wal(&conn, path);
    if heal_started.elapsed().as_secs() >= 1 {
        eprintln!(
            "[base-search] startup: reclaiming write-ahead log space took {:.1}s",
            heal_started.elapsed().as_secs_f64()
        );
    }
    Ok(conn)
}

pub(crate) fn open_runtime(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    configure_runtime_pragmas(&conn).map_err(|err| err.to_string())?;
    register_scalar_functions(&conn).map_err(|err| err.to_string())?;
    register_aggregate_functions(&conn).map_err(|err| err.to_string())?;
    Ok(conn)
}

fn initialize(
    conn: &Connection,
    path: &Path,
    database_existed: bool,
    progress: &mut dyn FnMut(StartupPhase),
) -> Result<(), String> {
    // Startup phases on a large database can take real time; report any phase
    // that exceeds one second so slow starts are diagnosable from the log.
    let phase_started = std::cell::Cell::new(std::time::Instant::now());
    let report = |name: &str| {
        let elapsed = phase_started.get().elapsed();
        if elapsed.as_secs() >= 1 {
            eprintln!(
                "[base-search] startup: {name} took {:.1}s",
                elapsed.as_secs_f64()
            );
        }
        phase_started.set(std::time::Instant::now());
    };
    configure_pragmas(conn).map_err(|err| err.to_string())?;
    report("configuring the database connection");
    register_scalar_functions(conn).map_err(|err| err.to_string())?;
    register_aggregate_functions(conn).map_err(|err| err.to_string())?;
    report("registering search functions");

    let recorded_backup = if database_existed {
        recorded_pre_upgrade_backup(conn)?
    } else {
        None
    };
    progress(StartupPhase::CheckingVersion);
    let upgrade_required = database_existed
        && migrations::destructive_upgrade_required(conn).map_err(|err| err.to_string())?;
    report("checking the schema version");
    let backup = if upgrade_required || recorded_backup.is_some() {
        Some(match recorded_backup {
            Some(backup) => reuse_pre_upgrade_backup(path, backup, progress)?,
            None => {
                let backup = create_pre_upgrade_backup(conn, path, progress)?;
                record_pre_upgrade_backup(conn, &backup)?;
                backup
            }
        })
    } else {
        None
    };
    if backup.is_some() {
        // Only a real upgrade announces itself. `ensure_schema` also runs on
        // every ordinary open — it is idempotent DDL — and saying "upgrading"
        // there would put "do not close the window" in front of a person every
        // single start.
        progress(StartupPhase::UpgradingStructure);
        eprintln!("[base-search] One-time database upgrade: applying schema changes.");
    }
    phase_started.set(std::time::Instant::now());
    if let Err(error) = migrations::ensure_schema(conn, progress) {
        return Err(match backup {
            Some(backup) => format!(
                "Database upgrade failed: {error}. The pre-upgrade backup is at {}",
                backup.display()
            ),
            None => error.to_string(),
        });
    }
    report("ensuring tables and indexes");
    if let Some(backup) = backup {
        progress(StartupPhase::VerifyingUpgrade);
        eprintln!("[base-search] One-time database upgrade: verifying the upgraded database.");
        sqlite_integrity_check(conn).map_err(|error| {
            format!(
                "Database upgrade finished with an integrity error: {error}. Restore the pre-upgrade backup at {} before retrying",
                backup.display()
            )
        })?;
        clear_recorded_pre_upgrade_backup(conn).map_err(|error| {
            format!(
                "Database upgrade and integrity verification succeeded, but its recovery marker could not be cleared: {error}. The verified backup remains at {}",
                backup.display()
            )
        })?;
        eprintln!(
            "[base-search] One-time database upgrade: complete. Verified backup retained at {}.",
            backup.display()
        );
    }
    Ok(())
}

fn upgrade_backup_meta_key() -> String {
    format!(
        "records_pre_upgrade_backup_v{}",
        migrations::RECORDS_SCHEMA_VERSION
    )
}

fn recorded_pre_upgrade_backup(conn: &Connection) -> Result<Option<PathBuf>, String> {
    let meta_exists = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'meta'
             )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        != 0;
    if !meta_exists {
        return Ok(None);
    }
    let key = upgrade_backup_meta_key();
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [&key], |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .map(|path| path.map(PathBuf::from))
    .map_err(|error| error.to_string())
}

fn record_pre_upgrade_backup(conn: &Connection, backup: &Path) -> Result<(), String> {
    let backup = std::fs::canonicalize(backup).unwrap_or_else(|_| backup.to_path_buf());
    let backup_text = backup.to_str().ok_or_else(|| {
        format!(
            "Verified backup path cannot be recorded as Unicode: {}",
            backup.display()
        )
    })?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
             key TEXT PRIMARY KEY,
             value TEXT
         );",
    )
    .map_err(|error| {
        format!(
            "Verified backup {} was created, but the upgrade recovery marker could not be initialized: {error}. The schema upgrade was not started",
            backup.display()
        )
    })?;
    let key = upgrade_backup_meta_key();
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key.as_str(), backup_text],
    )
    .map(|_| ())
    .map_err(|error| {
        format!(
            "Verified backup {} was created, but its upgrade recovery marker could not be saved: {error}. The schema upgrade was not started",
            backup.display()
        )
    })
}

fn clear_recorded_pre_upgrade_backup(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "DELETE FROM meta WHERE key = ?1",
        [upgrade_backup_meta_key()],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn reuse_pre_upgrade_backup(
    database: &Path,
    backup: PathBuf,
    progress: &mut dyn FnMut(StartupPhase),
) -> Result<PathBuf, String> {
    progress(StartupPhase::VerifyingBackup);
    if !backup_path_belongs_to_database(database, &backup) || !backup.is_file() {
        return Err(format!(
            "A previous one-time database upgrade is incomplete, but its recorded backup is missing or does not belong beside this database: {}. Refusing to continue; the database was not modified",
            backup.display()
        ));
    }
    eprintln!(
        "[base-search] One-time database upgrade: verifying the retained backup before resuming at {}.",
        backup.display()
    );
    verify_backup(&backup).map_err(|error| {
        format!(
            "The retained pre-upgrade backup at {} failed verification: {error}. Refusing to continue; the database was not modified",
            backup.display()
        )
    })?;
    eprintln!(
        "[base-search] One-time database upgrade: reusing the verified backup at {}.",
        backup.display()
    );
    Ok(backup)
}

fn backup_path_belongs_to_database(database: &Path, backup: &Path) -> bool {
    let database_parent = database.parent().unwrap_or_else(|| Path::new("."));
    let backup_parent = backup.parent().unwrap_or_else(|| Path::new("."));
    let database_parent =
        std::fs::canonicalize(database_parent).unwrap_or_else(|_| database_parent.to_path_buf());
    let backup_parent =
        std::fs::canonicalize(backup_parent).unwrap_or_else(|_| backup_parent.to_path_buf());
    if database_parent != backup_parent {
        return false;
    }
    let Some(database_name) = database.file_name() else {
        return false;
    };
    let Some(backup_name) = backup.file_name() else {
        return false;
    };
    backup_name.to_string_lossy().starts_with(&format!(
        "{}.pre-upgrade-v{}-",
        database_name.to_string_lossy(),
        migrations::RECORDS_SCHEMA_VERSION
    )) && backup_name.to_string_lossy().ends_with(".bak")
}

fn create_pre_upgrade_backup(
    conn: &Connection,
    path: &Path,
    progress: &mut dyn FnMut(StartupPhase),
) -> Result<PathBuf, String> {
    create_pre_upgrade_backup_with_available_space(conn, path, progress, |directory| {
        fs2::available_space(directory).map_err(|error| error.to_string())
    })
}

fn create_pre_upgrade_backup_with_available_space(
    conn: &Connection,
    path: &Path,
    progress: &mut dyn FnMut(StartupPhase),
    available_space: impl FnOnce(&Path) -> Result<u64, String>,
) -> Result<PathBuf, String> {
    progress(StartupPhase::CheckingFreeSpace);
    eprintln!(
        "[base-search] One-time database upgrade: checking free disk space for a verified backup."
    );
    let source_bytes = std::fs::metadata(path)
        .map_err(|error| {
            format!(
                "Could not inspect {} before schema upgrade: {error}. The original database was not modified",
                path.display()
            )
        })?
        .len();
    let wal_path = sqlite_sidecar_path(path, "-wal");
    let wal_bytes = match std::fs::metadata(&wal_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(format!(
                "Could not inspect the SQLite WAL at {} before schema upgrade: {error}. Refusing to estimate low; the original database was not modified",
                wal_path.display()
            ));
        }
    };
    let required_bytes = required_upgrade_space(source_bytes, wal_bytes);
    let backup_directory = path.parent().unwrap_or_else(|| Path::new("."));
    let available_bytes = available_space(backup_directory).map_err(|error| {
        format!(
            "Could not determine free space beside {} before schema upgrade: {error}. Refusing to create the backup; the original database was not modified",
            path.display()
        )
    })?;
    if available_bytes < required_bytes {
        return Err(format!(
            "Insufficient free space for the one-time database upgrade: {} required beside {}, but only {} available. Free disk space and retry; the original database was not modified",
            human_size(required_bytes),
            path.display(),
            human_size(available_bytes)
        ));
    }

    eprintln!(
        "[base-search] One-time database upgrade: checking the source database (quick check)."
    );
    sqlite_quick_check(conn).map_err(|error| {
        format!(
            "Database quick check failed before schema upgrade: {error}. The original database was not modified"
        )
    })?;
    let backup = next_backup_path(path)?;
    let backup_text = backup.to_str().ok_or_else(|| {
        format!(
            "Cannot create a schema-upgrade backup for a non-Unicode path: {}",
            backup.display()
        )
    })?;
    progress(StartupPhase::CreatingBackup);
    eprintln!(
        "[base-search] One-time database upgrade: creating backup at {} (source footprint {}, required free space {}, available {}).",
        backup.display(),
        human_size(source_bytes.saturating_add(wal_bytes)),
        human_size(required_bytes),
        human_size(available_bytes)
    );
    if let Err(error) = conn.execute("VACUUM main INTO ?1", [backup_text]) {
        let _ = std::fs::remove_file(&backup);
        return Err(format!(
            "Could not create the required pre-upgrade backup at {}: {error}. The original database was not modified",
            backup.display()
        ));
    }

    progress(StartupPhase::VerifyingBackup);
    eprintln!(
        "[base-search] One-time database upgrade: verifying the complete backup (full integrity check)."
    );
    if let Err(error) = verify_backup(&backup) {
        let _ = std::fs::remove_file(&backup);
        return Err(format!(
            "The pre-upgrade backup at {} failed verification: {error}. The original database was not modified",
            backup.display()
        ));
    }
    eprintln!(
        "[base-search] One-time database upgrade: verified backup ready at {}.",
        backup.display()
    );
    Ok(backup)
}

fn verify_backup(backup: &Path) -> Result<(), String> {
    let backup_conn = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    register_scalar_functions(&backup_conn).map_err(|error| error.to_string())?;
    register_aggregate_functions(&backup_conn).map_err(|error| error.to_string())?;
    sqlite_integrity_check(&backup_conn)
}

fn required_upgrade_space(database_bytes: u64, wal_bytes: u64) -> u64 {
    let source_footprint = database_bytes.saturating_add(wal_bytes);
    // One full footprint is reserved for the verified VACUUM INTO backup and
    // another for a worst-case table copy, WAL, and temporary migration pages.
    // The fixed margin covers SQLite metadata and small databases where a
    // percentage-only estimate would be misleadingly low.
    source_footprint
        .saturating_mul(2)
        .saturating_add(MIN_UPGRADE_HEADROOM_BYTES)
}

fn human_size(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIBIBYTE as f64)
}

fn next_backup_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Database path has no file name: {}", path.display()))?
        .to_string_lossy();
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    for suffix in 0..1000_u16 {
        let discriminator = if suffix == 0 {
            epoch.to_string()
        } else {
            format!("{epoch}-{suffix}")
        };
        let candidate = parent.join(format!(
            "{file_name}.pre-upgrade-v{}-{discriminator}.bak",
            migrations::RECORDS_SCHEMA_VERSION
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not choose a unique pre-upgrade backup name beside {}",
        path.display()
    ))
}

fn sqlite_quick_check(conn: &Connection) -> Result<(), String> {
    sqlite_check(conn, "PRAGMA quick_check")
}

fn sqlite_integrity_check(conn: &Connection) -> Result<(), String> {
    sqlite_check(conn, "PRAGMA integrity_check")
}

fn sqlite_check(conn: &Connection, pragma: &str) -> Result<(), String> {
    let mut statement = conn.prepare(pragma).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    for row in rows {
        let message = row.map_err(|error| error.to_string())?;
        if !message.eq_ignore_ascii_case("ok") {
            messages.push(message);
        }
    }
    if messages.is_empty() {
        Ok(())
    } else {
        Err(messages.join("; "))
    }
}

fn configure_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -131072)?;
    conn.pragma_update(None, "mmap_size", 268435456i64)?;
    conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT_BYTES as i64)?;
    Ok(())
}

fn configure_runtime_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -131072)?;
    conn.pragma_update(None, "mmap_size", 268435456i64)?;
    conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT_BYTES as i64)?;
    Ok(())
}

/// Best-effort recovery for databases that accumulated an oversized WAL under
/// previous versions: one cheap file-size check per open, and a truncating
/// checkpoint only when the WAL is far beyond the configured limit. Concurrent
/// readers can make the checkpoint partial; that is fine — the next open
/// retries.
fn heal_oversized_wal(conn: &Connection, path: &Path) {
    let wal_bytes = std::fs::metadata(sqlite_sidecar_path(path, "-wal"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    if wal_bytes > WAL_HEAL_THRESHOLD_BYTES {
        let _ = maintenance::checkpoint_wal_truncate(conn);
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn register_scalar_functions(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("cyr_contains", 2, flags, |ctx| {
        let hay = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        let needle = ctx
            .get_raw(1)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(match (hay, needle) {
            (Some(hay), Some(needle)) => contains_ci(hay, needle),
            _ => false,
        })
    })?;
    conn.create_scalar_function("num_value", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.and_then(parse_number))
    })?;
    conn.create_scalar_function("num_value_grouped", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.and_then(parse_number_grouped))
    })?;
    conn.create_scalar_function("country_key", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.map(normalize_country_key).unwrap_or_default())
    })?;
    conn.create_scalar_function("text_key", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.map(normalize_text_key).unwrap_or_default())
    })?;
    conn.create_scalar_function("label_value", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.map(clean_label_value).unwrap_or_default())
    })?;
    conn.create_scalar_function("month_key", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(raw.map(month_key).unwrap_or_default())
    })?;
    conn.create_scalar_function("extra_values_text", 1, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        let values = parse_extra(raw)
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>()
            .join(" ");
        Ok(values)
    })?;
    conn.create_scalar_function("extra_value", 2, flags, |ctx| {
        let raw = ctx
            .get_raw(0)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        let header = ctx
            .get_raw(1)
            .as_str_or_null()
            .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?;
        Ok(extra_value_for_header(raw, header))
    })?;
    Ok(())
}

fn register_aggregate_functions(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_aggregate_function("pctl_text", 1, flags, PercentilesAggregate)?;
    conn.create_aggregate_function("median_num", 1, flags, MedianAggregate)?;
    conn.create_scalar_function("pctl_num", 2, flags, |ctx| {
        let raw = ctx.get::<Option<String>>(0)?;
        let index = ctx.get::<i64>(1)?;
        Ok(raw.and_then(|raw| {
            usize::try_from(index)
                .ok()
                .and_then(|index| raw.split('|').nth(index))
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite())
        }))
    })?;
    Ok(())
}

struct PercentilesAggregate;

impl rusqlite::functions::Aggregate<Vec<f64>, Option<String>> for PercentilesAggregate {
    fn init(&self, _ctx: &mut rusqlite::functions::Context<'_>) -> rusqlite::Result<Vec<f64>> {
        Ok(Vec::new())
    }

    fn step(
        &self,
        ctx: &mut rusqlite::functions::Context<'_>,
        acc: &mut Vec<f64>,
    ) -> rusqlite::Result<()> {
        if let Some(value) = ctx.get::<Option<f64>>(0)?
            && value.is_finite()
        {
            acc.push(value);
        }
        Ok(())
    }

    fn finalize(
        &self,
        _ctx: &mut rusqlite::functions::Context<'_>,
        acc: Option<Vec<f64>>,
    ) -> rusqlite::Result<Option<String>> {
        let mut values = acc.unwrap_or_default();
        if values.is_empty() {
            return Ok(None);
        }
        values.sort_unstable_by(f64::total_cmp);
        Ok(Some(format!(
            "{}|{}|{}",
            continuous_percentile(&values, 0.25),
            continuous_percentile(&values, 0.5),
            continuous_percentile(&values, 0.75)
        )))
    }
}

struct MedianAggregate;

impl rusqlite::functions::Aggregate<Vec<f64>, Option<f64>> for MedianAggregate {
    fn init(&self, _ctx: &mut rusqlite::functions::Context<'_>) -> rusqlite::Result<Vec<f64>> {
        Ok(Vec::new())
    }

    fn step(
        &self,
        ctx: &mut rusqlite::functions::Context<'_>,
        acc: &mut Vec<f64>,
    ) -> rusqlite::Result<()> {
        if let Some(value) = ctx.get::<Option<f64>>(0)?
            && value.is_finite()
        {
            acc.push(value);
        }
        Ok(())
    }

    fn finalize(
        &self,
        _ctx: &mut rusqlite::functions::Context<'_>,
        acc: Option<Vec<f64>>,
    ) -> rusqlite::Result<Option<f64>> {
        let mut values = acc.unwrap_or_default();
        if values.is_empty() {
            return Ok(None);
        }
        values.sort_unstable_by(f64::total_cmp);
        Ok(Some(continuous_percentile(&values, 0.5)))
    }
}

/// R-7 continuous percentile, matching DuckDB's `quantile_cont` behavior.
fn continuous_percentile(sorted: &[f64], p: f64) -> f64 {
    let position = (sorted.len() - 1) as f64 * p.clamp(0.0, 1.0);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, OpenFlags};

    use super::{
        create_pre_upgrade_backup_with_available_space, open, register_aggregate_functions,
        register_scalar_functions, required_upgrade_space,
    };

    const GIBIBYTE: u64 = 1024 * 1024 * 1024;

    #[test]
    fn upgrade_space_preflight_includes_wal_and_conservative_headroom() {
        assert_eq!(
            required_upgrade_space(20 * GIBIBYTE, 2 * GIBIBYTE),
            45 * GIBIBYTE,
            "a 22 GiB source footprint needs a full backup, a full migration working copy, and 1 GiB headroom"
        );
        assert_eq!(
            required_upgrade_space(128 * 1024 * 1024, 0),
            1280 * 1024 * 1024,
            "small databases still reserve a second working copy plus 1 GiB headroom"
        );
        assert_eq!(
            required_upgrade_space(u64::MAX, u64::MAX),
            u64::MAX,
            "space estimates must saturate instead of wrapping into a false low requirement"
        );
    }

    #[test]
    fn insufficient_space_refuses_backup_without_modifying_the_source() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("base_search.db");
        let connection = open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO records(row_hash, source_file, description)
                 VALUES(zeroblob(16), 'legacy.xlsx', 'must remain in place')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE meta SET value = '4' WHERE key = 'records_schema'",
                [],
            )
            .unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();

        let error = create_pre_upgrade_backup_with_available_space(
            &connection,
            &database,
            &mut |_| {},
            |_| Ok(1024),
        )
        .unwrap_err();

        assert!(
            error.contains("Insufficient free space")
                && error.contains("required")
                && error.contains("available")
                && error.contains("was not modified"),
            "unexpected space-preflight error: {error}"
        );
        assert!(
            pre_upgrade_backups(&database).is_empty(),
            "space refusal must happen before a backup file is created"
        );
        assert_eq!(stored_description(&connection), "must remain in place");
        assert_eq!(stored_schema_version(&connection), "4");
    }

    #[test]
    fn failed_destructive_upgrade_keeps_a_valid_pre_upgrade_backup_and_original_rows() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("base_search.db");
        let connection = open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO records(row_hash, source_file, description)
                 VALUES(zeroblob(16), 'legacy.xlsx', 'preserve before migration')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE meta SET value = '4' WHERE key = 'records_schema'",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_upgrade
                 BEFORE UPDATE ON records
                 BEGIN
                     SELECT RAISE(ABORT, 'forced upgrade failure');
                 END;",
            )
            .unwrap();
        drop(connection);

        let error = match open(&database) {
            Ok(_) => panic!("the forced migration failure unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(
            error.contains("backup") && error.contains("forced upgrade failure"),
            "unexpected upgrade error: {error}"
        );

        let backups = pre_upgrade_backups(&database);
        assert_eq!(backups.len(), 1, "expected one pre-upgrade backup");
        let backup =
            Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        register_scalar_functions(&backup).unwrap();
        register_aggregate_functions(&backup).unwrap();
        let integrity: String = backup
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(stored_description(&backup), "preserve before migration");
        assert_eq!(stored_schema_version(&backup), "4");

        let original = Connection::open(&database).unwrap();
        assert_eq!(stored_description(&original), "preserve before migration");
        assert_eq!(stored_schema_version(&original), "4");
        original
            .execute_batch("DROP TRIGGER fail_upgrade;")
            .unwrap();
        drop(original);

        let upgraded = open(&database).unwrap();
        assert_eq!(stored_description(&upgraded), "preserve before migration");
        assert_eq!(
            stored_schema_version(&upgraded),
            crate::storage::migrations::RECORDS_SCHEMA_VERSION
        );
        assert_eq!(
            pre_upgrade_backups(&database),
            backups,
            "retry must reuse the verified backup instead of consuming another full copy"
        );
    }

    fn pre_upgrade_backups(database: &Path) -> Vec<PathBuf> {
        let file_name = database.file_name().unwrap().to_string_lossy();
        let prefix = format!("{file_name}.pre-upgrade-");
        let mut backups = std::fs::read_dir(database.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| {
                        let name = name.to_string_lossy();
                        name.starts_with(&prefix) && name.ends_with(".bak")
                    })
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        backups.sort();
        backups
    }

    fn stored_description(connection: &Connection) -> String {
        connection
            .query_row("SELECT description FROM records WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn stored_schema_version(connection: &Connection) -> String {
        connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'records_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}
