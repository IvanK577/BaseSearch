use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use base_search::db::{
    Db, DuplicateCompactionOptions, ImportRecord, PivotDim, PivotLimits, PivotMetric, Query,
    RecordScope, ResultSort, canonical_record_hash,
};
use base_search::schema::{COLUMNS, RESULT_COLUMNS, col_index};
use base_search::{export, schema};
use calamine::{Reader, open_workbook_auto};

const DERIVED_COLUMNS: &[&str] = &[
    "value_num",
    "net_kg_num",
    "gross_kg_num",
    "quantity_num",
    "rfv_num",
    "rmv_net_num",
    "rmv_extra_num",
    "rmv_gross_num",
    "min_base_num",
    "sender_label",
    "recipient_label",
    "edrpou_label",
    "trademark_label",
    "origin_key",
    "dispatch_key",
    "trade_key",
    "month",
];

fn occurrence_query(text: &str) -> Query {
    Query {
        text: text.to_string(),
        record_scope: RecordScope::Occurrences,
        ..Query::default()
    }
}

fn fixture_record(description: &str, extra_size: usize) -> ImportRecord {
    let mut values = vec![String::new(); COLUMNS.len()];
    for (column, value) in [
        ("declaration_number", "MD-001"),
        ("declaration_date", "2024-03-15"),
        ("sender", "Sender AG"),
        ("edrpou", "12345678"),
        ("recipient", "Importer LLC"),
        ("product_code", "8517130000"),
        ("description", description),
        ("origin_country", "CN"),
        ("dispatch_country", "DE"),
        ("trade_country", "DE"),
        ("quantity", "2"),
        ("gross_kg", "12"),
        ("net_kg", "10"),
        ("currency_control_value", "1200"),
        ("trademark", "ACME"),
    ] {
        values[col_index(column).unwrap()] = value.to_string();
    }
    let extra = (extra_size > 0).then(|| {
        serde_json::to_string(&vec![("Long source field", "x".repeat(extra_size))]).unwrap()
    });
    ImportRecord {
        hash: canonical_record_hash(&values, extra.as_deref()),
        year: Some(2024),
        values,
        extra,
    }
}

fn insert_legacy_full_occurrence(db: &Db, source_file: &str) {
    let payload_columns = COLUMNS
        .iter()
        .map(|column| column.name)
        .chain(DERIVED_COLUMNS.iter().copied())
        .collect::<Vec<_>>()
        .join(", ");
    let escaped_source = source_file.replace('\'', "''");
    db.diagnostic_execute_batch(&format!(
        "INSERT INTO records (
             row_hash, source_file, year, dup_first_file, canonical_id, extra, {payload_columns}
         )
         SELECT
             row_hash, '{escaped_source}', year, source_file, NULL, extra, {payload_columns}
         FROM records
         WHERE dup_first_file IS NULL AND canonical_id IS NULL
         ORDER BY id
         LIMIT 1;"
    ))
    .unwrap();
}

fn selected_export_fields(db: &Db) -> Vec<base_search::search::FieldInfo> {
    let catalog = db.result_fields_cached();
    export::resolve_fields(
        &catalog,
        Some(&[
            "description".to_string(),
            "currency_control_value".to_string(),
            "source_file".to_string(),
        ]),
    )
    .unwrap()
}

