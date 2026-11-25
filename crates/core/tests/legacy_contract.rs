use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use base_search_core::{Error, Operation, legacy::migrate_legacy, workspace::Workspace};
use rusqlite::Connection;
use serde_json::Value;

fn create_legacy(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
         INSERT INTO meta VALUES ('records_schema', '7');
         CREATE TABLE records (
             id INTEGER PRIMARY KEY, row_hash BLOB, source_file TEXT NOT NULL,
             dup_first_file TEXT, canonical_id INTEGER, schema_id INTEGER,
             source_id INTEGER, extra TEXT,
             declaration_date TEXT, recipient TEXT, description TEXT, currency_control_value TEXT
         );
         CREATE TABLE source_schemas (
             id INTEGER PRIMARY KEY, fixed_currency TEXT, fixed_weight_unit TEXT, detected_currency TEXT
         );
         CREATE TABLE source_columns (
             schema_id INTEGER NOT NULL, source_index INTEGER NOT NULL,
             raw_header TEXT NOT NULL, display_header TEXT NOT NULL, normalized_header TEXT NOT NULL,
             role TEXT NOT NULL, semantic TEXT, storage_kind TEXT NOT NULL, storage_name TEXT
         );
         CREATE TABLE import_sources (
             id INTEGER PRIMARY KEY, schema_id INTEGER NOT NULL,
             source_file TEXT NOT NULL, table_name TEXT NOT NULL, import_fingerprint TEXT NOT NULL
         );
         INSERT INTO source_schemas VALUES (7, NULL, 'kg', 'USD'), (9, 'EUR', NULL, 'USD');
         INSERT INTO source_columns VALUES
             (7, 2, 'CUSTOM FIELD', 'Custom Field', 'custom field', 'text', NULL, 'source_json', NULL),
             (7, 0, 'Date', 'Date', 'date', 'date', 'date', 'schema_column', 'declaration_date'),
             (7, 1, 'SKU', 'SKU', 'sku', 'code', 'product_code', 'source_json', NULL),
             (9, 0, 'Product', 'Product', 'product', 'text', 'description', 'schema_column', 'description');
         INSERT INTO import_sources VALUES
             (10, 7, 'first.xlsx', 'Data', 'same-v2-file-hash'),
             (11, 7, 'second.xlsx', 'Second', 'same-v2-file-hash'),
             (13, 9, 'fixed.csv', 'fixed.csv', 'another');
         INSERT INTO records VALUES
             (1, X'01', 'first.xlsx', NULL, NULL, 7, 10,
              '[["sku","00042"],["Custom Field"," untouched "]]', '2024-06-15', '', '', ''),
             (2, X'01', 'second.xlsx', 'first.xlsx', 1, 7, 11, NULL, NULL, NULL, NULL, NULL),
             (3, X'02', 'pre-schema.csv', NULL, NULL, NULL, NULL,
              '[["Old Column","abc"]]', '2023-01-21', 'Example', 'Synthetic', '4.50'),
             (4, X'01', 'second.xlsx', 'first.xlsx', NULL, 7, 11, NULL, NULL, NULL, NULL, NULL),
             (5, X'03', 'fixed.csv', NULL, NULL, 9, 13, NULL, '', '', 'P', '');"#,
    )
    .unwrap();
    conn
}

