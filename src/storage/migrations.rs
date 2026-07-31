use rusqlite::{Connection, params};

use crate::domain::table::SemanticField;
use crate::schema::{COLUMNS, column_for_semantic};
use crate::storage::{
    derived, fts_index, import_log, meta, records, source_mapping_profiles, source_schemas,
};

pub(crate) const RECORDS_SCHEMA_VERSION: &str = "7";
const RECORD_HASH_REBUILD_PENDING_KEY: &str = "records_hash_rebuild_pending_v1";

pub(crate) fn destructive_upgrade_required(conn: &Connection) -> rusqlite::Result<bool> {
    if !table_exists(conn, "records")? && !table_exists(conn, "records_v2")? {
        return Ok(false);
    }
    Ok(meta::get(conn, "records_schema").as_deref() != Some(RECORDS_SCHEMA_VERSION))
}

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    ensure_meta_schema(conn)?;
    ensure_fts_schema(conn)?;
    migrate_records_schema(conn)?;
    conn.execute_batch(&format!(
        "{records};
        {fts}
        CREATE INDEX IF NOT EXISTS idx_records_product_code ON records(product_code);
        CREATE INDEX IF NOT EXISTS idx_records_edrpou ON records(edrpou);
        CREATE INDEX IF NOT EXISTS idx_records_hash ON records(row_hash);",
        records = records_ddl(),
        fts = fts_index::create_table_sql("records_fts")
    ))?;
    ensure_search_indexes(conn)?;
    drop_superseded_indexes(conn)?;
    import_log::ensure_schema(conn)?;
    source_mapping_profiles::ensure_schema(conn)?;
    source_schemas::ensure_schema(conn)?;
    Ok(())
}

/// Indexes that only serve reads, dropped for the duration of a bulk load into
/// an empty database and rebuilt in one sorted pass afterwards.
///
/// Maintaining them row by row during a first import is the dominant write
/// cost, and building each one at the end is far cheaper than inserting into
/// it several million times. The hash indexes are NOT here: the importer's own
/// duplicate lookup needs them while the load is running.
///
/// Losing them to a crash is safe — `ensure_schema` recreates every index on
/// the next open, because they are all `CREATE INDEX IF NOT EXISTS`.
const READ_ONLY_INDEXES: [&str; 8] = [
    "idx_records_product_code",
    "idx_records_edrpou",
    "idx_records_hash",
    "idx_records_canonical_id",
    "idx_records_legacy_duplicates",
    "idx_records_canonical_scope_v2",
    "idx_records_year_scope",
    "idx_records_legacy_schema",
];

/// Drops the read-side indexes ahead of a bulk load. Derived-column indexes go
/// too: they are generated per derived column and equally pointless to
/// maintain row by row while nobody is querying.
pub(crate) fn drop_read_indexes(conn: &Connection) -> rusqlite::Result<()> {
    let mut ddl: Vec<String> = READ_ONLY_INDEXES
        .iter()
        .map(|name| format!("DROP INDEX IF EXISTS {name};"))
        .collect();
    for column in derived::DERIVED {
        ddl.push(format!(
            "DROP INDEX IF EXISTS idx_records_{name}_scope;",
            name = column.name
        ));
        ddl.push(format!(
            "DROP INDEX IF EXISTS idx_records_{source}_key_scope;",
            source = column.source
        ));
    }
    conn.execute_batch(&ddl.join("\n"))
}

/// Rebuilds everything [`drop_read_indexes`] removed.
pub(crate) fn create_read_indexes(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_records_product_code ON records(product_code);
         CREATE INDEX IF NOT EXISTS idx_records_edrpou ON records(edrpou);
         CREATE INDEX IF NOT EXISTS idx_records_hash ON records(row_hash);",
    )?;
    ensure_search_indexes(conn)
}

