use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, params};

use crate::schema::COLUMNS;
use crate::storage::derived;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatabaseStorageInfo {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub page_count: u64,
    pub page_size: u64,
    pub freelist_pages: u64,
    pub freelist_bytes: u64,
}

impl DatabaseStorageInfo {
    pub fn total_file_bytes(&self) -> u64 {
        self.database_bytes + self.wal_bytes + self.shm_bytes
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalCheckpointInfo {
    pub busy: u64,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateCompactionOptions {
    pub batch_rows: u64,
}

impl Default for DuplicateCompactionOptions {
    fn default() -> Self {
        Self { batch_rows: 10_000 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DuplicateCompactionProgress {
    pub rows_compacted: u64,
    pub batches: u64,
    pub estimated_payload_bytes_cleared: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DuplicateCompactionReport {
    pub rows_compacted: u64,
    pub batches: u64,
    pub estimated_payload_bytes_cleared: u64,
    pub reclaimed_bytes: u64,
    pub reclaimable_bytes_before: u64,
    pub reclaimable_bytes_after: u64,
    pub remaining_legacy_duplicates: u64,
    pub cancelled: bool,
    pub vacuum_performed: bool,
}

pub(crate) fn storage_info(
    conn: &Connection,
    db_path: &Path,
) -> rusqlite::Result<DatabaseStorageInfo> {
    let page_count = pragma_u64(conn, "page_count")?;
    let page_size = pragma_u64(conn, "page_size")?;
    let freelist_pages = pragma_u64(conn, "freelist_count")?;
    Ok(DatabaseStorageInfo {
        database_bytes: file_len(db_path),
        wal_bytes: file_len(&sidecar_path(db_path, "-wal")),
        shm_bytes: file_len(&sidecar_path(db_path, "-shm")),
        page_count,
        page_size,
        freelist_pages,
        freelist_bytes: freelist_pages.saturating_mul(page_size),
    })
}

pub(crate) fn checkpoint_wal_truncate(conn: &Connection) -> rusqlite::Result<WalCheckpointInfo> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok(WalCheckpointInfo {
            busy: row.get::<_, i64>(0)?.max(0) as u64,
            log_frames: row.get::<_, i64>(1)?.max(0) as u64,
            checkpointed_frames: row.get::<_, i64>(2)?.max(0) as u64,
        })
    })
}

pub(crate) fn vacuum_database(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("VACUUM;")?;
    checkpoint_wal_truncate(conn)?;
    Ok(())
}

pub(crate) fn compact_duplicate_payloads(
    conn: &mut Connection,
    cancel: &AtomicBool,
    options: DuplicateCompactionOptions,
    mut progress: impl FnMut(DuplicateCompactionProgress),
) -> rusqlite::Result<DuplicateCompactionReport> {
    let batch_rows = options.batch_rows.clamp(1, 50_000) as i64;
    let page_size = pragma_u64(conn, "page_size")?;
    let reclaimable_bytes_before = pragma_u64(conn, "freelist_count")?.saturating_mul(page_size);
    let payload_columns = std::iter::once("year")
        .chain(std::iter::once("extra"))
        .chain(COLUMNS.iter().map(|column| column.name))
        .chain(derived::DERIVED.iter().map(|column| column.name))
        .collect::<Vec<_>>();
    let payload_size = payload_columns
        .iter()
        .map(|column| format!("COALESCE(LENGTH(CAST(d.{column} AS BLOB)), 0)"))
        .collect::<Vec<_>>()
        .join(" + ");
    let clear_assignments = payload_columns
        .iter()
        .map(|column| format!("{column} = NULL"))
        .collect::<Vec<_>>()
        .join(", ");
    let candidate_sql = format!(
        "SELECT d.id, MIN(c.id) AS canonical_id, {payload_size} AS payload_bytes
         FROM records d
         JOIN records c
           ON c.row_hash = d.row_hash
          AND c.schema_id IS d.schema_id
          AND c.dup_first_file IS NULL
          AND c.canonical_id IS NULL
         WHERE d.dup_first_file IS NOT NULL
           AND d.canonical_id IS NULL
         GROUP BY d.id
         ORDER BY d.id
         LIMIT ?1"
    );
    let update_sql = format!(
        "UPDATE records
         SET canonical_id = ?1, {clear_assignments}
         WHERE id = ?2 AND canonical_id IS NULL AND dup_first_file IS NOT NULL"
    );

    let mut report = DuplicateCompactionReport {
        reclaimable_bytes_before,
        ..DuplicateCompactionReport::default()
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let batch_result = (|| -> rusqlite::Result<Vec<(i64, i64, u64)>> {
            let mut statement = conn.prepare(&candidate_sql)?;
            let rows = statement.query_map([batch_rows], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?;
            let candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            if candidates.is_empty() {
                return Ok(candidates);
            }
            let mut update = conn.prepare_cached(&update_sql)?;
            for (id, canonical_id, _) in &candidates {
                update.execute(params![canonical_id, id])?;
            }
            Ok(candidates)
        })();
        let candidates = match batch_result {
            Ok(candidates) => {
                conn.execute_batch("COMMIT")?;
                candidates
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        if candidates.is_empty() {
            break;
        }

        report.batches += 1;
        report.rows_compacted += candidates.len() as u64;
        report.estimated_payload_bytes_cleared = report
            .estimated_payload_bytes_cleared
            .saturating_add(candidates.iter().map(|candidate| candidate.2).sum::<u64>());
        progress(DuplicateCompactionProgress {
            rows_compacted: report.rows_compacted,
            batches: report.batches,
            estimated_payload_bytes_cleared: report.estimated_payload_bytes_cleared,
        });
    }

    report.remaining_legacy_duplicates = conn.query_row(
        "SELECT COUNT(*) FROM records
         WHERE dup_first_file IS NOT NULL AND canonical_id IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    report.reclaimable_bytes_after = pragma_u64(conn, "freelist_count")?.saturating_mul(page_size);
    report.reclaimed_bytes = report
        .reclaimable_bytes_after
        .saturating_sub(report.reclaimable_bytes_before);
    Ok(report)
}

fn pragma_u64(conn: &Connection, name: &str) -> rusqlite::Result<u64> {
    let sql = format!("PRAGMA {name}");
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map(|value| value.max(0) as u64)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