fn source_rows(workspace: &Workspace, source_name: &str) -> Vec<Vec<(String, Value)>> {
    let conn = workspace.connect().unwrap();
    let mut statement = conn
        .prepare(
            "SELECT s.label, s.column_id, r.raw, r.id
         FROM source_columns s JOIN sources o ON o.id = s.source_id
         JOIN records r ON r.source_id = s.source_id
         WHERE o.file_name = ?1 ORDER BY r.id, s.ordinal",
        )
        .unwrap();
    let mut rows = Vec::<Vec<(String, Value)>>::new();
    let mut last = None;
    for pair in statement
        .query_map([source_name], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
    {
        let (label, key, raw, id) = pair.unwrap();
        if last != Some(id) {
            rows.push(Vec::new());
            last = Some(id);
        }
        let raw: Value = serde_json::from_str(&raw).unwrap();
        rows.last_mut().unwrap().push((label, raw[&key].clone()));
    }
    rows
}

#[test]
fn migrates_effective_occurrences_and_reconstructs_raw_cells_from_v2_storage() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("v2.db");
    drop(create_legacy(&old));
    let original = fs::read(&old).unwrap();
    let workspace = Workspace::open(dir.path().join("v1.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let operation = Operation::new(&cancel, &|_| {});
    let report = migrate_legacy(&workspace, &old, &operation).unwrap();
    assert_eq!(report.inserted_rows, 5);
    assert_eq!(fs::read(&old).unwrap(), original);
    let expected = vec![
        ("Date".into(), Value::from("2024-06-15")),
        ("SKU".into(), Value::from("00042")),
        ("CUSTOM FIELD".into(), Value::from(" untouched ")),
    ];
    assert_eq!(
        source_rows(&workspace, "first.xlsx").as_slice(),
        std::slice::from_ref(&expected)
    );
    assert_eq!(
        source_rows(&workspace, "second.xlsx"),
        [expected.clone(), expected]
    );
    let synthetic = source_rows(&workspace, "pre-schema.csv");
    assert!(synthetic[0].contains(&("Опис товару".into(), Value::from("Synthetic"))));
    assert!(synthetic[0].contains(&("Old Column".into(), Value::from("abc"))));
    let conn = workspace.connect().unwrap();
    let settings: (String, String, String) = conn.query_row(
        "SELECT sheet, fixed_currency, fixed_weight_unit FROM sources WHERE file_name = 'first.xlsx'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(settings, ("Data".into(), "USD".into(), "kg".into()));
    let fixed: String = conn
        .query_row(
            "SELECT fixed_currency FROM sources WHERE file_name = 'fixed.csv'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fixed, "EUR");
    assert!(migrate_legacy(&workspace, &old, &operation).is_err());
}

#[test]
fn failed_readback_verification_rolls_back_all_legacy_rows() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("v2.db");
    drop(create_legacy(&old));
    let workspace = Workspace::open(dir.path().join("v1.db")).unwrap();
    let conn = workspace.connect().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER test_corruption AFTER INSERT ON records
         BEGIN UPDATE records SET raw = '{}' WHERE id = NEW.id; END;",
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let operation = Operation::new(&cancel, &|_| {});
    assert!(migrate_legacy(&workspace, &old, &operation).is_err());
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    let sources: i64 = conn
        .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!((rows, sources), (0, 0));
}

#[test]
fn cancelling_before_legacy_readback_keeps_new_db_empty() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("v2.db");
    drop(create_legacy(&old));
    let workspace = Workspace::open(dir.path().join("v1.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let progress = |progress: base_search_core::Progress| {
        if progress.processed >= 5 {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let result = migrate_legacy(&workspace, &old, &Operation::new(&cancel, &progress));
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(source_rows(&workspace, "first.xlsx").is_empty());
}

#[test]
fn preserves_legacy_gross_weight_without_using_it_as_net_weight() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("v2.db");
    let legacy = create_legacy(&old);
    legacy.execute_batch(
        "ALTER TABLE records ADD COLUMN gross_kg TEXT;
         UPDATE records SET gross_kg = '120.25' WHERE source_id = 13;
         INSERT INTO source_columns
         VALUES (9, 1, 'Брутто кг', 'Брутто кг', 'брутто кг', 'weight', 'gross_weight', 'schema_column', 'gross_kg');",
    ).unwrap();
    drop(legacy);

    let workspace = Workspace::open(dir.path().join("v1.db")).unwrap();
    let cancel = AtomicBool::new(false);
    migrate_legacy(&workspace, &old, &Operation::new(&cancel, &|_| {})).unwrap();
    let rows = source_rows(&workspace, "fixed.csv");
    assert_eq!(rows[0][1], ("Брутто кг".into(), Value::from("120.25")));
    let conn = workspace.connect().unwrap();
    let weight: Option<String> = conn.query_row(
        "SELECT r.weight_kg FROM records r JOIN sources s ON s.id = r.source_id WHERE s.file_name = 'fixed.csv'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(weight, None);
}
