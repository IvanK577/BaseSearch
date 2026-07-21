use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::db::ImportRecord;
use crate::schema::{self, COLUMNS};
use crate::storage::derived;

pub(crate) fn begin_import_file(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
}

pub(crate) fn commit_import_file(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("COMMIT")
}

pub(crate) fn rollback_import_file(conn: &Connection) {
    let _ = conn.execute_batch("ROLLBACK");
}

pub(crate) fn insert_batch(
    conn: &Connection,
    source_file: &str,
    records: &[ImportRecord],
) -> rusqlite::Result<(u64, u64)> {
    insert_batch_scoped(conn, source_file, None, None, records)
}

pub(crate) fn insert_batch_scoped(
    conn: &Connection,
    source_file: &str,
    schema_id: Option<i64>,
    source_id: Option<i64>,
    records: &[ImportRecord],
) -> rusqlite::Result<(u64, u64)> {
    if records.is_empty() {
        return Ok((0, 0));
    }
    let col_names: Vec<&str> = COLUMNS.iter().map(|column| column.name).collect();
    let derived_src: Vec<usize> = derived::DERIVED
        .iter()
        .map(|column| schema::col_index(column.source).expect("derived source is a schema column"))
        .collect();
    let derived_count = derived::DERIVED.len();
    let full_sql = format!(
        "INSERT INTO records (
             row_hash, source_file, year, dup_first_file, canonical_id,
             schema_id, source_id, extra, {}, {}
         ) VALUES ({})",
        col_names.join(", "),
        derived::insert_column_list(),
        std::iter::repeat_n("?", 8 + col_names.len() + derived_count)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let thin_sql = "INSERT INTO records (
                        row_hash, source_file, dup_first_file, canonical_id,
                        schema_id, source_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

    conn.execute_batch("SAVEPOINT insert_batch")?;
    let result = (|| -> rusqlite::Result<(u64, u64)> {
        let mut first_seen: u64 = 0;
        let mut duplicates: u64 = 0;
        let mut canonical_by_hash = load_existing_canonicals(conn, schema_id, records)?;
        let mut full_stmt = conn.prepare_cached(&full_sql)?;
        let mut thin_stmt = conn.prepare_cached(thin_sql)?;
        for rec in records {
            if let Some((canonical_id, first_file)) = canonical_by_hash.get(&rec.hash) {
                thin_stmt.execute((
                    &rec.hash[..],
                    source_file,
                    first_file.as_str(),
                    canonical_id,
                    schema_id,
                    source_id,
                ))?;
                duplicates += 1;
                continue;
            }

            full_stmt.raw_bind_parameter(1, &rec.hash[..])?;
            full_stmt.raw_bind_parameter(2, source_file)?;
            full_stmt.raw_bind_parameter(3, rec.year)?;
            full_stmt.raw_bind_parameter(4, rusqlite::types::Null)?;
            full_stmt.raw_bind_parameter(5, rusqlite::types::Null)?;
            full_stmt.raw_bind_parameter(6, schema_id)?;
            full_stmt.raw_bind_parameter(7, source_id)?;
            full_stmt.raw_bind_parameter(8, rec.extra.as_deref())?;
            for (i, value) in rec.values.iter().enumerate() {
                full_stmt.raw_bind_parameter(9 + i, value.as_str())?;
            }
            let derived_base = 9 + rec.values.len();
            for (j, column) in derived::DERIVED.iter().enumerate() {
                let source_value = rec.values[derived_src[j]].as_str();
                let value = derived::compute(column.derivation, source_value);
                full_stmt.raw_bind_parameter(derived_base + j, value)?;
            }
            full_stmt.raw_execute()?;
            let canonical_id = conn.last_insert_rowid();
            canonical_by_hash.insert(rec.hash, (canonical_id, source_file.to_string()));
            first_seen += 1;
        }
        Ok((first_seen + duplicates, duplicates))
    })();
    match result {
        Ok(counts) => {
            conn.execute_batch("RELEASE insert_batch")?;
            Ok(counts)
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK TO insert_batch");
            let _ = conn.execute_batch("RELEASE insert_batch");
            Err(err)
        }
    }
}

fn load_existing_canonicals(
    conn: &Connection,
    schema_id: Option<i64>,
    records: &[ImportRecord],
) -> rusqlite::Result<HashMap<[u8; 16], (i64, String)>> {
    const LOOKUP_CHUNK: usize = 500;
    let mut seen = HashSet::with_capacity(records.len());
    let hashes = records
        .iter()
        .filter_map(|record| seen.insert(record.hash).then_some(record.hash))
        .collect::<Vec<_>>();
    let mut canonicals = HashMap::with_capacity(hashes.len());
    for chunk in hashes.chunks(LOOKUP_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let schema_clause = if schema_id.is_some() {
            "schema_id = ?"
        } else {
            "schema_id IS NULL"
        };
        let sql = format!(
            "SELECT row_hash, id, source_file
             FROM records
             WHERE row_hash IN ({placeholders})
               AND {schema_clause}
               AND dup_first_file IS NULL
               AND canonical_id IS NULL
             ORDER BY id"
        );
        let mut params = chunk
            .iter()
            .map(|hash| Value::Blob(hash.to_vec()))
            .collect::<Vec<_>>();
        if let Some(schema_id) = schema_id {
            params.push(schema_id.into());
        }
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let bytes: Vec<u8> = row.get(0)?;
            let Ok(hash) = <[u8; 16]>::try_from(bytes.as_slice()) else {
                continue;
            };
            canonicals.entry(hash).or_insert((row.get(1)?, row.get(2)?));
        }
    }
    Ok(canonicals)
}

pub(crate) fn total_rows(conn: &Connection) -> u64 {
    conn.query_row("SELECT COUNT(*) FROM records", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0) as u64
}

pub(crate) fn max_record_id(conn: &Connection) -> u64 {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0)
    .max(0) as u64
}