fn ensure_search_indexes(conn: &Connection) -> rusqlite::Result<()> {
    let mut ddl = vec![
        "CREATE INDEX IF NOT EXISTS idx_records_canonical_id
             ON records(id) WHERE dup_first_file IS NULL;"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_records_hash_owner
             ON records(row_hash, id)
             WHERE dup_first_file IS NULL AND canonical_id IS NULL;"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_records_legacy_duplicates
             ON records(id)
             WHERE dup_first_file IS NOT NULL AND canonical_id IS NULL;"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_records_canonical_scope_v2
             ON records(id) WHERE dup_first_file IS NULL AND canonical_id IS NULL;"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_records_year_scope
             ON records(year, dup_first_file);"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_records_schema_hash_owner
             ON records(schema_id, row_hash, id)
             WHERE canonical_id IS NULL;"
            .to_string(),
        // Answers `has_legacy_rows` from an index instead of scanning the whole
        // table. On a database built entirely by 2.0 imports no row has a NULL
        // schema_id, so the partial index stays empty and the check is O(1) —
        // and that check sits on every search page, every export, and the
        // post-import field refresh.
        "CREATE INDEX IF NOT EXISTS idx_records_legacy_schema
             ON records(id) WHERE schema_id IS NULL;"
            .to_string(),
    ];
    let value_source = column_for_semantic(SemanticField::Value);
    let company_code_source = column_for_semantic(SemanticField::CompanyCode);
    for column in derived::DERIVED {
        match column.derivation {
            derived::Derivation::Country => ddl.push(format!(
                "CREATE INDEX IF NOT EXISTS idx_records_{name}_scope
                 ON records({name}, dup_first_file);",
                name = column.name
            )),
            derived::Derivation::Number(_) if Some(column.source) == value_source => {
                ddl.push(format!(
                    "CREATE INDEX IF NOT EXISTS idx_records_{name}_scope
                     ON records({name}, dup_first_file);",
                    name = column.name
                ));
            }
            derived::Derivation::Label if Some(column.source) == company_code_source => {
                ddl.push(format!(
                    "CREATE INDEX IF NOT EXISTS idx_records_{source}_key_scope
                     ON records(text_key({source}), dup_first_file);",
                    source = column.source
                ));
            }
            derived::Derivation::Number(_)
            | derived::Derivation::Label
            | derived::Derivation::Month => {}
        }
    }
    conn.execute_batch(&ddl.join("\n"))
}

/// Drops indexes earlier versions created that no query reads.
///
/// Each was maintained on every inserted row, so they were pure import cost.
/// Each was checked against the query text before removal:
/// - `idx_records_payload_owner(COALESCE(canonical_id, id))`: the payload join
///   resolves its owner with `p.id = COALESCE(r.canonical_id, ...)`, which is a
///   rowid lookup on `p`. The indexed expression appears in no WHERE clause,
///   only in the index definition itself.
/// - `idx_records_source_id(source_id, id)`: `source_id` is written on insert
///   and never used as a search key.
/// - `idx_records_canonical_ref(canonical_id)`: `canonical_id` is only tested
///   for NULL-ness or assigned in an UPDATE, never seeked by value.
/// - `idx_records_year(year)` and
///   `idx_records_year_scope_v2(year, dup_first_file, canonical_id)`: a year
///   filter always pairs `year` with the canonical scope predicate
///   `dup_first_file IS NULL` and constrains nothing else, which
///   `idx_records_year_scope(year, dup_first_file)` serves exactly. The plain
///   `(year)` index is a prefix of that one, and `_v2` only appends a column no
///   query constrains, making its entries larger for no gain.
///
/// `DROP INDEX IF EXISTS` is a catalog no-op once the index is gone, so this
/// costs nothing on later opens.
fn drop_superseded_indexes(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_records_payload_owner;
         DROP INDEX IF EXISTS idx_records_source_id;
         DROP INDEX IF EXISTS idx_records_canonical_ref;
         DROP INDEX IF EXISTS idx_records_year;
         DROP INDEX IF EXISTS idx_records_year_scope_v2;",
    )
}

fn ensure_meta_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );",
    )
}

fn ensure_fts_schema(conn: &Connection) -> rusqlite::Result<()> {
    if meta::get(conn, "fts_schema").as_deref() != Some(fts_index::SCHEMA_VERSION)
        && !table_exists(conn, "records_fts")?
    {
        meta::set(conn, "fts_watermark", "0");
    }
    Ok(())
}

fn records_ddl_for(table_name: &str) -> String {
    let mut fields: Vec<String> = COLUMNS
        .iter()
        .map(|column| format!("{} TEXT", column.name))
        .collect();
    fields.extend(derived::ddl_definitions());
    format!(
        "CREATE TABLE IF NOT EXISTS {table_name} (
            id INTEGER PRIMARY KEY,
            row_hash BLOB NOT NULL,
            source_file TEXT NOT NULL,
            year INTEGER,
            dup_first_file TEXT,
            canonical_id INTEGER,
            schema_id INTEGER,
            source_id INTEGER,
            extra TEXT,
            imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            {}
        )",
        fields.join(",\n            ")
    )
}

