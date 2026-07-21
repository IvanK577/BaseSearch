use std::sync::atomic::{AtomicBool, Ordering};

use base_search::db::{Db, FtsRepairIssue, ImportRecord, Query, canonical_record_hash};
use base_search::schema::{COLUMNS, col_index};

fn record(description: &str, extra: Option<&str>) -> ImportRecord {
    let mut values = vec![String::new(); COLUMNS.len()];
    values[col_index("declaration_number").unwrap()] = "24UA100000000001U1".to_string();
    values[col_index("description").unwrap()] = description.to_string();
    ImportRecord {
        hash: canonical_record_hash(&values, extra),
        year: Some(2024),
        values,
        extra: extra.map(str::to_string),
    }
}

fn query(text: &str) -> Query {
    Query {
        text: text.to_string(),
        ..Default::default()
    }
}

#[test]
fn opening_a_version_stale_database_preserves_the_live_fts_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale-fts.db");
    let cancel = AtomicBool::new(false);
    let extra = serde_json::to_string(&vec![("Серія", "Промислова")]).unwrap();

    let mut db = Db::open(&path).unwrap();
    db.insert_batch(
        "source.xlsx",
        &[record("Український гідравлічний насос", Some(&extra))],
    )
    .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();
    assert_eq!(db.count(&query("гідравлічний")).unwrap(), 1);
    db.meta_set("fts_schema", "legacy-version");
    drop(db);

    let db = Db::open(&path).unwrap();
    assert_eq!(db.meta_get("fts_schema").as_deref(), Some("legacy-version"));
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT COUNT(*) FROM records_fts
             WHERE records_fts MATCH '\"гідравлічний\"'",
            1,
        )
        .unwrap(),
        vec![vec!["1".to_string()]]
    );
    assert_eq!(db.count(&query("гідравлічний")).unwrap(), 1);
}

#[test]
fn repair_replaces_logically_corrupt_contents_and_preserves_live_search_until_swap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt-fts.db");
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&path).unwrap();
    db.insert_batch(
        "unicode.xlsx",
        &[
            record("Український гідравлічний насос", None),
            record("東京精密機器", None),
            record("مضخة صناعية", None),
        ],
    )
    .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();

    db.diagnostic_execute_batch(
        "INSERT INTO records_fts(records_fts, rowid, search_text)
         VALUES ('delete', 1, 'Український гідравлічний насос');",
    )
    .unwrap();
    assert_eq!(db.count(&query("гідравлічний")).unwrap(), 0);
    assert_eq!(db.count(&query("東京精密機器")).unwrap(), 1);

    let live_index_observed = AtomicBool::new(false);
    let report = db
        .repair_fts(&cancel, |done, _| {
            if done == 0 || live_index_observed.load(Ordering::Relaxed) {
                return;
            }
            let live = Db::open_runtime(&path).unwrap();
            assert_eq!(live.count(&query("東京精密機器")).unwrap(), 1);
            live_index_observed.store(true, Ordering::Relaxed);
        })
        .unwrap();

    assert!(report.rebuilt);
    assert!(!report.cancelled);
    assert_eq!(report.indexed_rows, 3);
    assert_eq!(report.watermark, 3);
    assert!(
        report.issues.contains(&FtsRepairIssue::ContentMismatch),
        "unexpected repair issues: {:?}",
        report.issues
    );
    assert!(live_index_observed.load(Ordering::Relaxed));
    assert_eq!(
        db.meta_get("fts_schema").as_deref(),
        Some(report.schema_version.as_str())
    );
    assert_eq!(db.meta_get("fts_watermark").as_deref(), Some("3"));

    assert_eq!(db.count(&query("ГІДРАВЛІЧНИЙ")).unwrap(), 1);
    assert_eq!(db.count(&query("東京精密機器")).unwrap(), 1);
    assert_eq!(db.count(&query("صناعية")).unwrap(), 1);
}

