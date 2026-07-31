use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::db::{FtsRepairError, FtsRepairIssue, FtsRepairReport};
use crate::storage::{effective_rows, meta, search_text};

/// Bumped to 8 when `SEARCH_COLUMNS` gained the identifying columns (EDRPOU,
/// contract, delivery place, customs office, ZED purpose). An existing database
/// rebuilds its index once on the next open so those columns become
/// searchable; the rebuild reports progress and resumes if it is interrupted.
pub(crate) const SCHEMA_VERSION: &str = "8";

const INDEX_TABLE: &str = "records_fts";
const REBUILD_TABLE: &str = "records_fts_rebuild";
const CHUNK_ROWS: i64 = 20_000;

// Crash-resume state for a long index rebuild. The guard fingerprints the
// data (max row id + import count); any import invalidates the partial
// rebuild, which then starts fresh.
const REBUILD_GUARD_KEY: &str = "fts_rebuild_guard";
const REBUILD_TARGET_MAX_KEY: &str = "fts_rebuild_target_max";
const REBUILD_TARGET_ROWS_KEY: &str = "fts_rebuild_target_rows";
const REBUILD_CURSOR_KEY: &str = "fts_rebuild_cursor";
const REBUILD_INDEXED_KEY: &str = "fts_rebuild_indexed";

fn rebuild_guard(conn: &Connection) -> rusqlite::Result<String> {
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
        row.get(0)
    })?;
    let imports: i64 = conn
        .query_row("SELECT COUNT(*) FROM import_log", [], |row| row.get(0))
        .unwrap_or(0);
    Ok(format!("v{SCHEMA_VERSION}:{max_id}:{imports}"))
}