fn records_ddl() -> String {
    records_ddl_for("records")
}

fn migrate_records_schema(conn: &Connection) -> rusqlite::Result<()> {
    let current_schema = meta::get(conn, "records_schema");
    let hash_rebuild_pending =
        meta::get(conn, RECORD_HASH_REBUILD_PENDING_KEY).as_deref() == Some("1");
    if current_schema.as_deref() == Some(RECORDS_SCHEMA_VERSION) && !hash_rebuild_pending {
        return Ok(());
    }

    if table_exists(conn, "records_v2")? {
        if table_exists(conn, "records")? {
            conn.execute_batch("DROP TABLE records_v2;")?;
        } else {
            conn.execute_batch("ALTER TABLE records_v2 RENAME TO records;")?;
            meta::set(conn, "fts_watermark", "0");
        }
    }

    if table_exists(conn, "records")? {
        let has_dup_first = table_has_column(conn, "records", "dup_first_file")?;
        let has_canonical_id = table_has_column(conn, "records", "canonical_id")?;
        let has_schema_id = table_has_column(conn, "records", "schema_id")?;
        let has_source_id = table_has_column(conn, "records", "source_id")?;
        let has_extra = table_has_column(conn, "records", "extra")?;
        if records_have_known_columns(conn)? {
            let schema_version = current_schema
                .as_deref()
                .and_then(|version| version.parse::<u32>().ok())
                .unwrap_or(0);
            if !has_dup_first {
                conn.execute_batch("ALTER TABLE records ADD COLUMN dup_first_file TEXT;")?;
            }
            if !has_canonical_id {
                conn.execute_batch("ALTER TABLE records ADD COLUMN canonical_id INTEGER;")?;
            }
            if !has_schema_id {
                conn.execute_batch("ALTER TABLE records ADD COLUMN schema_id INTEGER;")?;
            }
            if !has_source_id {
                conn.execute_batch("ALTER TABLE records ADD COLUMN source_id INTEGER;")?;
            }
            if !has_extra {
                conn.execute_batch("ALTER TABLE records ADD COLUMN extra TEXT;")?;
            }
            if hash_rebuild_pending {
                let total_rows: i64 =
                    conn.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))?;
                eprintln!(
                    "[base-search] One-time database upgrade: resuming an interrupted row fingerprint rebuild."
                );
                crate::storage::maintenance::checkpoint_wal_truncate(conn)?;
                rebuild_record_hashes(conn, total_rows.max(0) as u64)?;
                meta::delete(conn, RECORD_HASH_REBUILD_PENDING_KEY)?;
            }
            if table_exists(conn, "import_log")? {
                import_log::reset_file_hashes(conn)?;
            }
            if schema_version < 2 {
                meta::set(conn, "fts_watermark", "0");
            }
            if schema_version < 5 {
                backfill_derived_columns(conn)?;
            } else {
                ensure_derived_columns(conn)?;
            }
            meta::set(conn, "records_schema", RECORDS_SCHEMA_VERSION);
            let _ = crate::storage::maintenance::checkpoint_wal_truncate(conn);
            return Ok(());
        }

        let column_names = COLUMNS.iter().map(|column| column.name).collect::<Vec<_>>();
        let columns_sql = column_names.join(", ");
        let dup_expr = if has_dup_first {
            "dup_first_file"
        } else {
            "NULL AS dup_first_file"
        };
        let canonical_expr = if has_canonical_id {
            "canonical_id"
        } else {
            "NULL AS canonical_id"
        };
        let extra_expr = if has_extra { "extra" } else { "NULL AS extra" };
        let schema_expr = if has_schema_id {
            "schema_id"
        } else {
            "NULL AS schema_id"
        };
        let source_expr = if has_source_id {
            "source_id"
        } else {
            "NULL AS source_id"
        };

        let legacy_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))?;
        if legacy_rows >= BACKFILL_PROGRESS_MIN_ROWS {
            eprintln!(
                "[base-search] One-time upgrade of a legacy database layout ({legacy_rows} rows). \
                 This copies the table once and can take a while."
            );
        }
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let migration_result = (|| -> rusqlite::Result<()> {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES (?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [RECORD_HASH_REBUILD_PENDING_KEY],
            )?;
            conn.execute_batch(
                "DROP TABLE IF EXISTS records_fts; DROP TABLE IF EXISTS records_v2;",
            )?;
            conn.execute_batch(&records_ddl_for("records_v2"))?;
            conn.execute_batch(&format!(
                "INSERT INTO records_v2 (
                    id, row_hash, source_file, year, dup_first_file, canonical_id,
                    schema_id, source_id, extra, imported_at, {columns_sql}
                 )
                 SELECT
                    id, row_hash, source_file, year, {dup_expr}, {canonical_expr},
                    {schema_expr}, {source_expr}, {extra_expr}, imported_at, {columns_sql}
                 FROM records;
                 DROP TABLE records;
                 ALTER TABLE records_v2 RENAME TO records;"
            ))?;
            Ok(())
        })();
        match migration_result {
            Ok(()) => conn.execute_batch("COMMIT;")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(err);
            }
        }
        eprintln!(
            "[base-search] One-time database upgrade: finalizing the legacy table copy before rebuilding row fingerprints."
        );
        crate::storage::maintenance::checkpoint_wal_truncate(conn)?;
        rebuild_record_hashes(conn, legacy_rows.max(0) as u64)?;
        meta::delete(conn, RECORD_HASH_REBUILD_PENDING_KEY)?;
        if table_exists(conn, "import_log")? {
            import_log::reset_file_hashes(conn)?;
        }
        meta::set(conn, "fts_watermark", "0");
    }

    if table_exists(conn, "records")? {
        backfill_derived_columns(conn)?;
    }
    meta::set(conn, "records_schema", RECORDS_SCHEMA_VERSION);
    // The migration rewrites large parts of the table, which can leave a WAL
    // comparable to the database size. Fold it back into the main file now.
    let _ = crate::storage::maintenance::checkpoint_wal_truncate(conn);
    Ok(())
}

