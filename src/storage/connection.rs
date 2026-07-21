use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

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

pub(crate) fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    initialize(&conn).map_err(|err| err.to_string())?;
    heal_oversized_wal(&conn, path);
    Ok(conn)
}

pub(crate) fn open_runtime(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    configure_runtime_pragmas(&conn).map_err(|err| err.to_string())?;
    register_scalar_functions(&conn).map_err(|err| err.to_string())?;
    register_aggregate_functions(&conn).map_err(|err| err.to_string())?;
    Ok(conn)
}

fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    configure_pragmas(conn)?;
    register_scalar_functions(conn)?;
    register_aggregate_functions(conn)?;
    migrations::ensure_schema(conn)?;
    Ok(())
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
    let mut wal_path = path.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal_bytes = std::fs::metadata(std::path::Path::new(&wal_path))
        .map(|meta| meta.len())
        .unwrap_or(0);
    if wal_bytes > WAL_HEAL_THRESHOLD_BYTES {
        let _ = maintenance::checkpoint_wal_truncate(conn);
    }
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