fn clear_rebuild_meta(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM meta WHERE key IN (?1, ?2, ?3, ?4, ?5)",
        params![
            REBUILD_GUARD_KEY,
            REBUILD_TARGET_MAX_KEY,
            REBUILD_TARGET_ROWS_KEY,
            REBUILD_CURSOR_KEY,
            REBUILD_INDEXED_KEY
        ],
    )?;
    Ok(())
}

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
    if cancel.load(Ordering::Relaxed) {
        return Ok(cancelled_report(0, prior_watermark, issues));
    }

    // A rebuild of a multi-million-row index takes long enough that the
    // process may be closed before it finishes. Chunk progress is committed
    // together with a cursor, and an interrupted rebuild resumes from that
    // cursor as long as no import has changed the data since it started.
    let guard = rebuild_guard(conn)?;
    let resume = table_exists(conn, REBUILD_TABLE)?
        && meta::get(conn, REBUILD_GUARD_KEY).as_deref() == Some(guard.as_str());
    let (target_max, target_rows, start_cursor, already_indexed) = if resume {
        (
            meta::get_i64(conn, REBUILD_TARGET_MAX_KEY),
            meta::get_i64(conn, REBUILD_TARGET_ROWS_KEY),
            meta::get_i64(conn, REBUILD_CURSOR_KEY),
            meta::get_i64(conn, REBUILD_INDEXED_KEY) as u64,
        )
    } else {
        drop_rebuild_table(conn)?;
        conn.execute_batch(&create_table_sql(REBUILD_TABLE))?;
        let target_max: i64 =
            conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
                row.get(0)
            })?;
        let target_rows = searchable_rows_between(conn, 0, Some(target_max))?;
        set_meta_checked(conn, REBUILD_GUARD_KEY, &guard)?;
        set_meta_checked(conn, REBUILD_TARGET_MAX_KEY, &target_max.to_string())?;
        set_meta_checked(conn, REBUILD_TARGET_ROWS_KEY, &target_rows.to_string())?;
        set_meta_checked(conn, REBUILD_CURSOR_KEY, "0")?;
        set_meta_checked(conn, REBUILD_INDEXED_KEY, "0")?;
        (target_max, target_rows, 0, 0)
    };

    let source_version = data_version(conn)?;
    let build_result = build_replacement(
        conn,
        target_max,
        target_rows as u64,
        start_cursor,
        already_indexed,
        cancel,
        &mut progress,
    );
    let indexed_rows = match build_result {
        Ok(BuildOutcome::Complete(rows)) => rows,
        Ok(BuildOutcome::Cancelled(rows)) => {
            // Keep the partial rebuild and its cursor for the next attempt.
            return Ok(cancelled_report(rows, prior_watermark, issues));
        }
        Err(error) => {
            let _ = drop_rebuild_table(conn);
            let _ = clear_rebuild_meta(conn);
            return Err(error.into());
        }
    };

    if indexed_rows != target_rows as u64 {
        let _ = drop_rebuild_table(conn);
        let _ = clear_rebuild_meta(conn);
        return Err(FtsRepairError::Validation(format!(
            "FTS rebuild indexed {indexed_rows} rows, expected {target_rows}"
        )));
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(cancelled_report(indexed_rows, prior_watermark, issues));
    }
    if data_version(conn)? != source_version {
        drop_rebuild_table(conn)?;
        let _ = clear_rebuild_meta(conn);
        return Err(FtsRepairError::SourceChanged);
    }

    integrity_check(conn, REBUILD_TABLE)?;
    validate_unicode_tokenizer(conn)?;
    assess_live_contents(conn, &mut issues);

    if cancel.load(Ordering::Relaxed) {
        return Ok(cancelled_report(indexed_rows, prior_watermark, issues));
    }
    if data_version(conn)? != source_version {
        drop_rebuild_table(conn)?;
        let _ = clear_rebuild_meta(conn);
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
        clear_rebuild_meta(conn)?;
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

/// Counts searchable rows in `(above, up_to]` without evaluating the
/// row-by-row `searchable_payload_clause` over the whole table. The clause is
/// `canonical_id IS NULL AND (dup_first_file IS NULL OR NOT EXISTS(owner))`,
/// which splits into two disjoint, index-friendly parts: the canonical scope
/// (a partial-index-only count) and the small set of legacy duplicates that
/// lack a canonical owner (an indexed probe per legacy row). On a
/// multi-million-row database the naive form takes minutes; this takes
/// seconds, and it is exact.
fn searchable_rows_between(
    conn: &Connection,
    above: i64,
    up_to: Option<i64>,
) -> rusqlite::Result<i64> {
    let ceiling = up_to.unwrap_or(i64::MAX);
    let canonical: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records
         WHERE dup_first_file IS NULL AND canonical_id IS NULL
           AND id > ?1 AND id <= ?2",
        params![above, ceiling],
        |row| row.get(0),
    )?;
    let orphan_legacy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records searchable
         WHERE searchable.dup_first_file IS NOT NULL
           AND searchable.canonical_id IS NULL
           AND searchable.id > ?1 AND searchable.id <= ?2
           AND NOT EXISTS (
               SELECT 1
               FROM records searchable_owner
               WHERE searchable_owner.row_hash = searchable.row_hash
                 AND searchable_owner.schema_id IS searchable.schema_id
                 AND searchable_owner.dup_first_file IS NULL
                 AND searchable_owner.canonical_id IS NULL
           )",
        params![above, ceiling],
        |row| row.get(0),
    )?;
    Ok(canonical + orphan_legacy)
}

pub(crate) fn unindexed_rows(conn: &Connection) -> u64 {
    if meta::get(conn, "fts_schema").as_deref() != Some(SCHEMA_VERSION) {
        return searchable_rows_between(conn, 0, None).unwrap_or(0) as u64;
    }
    let watermark = meta::get_i64(conn, "fts_watermark");
    searchable_rows_between(conn, watermark, None).unwrap_or(0) as u64
}

enum BuildOutcome {
    Complete(u64),
    Cancelled(u64),
}

