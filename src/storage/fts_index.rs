use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::db::{FtsRepairError, FtsRepairIssue, FtsRepairReport};
use crate::storage::{effective_rows, meta, search_text};

pub(crate) const SCHEMA_VERSION: &str = "7";

const INDEX_TABLE: &str = "records_fts";
const REBUILD_TABLE: &str = "records_fts_rebuild";
const CHUNK_ROWS: i64 = 20_000;

pub(crate) fn create_table_sql(table: &str) -> String {
    debug_assert!(
        table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING fts5(
            search_text,
            content='',
            detail=none,
            columnsize=0,
            tokenize='unicode61 remove_diacritics 2'
        );"
    )
}

pub(crate) fn index(
    conn: &mut Connection,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> rusqlite::Result<(u64, bool)> {
    if meta::get(conn, "fts_schema").as_deref() != Some(SCHEMA_VERSION) {
        let report = repair(conn, cancel, &mut progress).map_err(|error| match error {
            FtsRepairError::Database(error) => error,
            error => rusqlite::Error::UserFunctionError(Box::new(error)),
        })?;
        return Ok((report.indexed_rows, report.cancelled));
    }
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
        row.get(0)
    })?;
    let start = meta::get_i64(conn, "fts_watermark");
    if start >= max_id {
        return Ok((0, false));
    }
    let span_total = (max_id - start) as u64;
    let insert_sql = format!(
        "INSERT INTO records_fts(rowid, search_text)
         SELECT id, {} FROM records
         WHERE id > ?1 AND id <= ?2 AND {}",
        search_text::search_text_expr(),
        effective_rows::searchable_payload_clause("records")
    );
    let mut watermark = start;
    let mut indexed: u64 = 0;
    while watermark < max_id {
        if cancel.load(Ordering::Relaxed) {
            return Ok((indexed, true));
        }
        let end = (watermark + CHUNK_ROWS).min(max_id);
        let tx = conn.transaction()?;
        let n = tx.execute(&insert_sql, params![watermark, end])?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('fts_watermark', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![end.to_string()],
        )?;
        tx.commit()?;
        indexed += n as u64;
        watermark = end;
        progress((watermark - start) as u64, span_total);
    }
    Ok((indexed, false))
}

pub(crate) fn repair(
    conn: &mut Connection,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<FtsRepairReport, FtsRepairError> {
    let prior_watermark = meta::get_i64(conn, "fts_watermark");
    let mut issues = initial_issues(conn, prior_watermark);
    drop_rebuild_table(conn)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(cancelled_report(0, prior_watermark, issues));
    }

    conn.execute_batch(&create_table_sql(REBUILD_TABLE))?;

    let source_version = data_version(conn)?;
    let target_max: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
            row.get(0)
        })?;
    let target_rows: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM records
             WHERE id <= ?1 AND {}",
            effective_rows::searchable_payload_clause("records")
        ),
        [target_max],
        |row| row.get(0),
    )?;

    let build_result =
        build_replacement(conn, target_max, target_rows as u64, cancel, &mut progress);
    let indexed_rows = match build_result {
        Ok(BuildOutcome::Complete(rows)) => rows,
        Ok(BuildOutcome::Cancelled(rows)) => {
            drop_rebuild_table(conn)?;
            return Ok(cancelled_report(rows, prior_watermark, issues));
        }
        Err(error) => {
            let _ = drop_rebuild_table(conn);
            return Err(error.into());
        }
    };

    if indexed_rows != target_rows as u64 {
        let _ = drop_rebuild_table(conn);
        return Err(FtsRepairError::Validation(format!(
            "FTS rebuild indexed {indexed_rows} rows, expected {target_rows}"
        )));
    }
    if cancel.load(Ordering::Relaxed) {
        drop_rebuild_table(conn)?;
        return Ok(cancelled_report(indexed_rows, prior_watermark, issues));
    }
    if data_version(conn)? != source_version {
        drop_rebuild_table(conn)?;
        return Err(FtsRepairError::SourceChanged);
    }

    integrity_check(conn, REBUILD_TABLE)?;
    validate_unicode_tokenizer(conn)?;
    assess_live_contents(conn, &mut issues);

    if cancel.load(Ordering::Relaxed) {
        drop_rebuild_table(conn)?;
        return Ok(cancelled_report(indexed_rows, prior_watermark, issues));
    }
    if data_version(conn)? != source_version {
        drop_rebuild_table(conn)?;
        return Err(FtsRepairError::SourceChanged);
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let swap_result = (|| -> Result<(), FtsRepairError> {
        if data_version(conn)? != source_version {
            return Err(FtsRepairError::SourceChanged);
        }
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {INDEX_TABLE};
             ALTER TABLE {REBUILD_TABLE} RENAME TO {INDEX_TABLE};"
        ))?;
        integrity_check(conn, INDEX_TABLE)?;
        set_meta_checked(conn, "fts_watermark", &target_max.to_string())?;
        set_meta_checked(conn, "fts_schema", SCHEMA_VERSION)?;
        set_meta_checked(conn, "fts_indexed_rows", &indexed_rows.to_string())?;
        let completed_at: String =
            conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                row.get(0)
            })?;
        set_meta_checked(conn, "fts_last_rebuilt_at", &completed_at)?;
        Ok(())
    })();

    match swap_result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            let _ = drop_rebuild_table(conn);
            return Err(error);
        }
    }

    Ok(FtsRepairReport {
        rebuilt: true,
        cancelled: false,
        indexed_rows,
        watermark: target_max,
        schema_version: SCHEMA_VERSION.to_string(),
        issues,
    })
}