/// Rows per backfill chunk. Chunking keeps the WAL bounded (a truncating
/// checkpoint runs between chunks) and lets multi-million-row upgrades
/// report progress instead of looking frozen for many minutes.
const BACKFILL_CHUNK_ROWS: i64 = 200_000;
/// Progress messages start at this size; small databases upgrade silently.
const BACKFILL_PROGRESS_MIN_ROWS: i64 = 100_000;

fn backfill_derived_columns(conn: &Connection) -> rusqlite::Result<()> {
    ensure_derived_columns(conn)?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))?;
    if total == 0 {
        return Ok(());
    }
    let report = total >= BACKFILL_PROGRESS_MIN_ROWS;
    if report {
        eprintln!(
            "[base-search] One-time database upgrade: computing typed columns for {total} rows. \
             This runs once and can take several minutes on large databases."
        );
    }
    let assignments = derived::backfill_assignments();
    // Advance by the real id of the last row in each batch, not by fixed
    // id-space steps: a legacy database with sparse or very large ids would
    // otherwise loop over millions of empty id ranges. The number of chunks
    // now tracks the row count, never the id magnitude.
    let update_sql = format!(
        "UPDATE records SET {assignments}
         WHERE id IN (SELECT id FROM records WHERE id > ?1 ORDER BY id LIMIT ?2)"
    );
    let started = std::time::Instant::now();
    let mut done_rows: i64 = 0;
    let mut cursor: i64 = 0;
    loop {
        let next_cursor: Option<i64> = conn.query_row(
            "SELECT MAX(id) FROM (SELECT id FROM records WHERE id > ?1 ORDER BY id LIMIT ?2)",
            params![cursor, BACKFILL_CHUNK_ROWS],
            |row| row.get(0),
        )?;
        let Some(next_cursor) = next_cursor else {
            break;
        };
        let changed = conn.execute(&update_sql, params![cursor, BACKFILL_CHUNK_ROWS])?;
        done_rows += changed as i64;
        cursor = next_cursor;
        // Fold the chunk back into the main file so the WAL never grows to
        // database size during the upgrade.
        let _ = crate::storage::maintenance::checkpoint_wal_truncate(conn);
        if report {
            let percent = (done_rows.min(total) as f64 / total as f64) * 100.0;
            eprintln!(
                "[base-search] Database upgrade: {percent:.0}% ({done_rows} of {total} rows, {}s elapsed)",
                started.elapsed().as_secs()
            );
        }
        if changed == 0 {
            break;
        }
    }
    if report {
        eprintln!(
            "[base-search] Database upgrade finished in {}s.",
            started.elapsed().as_secs()
        );
    }
    Ok(())
}