#[test]
fn cancelled_repair_keeps_live_index_and_all_success_metadata_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancelled-fts.db");
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&path).unwrap();
    db.insert_batch("initial.xlsx", &[record("Stable searchable record", None)])
        .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();
    db.meta_set("fts_schema", "legacy-version");
    db.meta_set("fts_last_rebuilt_at", "old-timestamp");
    db.diagnostic_execute_batch(
        "WITH RECURSIVE rows(n) AS (
             VALUES(1)
             UNION ALL
             SELECT n + 1 FROM rows WHERE n < 20050
         )
         INSERT INTO records(row_hash, source_file, description)
         SELECT randomblob(16), 'tail.xlsx', printf('Tail row %d', n) FROM rows;",
    )
    .unwrap();

    let report = db
        .repair_fts(&cancel, |done, _| {
            if done > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();

    assert!(!report.rebuilt);
    assert!(report.cancelled);
    assert_eq!(db.meta_get("fts_schema").as_deref(), Some("legacy-version"));
    assert_eq!(db.meta_get("fts_watermark").as_deref(), Some("1"));
    assert_eq!(
        db.meta_get("fts_last_rebuilt_at").as_deref(),
        Some("old-timestamp")
    );
    assert_eq!(db.count(&query("Stable searchable record")).unwrap(), 1);
    assert_eq!(rebuild_artifact_count(&db), 0);
}

#[test]
fn concurrent_source_change_aborts_repair_without_replacing_the_live_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent-change.db");
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&path).unwrap();
    db.insert_batch("initial.xlsx", &[record("Original indexed value", None)])
        .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();
    db.meta_set("fts_schema", "legacy-version");

    let changed = AtomicBool::new(false);
    let error = db
        .repair_fts(&cancel, |done, _| {
            if done == 0 || changed.swap(true, Ordering::Relaxed) {
                return;
            }
            let concurrent = Db::open_runtime(&path).unwrap();
            concurrent
                .diagnostic_execute(
                    "INSERT INTO records(row_hash, source_file, description)
                     VALUES(randomblob(16), 'concurrent.xlsx', 'Concurrent tail value')",
                )
                .unwrap();
        })
        .unwrap_err();

    assert!(error.to_string().contains("changed during FTS rebuild"));
    assert_eq!(db.meta_get("fts_schema").as_deref(), Some("legacy-version"));
    assert_eq!(db.meta_get("fts_watermark").as_deref(), Some("1"));
    assert_eq!(db.count(&query("Original indexed value")).unwrap(), 1);
    assert_eq!(rebuild_artifact_count(&db), 0);
}

#[test]
fn successful_repair_marks_a_stale_schema_current_only_after_the_swap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale-success.db");
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&path).unwrap();
    db.insert_batch("source.xlsx", &[record("Versioned searchable value", None)])
        .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();
    db.meta_set("fts_schema", "legacy-version");

    let report = db.repair_fts(&cancel, |_, _| {}).unwrap();

    assert!(report.issues.contains(&FtsRepairIssue::VersionStale));
    assert_eq!(
        db.meta_get("fts_schema").as_deref(),
        Some(report.schema_version.as_str())
    );
    assert_eq!(db.meta_get("fts_watermark").as_deref(), Some("1"));
    assert!(db.meta_get("fts_last_rebuilt_at").is_some());
    assert_eq!(db.count(&query("Versioned searchable value")).unwrap(), 1);
}

#[test]
fn an_immediately_cancelled_repair_removes_a_stale_rebuild_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale-artifact.db");
    let mut db = Db::open(&path).unwrap();
    db.diagnostic_execute_batch(
        "CREATE VIRTUAL TABLE records_fts_rebuild USING fts5(
             search_text,
             content='',
             detail=none,
             columnsize=0,
             tokenize='unicode61 remove_diacritics 2'
         );
         INSERT INTO records_fts_rebuild(rowid, search_text)
         VALUES(1, 'abandoned staging value');",
    )
    .unwrap();
    assert!(rebuild_artifact_count(&db) > 0);

    let cancel = AtomicBool::new(true);
    let report = db.repair_fts(&cancel, |_, _| {}).unwrap();

    assert!(report.cancelled);
    assert!(!report.rebuilt);
    assert_eq!(rebuild_artifact_count(&db), 0);
}

fn rebuild_artifact_count(db: &Db) -> u64 {
    db.diagnostic_query_rows(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name LIKE 'records_fts_rebuild%'",
        1,
    )
    .unwrap()[0][0]
        .parse()
        .unwrap()
}