pub(crate) fn unindexed_rows(conn: &Connection) -> u64 {
    if meta::get(conn, "fts_schema").as_deref() != Some(SCHEMA_VERSION) {
        return conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM records WHERE {}",
                    effective_rows::searchable_payload_clause("records")
                ),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;
    }
    let watermark = meta::get_i64(conn, "fts_watermark");
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM records
             WHERE id > ?1 AND {}",
            effective_rows::searchable_payload_clause("records")
        ),
        [watermark],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0) as u64
}

enum BuildOutcome {
    Complete(u64),
    Cancelled(u64),
}

fn build_replacement(
    conn: &mut Connection,
    target_max: i64,
    target_rows: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64, u64),
) -> rusqlite::Result<BuildOutcome> {
    let insert_sql = format!(
        "INSERT INTO {REBUILD_TABLE}(rowid, search_text)
         SELECT id, {} FROM records
         WHERE id > ?1 AND id <= ?2
           AND {}
         ORDER BY id",
        search_text::search_text_expr(),
        effective_rows::searchable_payload_clause("records")
    );
    let mut cursor = 0_i64;
    let mut indexed = 0_u64;
    while cursor < target_max {
        if cancel.load(Ordering::Relaxed) {
            return Ok(BuildOutcome::Cancelled(indexed));
        }
        let next_cursor: Option<i64> = conn.query_row(
            &format!(
                "SELECT MAX(id) FROM (
                 SELECT id FROM records
                 WHERE id > ?1 AND id <= ?2
                   AND {}
                 ORDER BY id LIMIT ?3
             )",
                effective_rows::searchable_payload_clause("records")
            ),
            params![cursor, target_max, CHUNK_ROWS],
            |row| row.get(0),
        )?;
        let Some(next_cursor) = next_cursor else {
            break;
        };
        let tx = conn.transaction()?;
        let inserted = tx.execute(&insert_sql, params![cursor, next_cursor])?;
        tx.commit()?;
        indexed += inserted as u64;
        cursor = next_cursor;
        progress(indexed, target_rows);
    }
    if cancel.load(Ordering::Relaxed) {
        Ok(BuildOutcome::Cancelled(indexed))
    } else {
        Ok(BuildOutcome::Complete(indexed))
    }
}

fn initial_issues(conn: &Connection, watermark: i64) -> Vec<FtsRepairIssue> {
    let mut issues = Vec::new();
    let max_id = conn
        .query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);
    if watermark != max_id {
        issues.push(FtsRepairIssue::WatermarkMismatch);
    }
    if meta::get(conn, "fts_schema").as_deref() != Some(SCHEMA_VERSION) {
        issues.push(FtsRepairIssue::VersionStale);
    }
    if !table_exists(conn, INDEX_TABLE).unwrap_or(false) {
        issues.push(FtsRepairIssue::MissingIndex);
    }
    issues
}

fn assess_live_contents(conn: &Connection, issues: &mut Vec<FtsRepairIssue>) {
    if issues.contains(&FtsRepairIssue::MissingIndex) {
        return;
    }
    if integrity_check(conn, INDEX_TABLE).is_err() {
        push_issue(issues, FtsRepairIssue::IntegrityCheckFailed);
        return;
    }
    match vocabularies_match(conn) {
        Ok(true) => {}
        Ok(false) => push_issue(issues, FtsRepairIssue::ContentMismatch),
        Err(_) => push_issue(issues, FtsRepairIssue::IntegrityCheckFailed),
    }
}

fn vocabularies_match(conn: &Connection) -> rusqlite::Result<bool> {
    const LIVE_VOCAB: &str = "base_search_live_fts_vocab";
    const REBUILD_VOCAB: &str = "base_search_rebuild_fts_vocab";
    let live = vocabulary_fingerprint(conn, INDEX_TABLE, LIVE_VOCAB)?;
    let replacement = vocabulary_fingerprint(conn, REBUILD_TABLE, REBUILD_VOCAB)?;
    Ok(live == replacement)
}