#[test]
fn mixed_legacy_and_thin_occurrences_preserve_every_read_surface() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.db");
    let mut db = Db::open(&path).unwrap();
    let record = fixture_record("Precision hydraulic pump", 128);

    let (inserted, duplicates) = db
        .insert_batch(
            "first.xlsx",
            &[
                fixture_record("Precision hydraulic pump", 128),
                fixture_record("Precision hydraulic pump", 128),
            ],
        )
        .unwrap();
    assert_eq!((inserted, duplicates), (2, 1));
    insert_legacy_full_occurrence(&db, "legacy-a.xlsx");
    insert_legacy_full_occurrence(&db, "legacy-b.xlsx");
    db.diagnostic_execute_batch(
        "UPDATE records
         SET description = 'DRIFTED LEGACY PAYLOAD',
             currency_control_value = '9999',
             value_num = 9999
         WHERE id = 3;",
    )
    .unwrap();

    let physical = db
        .diagnostic_query_rows(
            "SELECT id, canonical_id, source_file, description IS NULL, extra IS NULL
             FROM records ORDER BY id",
            10,
        )
        .unwrap();
    assert_eq!(physical.len(), 4);
    assert_eq!(physical[0][1], "NULL");
    assert_eq!(physical[1][1], "1");
    assert_eq!(physical[1][3], "1");
    assert_eq!(physical[1][4], "1");
    assert_eq!(physical[2][1], "NULL");
    assert_eq!(physical[2][3], "0");

    let cancel = AtomicBool::new(false);
    let indexed = db.index_fts(&cancel, |_, _| {}).unwrap();
    assert_eq!(indexed, (1, false));
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH 'hydraulic'",
            1,
        )
        .unwrap()[0][0],
        "1"
    );

    let canonical = Query {
        text: "hydraulic".to_string(),
        ..Query::default()
    };
    let occurrences = occurrence_query("hydraulic");
    assert_eq!(db.count(&canonical).unwrap(), 1);
    assert_eq!(db.count(&occurrences).unwrap(), 4);
    assert_eq!(db.count(&occurrence_query("DRIFTED")).unwrap(), 0);

    let (_, ids, rows, _) = db
        .search_page_dynamic_sorted(
            &occurrences,
            10,
            0,
            Some(ResultSort {
                field: "source_file".to_string(),
                descending: false,
            }),
        )
        .unwrap();
    assert_eq!(ids.len(), 4);
    let description_index = RESULT_COLUMNS
        .iter()
        .position(|column| *column == "description")
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row[description_index] == "Precision hydraulic pump")
    );

    let thin_card = db.record_card(2).unwrap();
    assert_eq!(thin_card.source_file, "first.xlsx");
    assert!(
        thin_card
            .fields
            .iter()
            .any(|(_, value)| value == "Precision hydraulic pump")
    );
    assert_eq!(thin_card.extra[0].0, "Long source field");

    let canonical_analytics = db.analytics(&canonical, 20).unwrap();
    let occurrence_analytics = db.analytics(&occurrences, 20).unwrap();
    assert_eq!(canonical_analytics.overview.row_count, 1);
    assert_eq!(canonical_analytics.overview.total_value_usd, 1200.0);
    assert_eq!(occurrence_analytics.overview.row_count, 4);
    assert_eq!(occurrence_analytics.overview.total_value_usd, 4800.0);
    assert_eq!(occurrence_analytics.overview.total_net_kg, 40.0);

    let pivot = db
        .pivot(
            &occurrences,
            PivotDim::Recipient,
            PivotDim::OriginCountry,
            PivotMetric::Rows,
            PivotLimits { rows: 10, cols: 10 },
            "Other",
        )
        .unwrap();
    assert_eq!(pivot.grand_total, 4.0);
    let dossier = db.company_profile("12345678", 20).unwrap();
    assert_eq!(dossier.overview.row_count, 1);
    assert_eq!(dossier.overview.total_value_usd, 1200.0);

    let fields = selected_export_fields(&db);
    let csv_path = dir.path().join("occurrences.csv");
    assert_eq!(
        export::export_selected(
            &db,
            &occurrences,
            &csv_path,
            &fields,
            None,
            &cancel,
            |_, _| {},
        )
        .unwrap(),
        4
    );
    let csv_bytes = std::fs::read(&csv_path).unwrap();
    let mut csv = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(&csv_bytes[3..]);
    let csv_rows = csv.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(csv_rows.len(), 4);
    assert!(
        csv_rows
            .iter()
            .all(|row| row.get(0) == Some("Precision hydraulic pump"))
    );
    assert_eq!(
        csv_rows
            .iter()
            .map(|row| row.get(2).unwrap().to_string())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "first.xlsx".to_string(),
            "legacy-a.xlsx".to_string(),
            "legacy-b.xlsx".to_string(),
        ])
    );

    let xlsx_path = dir.path().join("occurrences.xlsx");
    assert_eq!(
        export::export_selected(
            &db,
            &occurrences,
            &xlsx_path,
            &fields,
            None,
            &cancel,
            |_, _| {},
        )
        .unwrap(),
        4
    );
    let mut workbook = open_workbook_auto(&xlsx_path).unwrap();
    let range = workbook.worksheet_range_at(0).unwrap().unwrap();
    assert_eq!(range.height(), 5);

    let cancelled = AtomicBool::new(false);
    let first_report = db
        .compact_duplicate_payloads(
            &cancelled,
            DuplicateCompactionOptions { batch_rows: 1 },
            |progress| {
                if progress.rows_compacted == 1 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
    assert!(first_report.cancelled);
    assert_eq!(first_report.rows_compacted, 1);
    assert_eq!(db.count(&occurrences).unwrap(), 4);

    cancelled.store(false, Ordering::Relaxed);
    let resumed = db
        .compact_duplicate_payloads(
            &cancelled,
            DuplicateCompactionOptions { batch_rows: 1 },
            |_| {},
        )
        .unwrap();
    assert!(!resumed.cancelled);
    assert_eq!(resumed.rows_compacted, 1);
    assert_eq!(resumed.remaining_legacy_duplicates, 0);
    assert_eq!(db.count(&canonical).unwrap(), 1);
    assert_eq!(db.count(&occurrences).unwrap(), 4);
    assert_eq!(
        db.analytics(&occurrences, 20)
            .unwrap()
            .overview
            .total_value_usd,
        4800.0
    );
    assert_eq!(db.record_card(3).unwrap().source_file, "legacy-a.xlsx");

    db.diagnostic_execute_batch(
        "INSERT INTO records_fts(rowid, search_text) VALUES
             (3, 'Precision hydraulic pump'),
             (4, 'Precision hydraulic pump');",
    )
    .unwrap();
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH 'hydraulic'",
            1,
        )
        .unwrap()[0][0],
        "3"
    );
    let repair = db.repair_fts(&cancelled, |_, _| {}).unwrap();
    assert!(repair.rebuilt);
    assert_eq!(repair.indexed_rows, 1);
    assert_eq!(db.count(&occurrences).unwrap(), 4);
    assert_eq!(
        db.diagnostic_query_rows(
            "SELECT COUNT(*) FROM records_fts WHERE records_fts MATCH 'hydraulic'",
            1,
        )
        .unwrap()[0][0],
        "1"
    );

    assert_eq!(
        record.hash,
        fixture_record("Precision hydraulic pump", 128).hash
    );
}

