use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{ImportLogEntry, ImportLogWrite, ImportQuality};

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS import_log (
            id INTEGER PRIMARY KEY,
            file_name TEXT NOT NULL,
            total_rows INTEGER NOT NULL,
            imported INTEGER NOT NULL,
            duplicates INTEGER NOT NULL,
            seconds REAL NOT NULL,
            layout TEXT,
            header_row INTEGER,
            source_columns INTEGER,
            recognized_columns INTEGER,
            extra_columns INTEGER,
            non_empty_cells INTEGER,
            empty_cells INTEGER,
            warnings TEXT,
            imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )?;
    ensure_file_hash_column(conn)?;
    ensure_quality_columns(conn)
}

pub(crate) fn ensure_file_hash_column(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute("ALTER TABLE import_log ADD COLUMN file_hash TEXT", []);
    Ok(())
}

pub(crate) fn reset_file_hashes(conn: &Connection) -> rusqlite::Result<()> {
    ensure_file_hash_column(conn)?;
    conn.execute("UPDATE import_log SET file_hash = NULL", [])?;
    Ok(())
}

pub(crate) fn add(conn: &Connection, entry: ImportLogWrite<'_>) {
    let warnings = entry.quality.warnings_text();
    let _ = conn.execute(
        "INSERT INTO import_log (
            file_name, total_rows, imported, duplicates, seconds, file_hash,
            layout, header_row, source_columns, recognized_columns, extra_columns,
            non_empty_cells, empty_cells, warnings
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            entry.file_name,
            entry.total_rows as i64,
            entry.imported as i64,
            entry.duplicates as i64,
            entry.seconds,
            entry.file_hash,
            empty_to_null(&entry.quality.layout),
            entry.quality.header_row as i64,
            entry.quality.source_columns as i64,
            entry.quality.recognized_columns as i64,
            entry.quality.extra_columns as i64,
            entry.quality.non_empty_cells as i64,
            entry.quality.empty_cells as i64,
            empty_to_null(&warnings),
        ],
    );
}

pub(crate) fn find_by_hash(conn: &Connection, file_hash: &str) -> Option<String> {
    conn.query_row(
        "SELECT file_name FROM import_log WHERE file_hash = ?1 ORDER BY id DESC LIMIT 1",
        [file_hash],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub(crate) fn list(conn: &Connection, limit: u64) -> Vec<ImportLogEntry> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT
            file_name, total_rows, imported, duplicates, seconds, imported_at,
            COALESCE(layout, ''),
            COALESCE(header_row, 0),
            COALESCE(source_columns, 0),
            COALESCE(recognized_columns, 0),
            COALESCE(extra_columns, 0),
            COALESCE(non_empty_cells, 0),
            COALESCE(empty_cells, 0),
            COALESCE(warnings, '')
         FROM import_log ORDER BY id DESC LIMIT ?1",
    ) else {
        return Vec::new();
    };
    stmt.query_map([limit as i64], |row| {
        let quality = ImportQuality {
            layout: row.get(6)?,
            header_row: row.get::<_, i64>(7)?.max(0) as u64,
            source_columns: row.get::<_, i64>(8)?.max(0) as u64,
            recognized_columns: row.get::<_, i64>(9)?.max(0) as u64,
            extra_columns: row.get::<_, i64>(10)?.max(0) as u64,
            non_empty_cells: row.get::<_, i64>(11)?.max(0) as u64,
            empty_cells: row.get::<_, i64>(12)?.max(0) as u64,
            warnings: Vec::new(),
        }
        .with_warnings_text(row.get(13)?);
        Ok(ImportLogEntry {
            file_name: row.get(0)?,
            total_rows: row.get::<_, i64>(1)? as u64,
            imported: row.get::<_, i64>(2)? as u64,
            duplicates: row.get::<_, i64>(3)? as u64,
            seconds: row.get(4)?,
            imported_at: row.get(5)?,
            quality,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

fn ensure_quality_columns(conn: &Connection) -> rusqlite::Result<()> {
    for (name, ty) in [
        ("layout", "TEXT"),
        ("header_row", "INTEGER"),
        ("source_columns", "INTEGER"),
        ("recognized_columns", "INTEGER"),
        ("extra_columns", "INTEGER"),
        ("non_empty_cells", "INTEGER"),
        ("empty_cells", "INTEGER"),
        ("warnings", "TEXT"),
    ] {
        if !table_has_column(conn, "import_log", name)? {
            let sql = format!("ALTER TABLE import_log ADD COLUMN {name} {ty}");
            conn.execute(&sql, [])?;
        }
    }
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for col in cols {
        if col? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn empty_to_null(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}