fn vocabulary_fingerprint(
    conn: &Connection,
    index_table: &str,
    vocab_table: &str,
) -> rusqlite::Result<[u8; 32]> {
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.{vocab_table};
         CREATE VIRTUAL TABLE temp.{vocab_table}
             USING fts5vocab(main, {index_table}, instance);"
    ))?;
    let fingerprint = (|| -> rusqlite::Result<[u8; 32]> {
        let mut statement = conn.prepare(&format!(
            "SELECT term, doc FROM temp.{vocab_table} ORDER BY term"
        ))?;
        let mut rows = statement.query([])?;
        let mut digest = Sha256::new();
        while let Some(row) = rows.next()? {
            let term: String = row.get(0)?;
            let document: i64 = row.get(1)?;
            digest.update((term.len() as u64).to_le_bytes());
            digest.update(term.as_bytes());
            digest.update(document.to_le_bytes());
        }
        Ok(digest.finalize().into())
    })();
    let cleanup = conn.execute_batch(&format!("DROP TABLE IF EXISTS temp.{vocab_table};"));
    match (fingerprint, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(fingerprint), Ok(())) => Ok(fingerprint),
    }
}

fn validate_unicode_tokenizer(conn: &Connection) -> Result<(), FtsRepairError> {
    const PROBE: &str = "base_search_fts_unicode_probe";
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.{PROBE};
         CREATE VIRTUAL TABLE temp.{PROBE} USING fts5(
             search_text,
             content='',
             detail=none,
             columnsize=0,
             tokenize='unicode61 remove_diacritics 2'
         );
         INSERT INTO {PROBE}(rowid, search_text) VALUES
             (1, 'Український гідравлічний насос'),
             (2, '東京精密機器'),
             (3, 'مضخة صناعية');"
    ))?;
    let result = (|| -> Result<(), FtsRepairError> {
        for (rowid, term) in [
            (1_i64, "ГІДРАВЛІЧНИЙ"),
            (2_i64, "東京精密機器"),
            (3_i64, "صناعية"),
        ] {
            let found: i64 = conn.query_row(
                &format!(
                    "SELECT EXISTS(
                         SELECT 1 FROM {PROBE}
                         WHERE {PROBE} MATCH ?1 AND rowid = ?2
                     )"
                ),
                params![term, rowid],
                |row| row.get(0),
            )?;
            if found == 0 {
                return Err(FtsRepairError::Validation(format!(
                    "FTS Unicode tokenizer validation failed for {term}"
                )));
            }
        }
        Ok(())
    })();
    let cleanup = conn.execute_batch(&format!("DROP TABLE IF EXISTS temp.{PROBE};"));
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn integrity_check(conn: &Connection, table: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("INSERT INTO {table}({table}) VALUES('integrity-check')"),
        [],
    )?;
    Ok(())
}

fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "data_version", |row| row.get(0))
}

fn set_meta_checked(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

fn drop_rebuild_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!("DROP TABLE IF EXISTS {REBUILD_TABLE};"))
}

fn cancelled_report(
    indexed_rows: u64,
    watermark: i64,
    issues: Vec<FtsRepairIssue>,
) -> FtsRepairReport {
    FtsRepairReport {
        rebuilt: false,
        cancelled: true,
        indexed_rows,
        watermark,
        schema_version: SCHEMA_VERSION.to_string(),
        issues,
    }
}

fn push_issue(issues: &mut Vec<FtsRepairIssue>, issue: FtsRepairIssue) {
    if !issues.contains(&issue) {
        issues.push(issue);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::{SCHEMA_VERSION, index, unindexed_rows};
    use crate::storage::{connection, meta};

    #[test]
    fn stale_schema_forces_a_complete_rebuild_even_when_the_watermark_is_current() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stale-schema.db");
        let mut conn = connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO records(row_hash, source_file, description)
             VALUES(zeroblob(16), 'source.xlsx', 'obsoletetoken')",
            [],
        )
        .unwrap();
        meta::set(&conn, "fts_schema", SCHEMA_VERSION);
        assert_eq!(
            index(&mut conn, &AtomicBool::new(false), |_, _| {}).unwrap(),
            (1, false)
        );

        conn.execute(
            "UPDATE records SET description = 'replacementtoken' WHERE id = 1",
            [],
        )
        .unwrap();
        meta::set(&conn, "fts_schema", "legacy-version");

        assert_eq!(
            unindexed_rows(&conn),
            1,
            "startup must notice a stale schema even with a current watermark"
        );
        assert_eq!(
            index(&mut conn, &AtomicBool::new(false), |_, _| {}).unwrap(),
            (1, false)
        );
        assert_eq!(
            meta::get(&conn, "fts_schema").as_deref(),
            Some(SCHEMA_VERSION)
        );
        assert_eq!(fts_matches(&conn, "replacementtoken"), 1);
        assert_eq!(fts_matches(&conn, "obsoletetoken"), 0);
    }

    fn fts_matches(connection: &rusqlite::Connection, term: &str) -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH ?1",
                [term],
                |row| row.get(0),
            )
            .unwrap()
    }
}