fn ensure_derived_columns(conn: &Connection) -> rusqlite::Result<()> {
    for column in derived::DERIVED {
        if !table_has_column(conn, "records", column.name)? {
            conn.execute_batch(&format!(
                "ALTER TABLE records ADD COLUMN {} {};",
                column.name, column.sql_type
            ))?;
        }
    }
    Ok(())
}

fn records_have_known_columns(conn: &Connection) -> rusqlite::Result<bool> {
    for name in ["id", "row_hash", "source_file", "year", "imported_at"] {
        if !table_has_column(conn, "records", name)? {
            return Ok(false);
        }
    }
    for column in COLUMNS {
        if !table_has_column(conn, "records", column.name)? {
            return Ok(false);
        }
    }
    Ok(true)
}

const HASH_REBUILD_CHUNK_ROWS: usize = 10_000;

#[derive(Debug, Default, PartialEq, Eq)]
struct HashRebuildStats {
    rows: u64,
    batches: u64,
    max_batch_rows: usize,
}

fn rebuild_record_hashes(conn: &Connection, total_rows: u64) -> rusqlite::Result<()> {
    rebuild_record_hashes_in_chunks(conn, total_rows, HASH_REBUILD_CHUNK_ROWS).map(|_| ())
}

fn rebuild_record_hashes_in_chunks(
    conn: &Connection,
    total_rows: u64,
    chunk_rows: usize,
) -> rusqlite::Result<HashRebuildStats> {
    assert!(chunk_rows > 0, "hash rebuild chunks must not be empty");
    let select: Vec<String> = COLUMNS
        .iter()
        .map(|column| column.name.to_string())
        .collect();
    let selected_columns = select.join(", ");
    let report_progress = total_rows >= BACKFILL_PROGRESS_MIN_ROWS as u64;
    if report_progress {
        eprintln!(
            "[base-search] One-time database upgrade: rebuilding row fingerprints for {total_rows} rows in bounded batches."
        );
    }
    let started = std::time::Instant::now();
    let mut cursor = None;
    let mut stats = HashRebuildStats::default();

    loop {
        let (sql, parameters) = if let Some(cursor) = cursor {
            (
                format!(
                    "SELECT id, {selected_columns}, extra FROM records
                     WHERE id > ?1 ORDER BY id LIMIT ?2"
                ),
                vec![cursor, i64::try_from(chunk_rows).unwrap_or(i64::MAX)],
            )
        } else {
            (
                format!(
                    "SELECT id, {selected_columns}, extra FROM records
                     ORDER BY id LIMIT ?1"
                ),
                vec![i64::try_from(chunk_rows).unwrap_or(i64::MAX)],
            )
        };
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
        let mut updates = Vec::with_capacity(chunk_rows);
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let mut values = Vec::with_capacity(COLUMNS.len());
            for index in 0..COLUMNS.len() {
                values.push(row.get::<_, Option<String>>(index + 1)?.unwrap_or_default());
            }
            let extra: Option<String> = row.get(COLUMNS.len() + 1)?;
            updates.push((
                id,
                records::canonical_record_hash(&values, extra.as_deref()),
            ));
        }
        drop(rows);
        drop(statement);
        if updates.is_empty() {
            break;
        }

        let batch_rows = updates.len();
        cursor = updates.last().map(|(id, _)| *id);
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let update_result = (|| -> rusqlite::Result<()> {
            let mut statement =
                conn.prepare_cached("UPDATE records SET row_hash = ?1 WHERE id = ?2")?;
            for (id, hash) in &updates {
                statement.execute(params![&hash[..], id])?;
            }
            Ok(())
        })();
        match update_result {
            Ok(()) => conn.execute_batch("COMMIT;")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(error);
            }
        }
        crate::storage::maintenance::checkpoint_wal_truncate(conn)?;

        stats.rows = stats.rows.saturating_add(batch_rows as u64);
        stats.batches = stats.batches.saturating_add(1);
        stats.max_batch_rows = stats.max_batch_rows.max(batch_rows);
        if report_progress {
            let percent = if total_rows == 0 {
                100.0
            } else {
                (stats.rows.min(total_rows) as f64 / total_rows as f64) * 100.0
            };
            eprintln!(
                "[base-search] Database upgrade: row fingerprints {percent:.0}% ({} of {total_rows} rows, {}s elapsed)",
                stats.rows,
                started.elapsed().as_secs()
            );
        }
    }

    Ok(stats)
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type IN ('table', 'virtual table') AND name = ?1
        )",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::{
        RECORD_HASH_REBUILD_PENDING_KEY, RECORDS_SCHEMA_VERSION, migrate_records_schema,
        rebuild_record_hashes_in_chunks, records_ddl, table_has_column,
    };
    use crate::schema::{COLUMNS, col_index};
    use crate::storage::{meta, records};

    #[test]
    fn record_hash_rebuild_uses_bounded_batches_and_sparse_real_ids() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(&records_ddl()).unwrap();
        let fixtures = [
            (-9_i64, "negative sparse row"),
            (7_i64, "small sparse row"),
            (9_000_000_000_i64, "large sparse row"),
        ];
        for (id, description) in fixtures {
            connection
                .execute(
                    "INSERT INTO records(id, row_hash, source_file, description)
                     VALUES(?1, zeroblob(16), 'legacy.xlsx', ?2)",
                    params![id, description],
                )
                .unwrap();
        }

        let stats = rebuild_record_hashes_in_chunks(&connection, fixtures.len() as u64, 2).unwrap();

        assert_eq!(stats.rows, 3);
        assert_eq!(stats.batches, 2);
        assert_eq!(stats.max_batch_rows, 2);
        let description_column = col_index("description").unwrap();
        for (id, description) in fixtures {
            let actual: Vec<u8> = connection
                .query_row("SELECT row_hash FROM records WHERE id = ?1", [id], |row| {
                    row.get(0)
                })
                .unwrap();
            let mut values = vec![String::new(); COLUMNS.len()];
            values[description_column] = description.to_string();
            assert_eq!(actual, records::canonical_record_hash(&values, None));
        }
    }

    #[test]
    fn pending_record_hash_rebuild_is_resumed_before_schema_is_marked_current() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        connection.execute_batch(&records_ddl()).unwrap();
        connection
            .execute(
                "INSERT INTO records(id, row_hash, source_file, description)
                 VALUES(9000000000, zeroblob(16), 'legacy.xlsx', 'resume marker row')",
                [],
            )
            .unwrap();
        meta::set(&connection, "records_schema", "5");
        meta::set(&connection, RECORD_HASH_REBUILD_PENDING_KEY, "1");

        migrate_records_schema(&connection).unwrap();

        assert_eq!(
            meta::get(&connection, RECORD_HASH_REBUILD_PENDING_KEY),
            None
        );
        assert_eq!(
            meta::get(&connection, "records_schema").as_deref(),
            Some(RECORDS_SCHEMA_VERSION)
        );
        let actual: Vec<u8> = connection
            .query_row(
                "SELECT row_hash FROM records WHERE id = 9000000000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut values = vec![String::new(); COLUMNS.len()];
        values[col_index("description").unwrap()] = "resume marker row".to_string();
        assert_eq!(actual, records::canonical_record_hash(&values, None));
    }

    #[test]
    fn v5_upgrade_adds_canonical_id_without_touching_existing_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        let legacy_ddl = records_ddl().replace("            canonical_id INTEGER,\n", "");
        connection.execute_batch(&legacy_ddl).unwrap();
        connection
            .execute(
                "INSERT INTO records(row_hash, source_file, description)
                 VALUES (zeroblob(16), 'legacy.xlsx', 'preserve me')",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_records_update
                 BEFORE UPDATE ON records
                 BEGIN
                     SELECT RAISE(ABORT, 'records backfill is forbidden');
                 END;",
            )
            .unwrap();
        meta::set(&connection, "records_schema", "5");

        migrate_records_schema(&connection).unwrap();
        migrate_records_schema(&connection).unwrap();

        assert!(table_has_column(&connection, "records", "canonical_id").unwrap());
        assert_eq!(
            meta::get(&connection, "records_schema").as_deref(),
            Some(RECORDS_SCHEMA_VERSION)
        );
        let preserved: (String, Option<i64>) = connection
            .query_row(
                "SELECT description, canonical_id FROM records WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(preserved, ("preserve me".to_string(), None));
    }
}