#[test]
fn compaction_releases_duplicate_payload_pages_without_automatic_vacuum() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("storage.db");
    let mut db = Db::open(&path).unwrap();
    let large = fixture_record(&"payload ".repeat(16_384), 65_536);
    db.insert_batch("canonical.xlsx", &[large]).unwrap();
    for index in 0..48 {
        insert_legacy_full_occurrence(&db, &format!("legacy-{index}.xlsx"));
    }
    db.checkpoint_wal_truncate().unwrap();
    let before = db.storage_info(&path).unwrap();

    let cancel = AtomicBool::new(false);
    let report = db
        .compact_duplicate_payloads(
            &cancel,
            DuplicateCompactionOptions { batch_rows: 8 },
            |_| {},
        )
        .unwrap();
    assert_eq!(report.rows_compacted, 48);
    assert!(report.estimated_payload_bytes_cleared > 5 * 1024 * 1024);
    assert!(report.reclaimed_bytes > 1024 * 1024);
    assert!(!report.vacuum_performed);

    db.checkpoint_wal_truncate().unwrap();
    let before_vacuum = db.storage_info(&path).unwrap();
    assert_eq!(before_vacuum.database_bytes, before.database_bytes);
    db.vacuum_database().unwrap();
    let after_vacuum = db.storage_info(&path).unwrap();
    assert!(
        after_vacuum.database_bytes * 2 < before_vacuum.database_bytes,
        "expected meaningful reduction: before={} after={}",
        before_vacuum.database_bytes,
        after_vacuum.database_bytes
    );
}

#[test]
fn fts_tail_expands_unindexed_canonical_payload_to_thin_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(&dir.path().join("tail.db")).unwrap();
    let cancel = AtomicBool::new(false);

    db.insert_batch(
        "indexed.xlsx",
        &[fixture_record("Already indexed marker", 0)],
    )
    .unwrap();
    db.index_fts(&cancel, |_, _| {}).unwrap();
    db.insert_batch(
        "tail.xlsx",
        &[
            fixture_record("Fresh unindexed payload", 0),
            fixture_record("Fresh unindexed payload", 0),
        ],
    )
    .unwrap();

    let canonical = Query {
        text: "Fresh unindexed".to_string(),
        ..Query::default()
    };
    let occurrences = occurrence_query("Fresh unindexed");
    assert_eq!(db.count(&canonical).unwrap(), 1);
    assert_eq!(db.count(&occurrences).unwrap(), 2);
    assert_eq!(db.search_page(&occurrences, 10, 0).unwrap().0.len(), 2);

    assert_eq!(db.index_fts(&cancel, |_, _| {}).unwrap(), (1, false));
    assert_eq!(db.count(&canonical).unwrap(), 1);
    assert_eq!(db.count(&occurrences).unwrap(), 2);
}

#[test]
#[ignore = "bounded duplicate import benchmark"]
fn duplicate_import_storage_and_speed_benchmark() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benchmark.db");
    let mut db = Db::open(&path).unwrap();
    let records = (0..20_000)
        .map(|_| fixture_record("Repeated benchmark payload", 2048))
        .collect::<Vec<_>>();
    let started = Instant::now();
    let (_, duplicates) = db.insert_batch("benchmark.xlsx", &records).unwrap();
    db.checkpoint_wal_truncate().unwrap();
    let elapsed = started.elapsed();
    let storage = db.storage_info(&path).unwrap();
    assert_eq!(duplicates, 19_999);
    assert!(storage.database_bytes < 8 * 1024 * 1024);
    eprintln!(
        "duplicate benchmark: rows=20000 elapsed_ms={} db_bytes={}",
        elapsed.as_millis(),
        storage.database_bytes
    );
}

#[test]
fn schema_header_lookup_used_by_cards_remains_available() {
    assert!(!schema::header_for("description").is_empty());
}