fn build_replacement(
    conn: &mut Connection,
    target_max: i64,
    target_rows: u64,
    start_cursor: i64,
    already_indexed: u64,
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
    let mut cursor = start_cursor;
    let mut indexed = already_indexed;
    while cursor < target_max {
        if cancel.load(Ordering::Relaxed) {
            return Ok(BuildOutcome::Cancelled(indexed));
        }
        let next_cursor = (cursor + CHUNK_ROWS).min(target_max);
        let tx = conn.transaction()?;
        let inserted = tx.execute(&insert_sql, params![cursor, next_cursor])?;
        let chunk_indexed = indexed + inserted as u64;
        // The cursor and counter commit atomically with the chunk so an
        // interrupted rebuild resumes exactly where it stopped.
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![REBUILD_CURSOR_KEY, next_cursor.to_string()],
        )?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![REBUILD_INDEXED_KEY, chunk_indexed.to_string()],
        )?;
        tx.commit()?;
        indexed = chunk_indexed;
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

    use super::{
        REBUILD_CURSOR_KEY, REBUILD_TABLE, SCHEMA_VERSION, index, repair, table_exists,
        unindexed_rows,
    };
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

    #[test]
    fn interrupted_rebuild_resumes_from_its_cursor_instead_of_starting_over() {
        const ROWS: i64 = 45_000; // more than one 20k-id chunk
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("resumable-rebuild.db");
        let mut conn = connection::open(&path).unwrap();
        conn.execute(
            "WITH RECURSIVE seq(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1
             )
             INSERT INTO records(row_hash, source_file, description)
             SELECT randomblob(16), 'source.xlsx', 'resumabletoken' || n FROM seq",
            [ROWS],
        )
        .unwrap();
        meta::set(&conn, "fts_schema", "legacy-version");

        // Cancel after the first committed chunk: progress must survive.
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let seen = cancel.clone();
        let report = repair(&mut conn, &cancel, &mut |_, _| {
            seen.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .unwrap();
        assert!(report.cancelled);
        assert!(report.indexed_rows > 0);
        assert!(report.indexed_rows < ROWS as u64);
        assert!(
            table_exists(&conn, REBUILD_TABLE).unwrap(),
            "a cancelled rebuild must keep its partial work for the next run"
        );
        let resumed_from = meta::get_i64(&conn, REBUILD_CURSOR_KEY);
        assert!(resumed_from > 0);

        // The next run resumes from the cursor and completes.
        let report = repair(&mut conn, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert!(report.rebuilt);
        assert_eq!(report.indexed_rows, ROWS as u64);
        assert!(!table_exists(&conn, REBUILD_TABLE).unwrap());
        assert_eq!(
            meta::get(&conn, REBUILD_CURSOR_KEY),
            None,
            "resume bookkeeping must be cleared after a successful swap"
        );
        assert_eq!(fts_matches(&conn, "resumabletoken1"), 1);
        assert_eq!(unindexed_rows(&conn), 0);

        // An import between attempts invalidates the partial rebuild.
        meta::set(&conn, "fts_schema", "legacy-version");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let seen = cancel.clone();
        let report = repair(&mut conn, &cancel, &mut |_, _| {
            seen.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .unwrap();
        assert!(report.cancelled);
        conn.execute(
            "INSERT INTO records(row_hash, source_file, description)
             VALUES(randomblob(16), 'later.xlsx', 'latertoken')",
            [],
        )
        .unwrap();
        let report = repair(&mut conn, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert!(report.rebuilt);
        assert_eq!(
            report.indexed_rows,
            ROWS as u64 + 1,
            "a data change must force a fresh, complete rebuild"
        );
        assert_eq!(fts_matches(&conn, "latertoken"), 1);
    }

    #[test]
    fn unindexed_count_matches_the_searchable_clause_with_legacy_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unindexed-decomposed.db");
        let mut conn = connection::open(&path).unwrap();
        // Canonical row + a V2 duplicate (canonical_id set, never searchable)
        // + a legacy duplicate WITH an owner (not searchable) + an orphan
        // legacy duplicate (searchable).
        conn.execute_batch(
            "INSERT INTO records(id, row_hash, source_file, description)
             VALUES(1, x'01', 'a.xlsx', 'alpha');
             INSERT INTO records(id, row_hash, source_file, canonical_id, description)
             VALUES(2, x'01', 'b.xlsx', 1, 'alpha');
             INSERT INTO records(id, row_hash, source_file, dup_first_file, description)
             VALUES(3, x'01', 'c.xlsx', 'a.xlsx', 'alpha');
             INSERT INTO records(id, row_hash, source_file, dup_first_file, description)
             VALUES(4, x'02', 'd.xlsx', 'gone.xlsx', 'beta');",
        )
        .unwrap();
        meta::set(&conn, "fts_schema", "legacy-version");

        let expected: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM records WHERE {}",
                    crate::storage::effective_rows::searchable_payload_clause("records")
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(expected, 2, "canonical row 1 + orphan legacy row 4");
        assert_eq!(unindexed_rows(&conn), expected as u64);

        // The watermark branch must agree as well.
        let indexed = index(&mut conn, &AtomicBool::new(false), |_, _| {}).unwrap();
        assert_eq!(indexed.0, 2);
        assert_eq!(unindexed_rows(&conn), 0);
    }
}
