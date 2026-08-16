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

/// Inserts rows that belong to no registered source schema.
///
/// Real imports always go through [`insert_batch_scoped`] with a schema. This
/// unscoped form is how fixtures and the pre-2.0 layout put rows in, so
/// duplicate resolution is scoped by `schema_id IS NULL` instead.
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

    // WHY there is no savepoint around this batch: the whole file is imported
    // inside one `BEGIN IMMEDIATE` opened by `begin_import_file`, and every
    // failure path in `import_file_with_options` answers with a full
    // `rollback_import_file`, so a per-batch rollback point was never the thing
    // that undid a failed batch.
    //
    // It was not free. Inside an open savepoint SQLite has to be able to return
    // to the mark, so it journals the original content of every page the batch
    // touches. With fifteen indexes over random keys each further row lands on
    // more pages the savepoint is already holding, so the cost of a row grows
    // with the number of rows already in the batch — quadratic in `BATCH_SIZE`.
    // Measured on a 400k-row database at the current `BATCH_SIZE` of 8192:
    // 0.70s for the batch without the savepoint, 140.75s with it. That is the
    // whole difference between a first import (~14k rows/s, read indexes
    // dropped) and every later one (~100 rows/s).
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
        // One row per hash, already the earliest — `MIN(id)` with bare
        // columns is SQLite's documented "take the rest of the row the
        // minimum came from", so `source_file` belongs to that same row. The
        // previous form asked for every matching row `ORDER BY id` and kept
        // the first, which made SQLite build a temporary B-tree to sort rows
        // that were then thrown away.
        let sql = format!(
            "SELECT row_hash, MIN(id), source_file
             FROM records
             WHERE row_hash IN ({placeholders})
               AND {schema_clause}
               AND dup_first_file IS NULL
               AND canonical_id IS NULL
             GROUP BY row_hash"
        );
        let mut params = chunk
            .iter()
            .map(|hash| Value::Blob(hash.to_vec()))
            .collect::<Vec<_>>();
        if let Some(schema_id) = schema_id {
            params.push(schema_id.into());
        }
        // Cached: every full chunk produces the same text, and an import runs
        // this once per 500 rows.
        let mut statement = conn.prepare_cached(&sql)?;
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

#[cfg(test)]
mod tests {
    use super::{ImportRecord, load_existing_canonicals};
    use crate::storage::connection;

    const HASH: [u8; 16] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10,
    ];

    fn probe() -> ImportRecord {
        ImportRecord {
            hash: HASH,
            year: None,
            values: Vec::new(),
            extra: None,
        }
    }

    /// Several canonical rows can share one fingerprint, and the earliest of
    /// them owns it.
    ///
    /// An import cannot produce that state — the second copy of a row is
    /// flagged as it goes in. A v1 upgrade can: `rebuild_record_hashes`
    /// recomputes every fingerprint, so two rows that were distinct under the
    /// old scheme collapse onto one value with both still marked canonical.
    /// Whichever of them the lookup picks becomes the file every later copy
    /// points back at, so it has to be the earliest and not whichever the
    /// query reached first.
    #[test]
    fn the_earliest_canonical_row_owns_a_shared_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let conn = connection::open(&dir.path().join("owners.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO records(id, row_hash, source_file) VALUES
                 (7,  x'0102030405060708090a0b0c0d0e0f10', 'earliest.xlsx'),
                 (9,  x'0102030405060708090a0b0c0d0e0f10', 'later.xlsx');",
        )
        .unwrap();

        let owners = load_existing_canonicals(&conn, None, &[probe()]).unwrap();
        assert_eq!(
            owners.get(&HASH),
            Some(&(7, "earliest.xlsx".to_string())),
            "the lowest id, and the file name from that same row"
        );
    }

    /// A row already flagged as a duplicate is not a candidate owner, even when
    /// it sorts first. Adopting one would make later copies point at a row that
    /// is itself pointing somewhere else.
    #[test]
    fn a_flagged_duplicate_is_never_adopted_as_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let conn = connection::open(&dir.path().join("flagged.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO records(id, row_hash, source_file, dup_first_file) VALUES
                 (3, x'0102030405060708090a0b0c0d0e0f10', 'copy.xlsx', 'earliest.xlsx');
             INSERT INTO records(id, row_hash, source_file) VALUES
                 (7, x'0102030405060708090a0b0c0d0e0f10', 'earliest.xlsx');",
        )
        .unwrap();

        let owners = load_existing_canonicals(&conn, None, &[probe()]).unwrap();
        assert_eq!(
            owners.get(&HASH),
            Some(&(7, "earliest.xlsx".to_string())),
            "id 3 sorts first but is a duplicate, so id 7 owns the fingerprint"
        );
    }

    /// Two schemas can hold the same fingerprint and must not resolve each
    /// other's duplicates: the same bytes under a different column layout are
    /// a different record.
    #[test]
    fn a_fingerprint_is_only_matched_within_its_own_schema() {
        let dir = tempfile::tempdir().unwrap();
        let conn = connection::open(&dir.path().join("schemas.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO records(id, row_hash, source_file, schema_id) VALUES
                 (3, x'0102030405060708090a0b0c0d0e0f10', 'other-layout.xlsx', 1),
                 (7, x'0102030405060708090a0b0c0d0e0f10', 'this-layout.xlsx', 2);",
        )
        .unwrap();

        assert_eq!(
            load_existing_canonicals(&conn, Some(2), &[probe()])
                .unwrap()
                .get(&HASH),
            Some(&(7, "this-layout.xlsx".to_string()))
        );
        assert!(
            load_existing_canonicals(&conn, None, &[probe()])
                .unwrap()
                .is_empty(),
            "schema-less rows must not adopt a schema's row"
        );
    }
}
