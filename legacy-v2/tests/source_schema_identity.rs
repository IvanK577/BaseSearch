use std::sync::atomic::{AtomicBool, Ordering};

use base_search::db::{Db, Query, RecordScope, ResultSort};
use base_search::domain::table::SemanticField;
use base_search::import::{self, ImportOptions, ImportPhase};
use base_search::search::{ConditionOp, ConditionValue, FieldRef, QueryCondition, QueryExpr};
use base_search::{export, schema};
use rusqlite::Connection;

fn write_csv(path: &std::path::Path, rows: &[&str]) {
    std::fs::write(path, rows.join("\n")).unwrap();
}

fn import_csv(db: &mut Db, path: &std::path::Path, options: &ImportOptions) {
    let cancel = AtomicBool::new(false);
    let summary = import::import_file_with_options(db, path, options, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None, "{summary:?}");
    assert!(!summary.cancelled, "{summary:?}");
    assert!(summary.imported > 0, "{summary:?}");
}

fn source_field_query(field_id: &str, op: ConditionOp, value: ConditionValue) -> Query {
    Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::SourceField(field_id.to_string()),
            op,
            value,
            negated: false,
        })),
        record_scope: RecordScope::Occurrences,
        ..Query::default()
    }
}

#[test]
fn schemas_with_same_header_but_different_meaning_never_merge() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("values.csv");
    let second = dir.path().join("quantities.csv");
    write_csv(&first, &["Amount,Label", "10,Alpha"]);
    write_csv(&second, &["Amount,Label", "10,Beta"]);

    let mut db = Db::open(&dir.path().join("identity.db")).unwrap();
    import_csv(
        &mut db,
        &first,
        &ImportOptions::default()
            .with_sheet_semantics("values.csv", [(0, Some(SemanticField::Value))]),
    );
    import_csv(
        &mut db,
        &second,
        &ImportOptions::default()
            .with_sheet_semantics("quantities.csv", [(0, Some(SemanticField::Quantity))]),
    );

    let schemas = db.list_source_schemas().unwrap();
    assert_eq!(schemas.len(), 2);
    assert_ne!(schemas[0].fingerprint, schemas[1].fingerprint);
    let amount_fields = schemas
        .iter()
        .flat_map(|source_schema| source_schema.columns.iter())
        .filter(|field| field.header == "Amount")
        .collect::<Vec<_>>();
    assert_eq!(amount_fields.len(), 2);
    assert_ne!(amount_fields[0].field_id, amount_fields[1].field_id);
    assert_eq!(
        amount_fields
            .iter()
            .map(|field| field.semantic)
            .collect::<std::collections::BTreeSet<_>>(),
        [Some(SemanticField::Value), Some(SemanticField::Quantity),]
            .into_iter()
            .collect()
    );

    let compatibility = db.table_shape().unwrap();
    let compatibility_amounts = compatibility
        .columns
        .iter()
        .filter(|field| field.header == "Amount")
        .collect::<Vec<_>>();
    assert_eq!(compatibility_amounts.len(), 2);
    assert_ne!(compatibility_amounts[0].id, compatibility_amounts[1].id);
}

#[test]
fn ordered_headers_define_schema_and_deduplication_scope() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("a.csv");
    let reordered = dir.path().join("b.csv");
    write_csv(
        &first,
        &[
            "Description,Product code",
            "Same payload,8517130000",
            "Same payload,8517130000",
        ],
    );
    write_csv(
        &reordered,
        &["Product code,Description", "8517130000,Same payload"],
    );

    let mut db = Db::open(&dir.path().join("dedupe.db")).unwrap();
    import_csv(&mut db, &first, &ImportOptions::default());
    import_csv(&mut db, &reordered, &ImportOptions::default());

    let schemas = db.list_source_schemas().unwrap();
    assert_eq!(schemas.len(), 2);
    assert_ne!(schemas[0].fingerprint, schemas[1].fingerprint);
    let physical = db
        .diagnostic_query_rows(
            "SELECT schema_id, canonical_id, row_hash FROM records ORDER BY id",
            10,
        )
        .unwrap();
    assert_eq!(physical.len(), 3);
    assert_eq!(physical[0][0], physical[1][0]);
    assert_ne!(physical[0][0], physical[2][0]);
    assert_eq!(physical[0][2], physical[2][2]);
    assert_eq!(physical[1][1], "1");
    assert_eq!(physical[2][1], "NULL");
}

#[test]
fn failed_import_rolls_back_source_schema_and_rows() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cancel.csv");
    let mut csv = String::from("Description,Product code\n");
    for index in 0..8_300 {
        csv.push_str(&format!("Row {index},8517{index:06}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    let mut db = Db::open(&dir.path().join("rollback.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file_with_options(
        &mut db,
        &input,
        &ImportOptions::default(),
        &cancel,
        &mut |phase, _, _| {
            if phase == ImportPhase::Inserting {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(summary.cancelled, "{summary:?}");
    assert_eq!(db.total_rows(), 0);
    assert!(db.list_source_schemas().unwrap().is_empty());
    assert!(db.list_import_sources().unwrap().is_empty());
}

#[test]
fn schema_and_source_ids_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("restart.db");
    let input = dir.path().join("restart.csv");
    write_csv(&input, &["Description,Inventory", "Restart row,R-001"]);

    let (schema_id, source_id, field_ids) = {
        let mut db = Db::open(&db_path).unwrap();
        import_csv(&mut db, &input, &ImportOptions::default());
        let source_schema = db.list_source_schemas().unwrap().remove(0);
        let source = db.list_import_sources().unwrap().remove(0);
        (
            source_schema.public_id,
            source.public_id,
            source_schema
                .columns
                .into_iter()
                .map(|field| field.field_id)
                .collect::<Vec<_>>(),
        )
    };

    let db = Db::open(&db_path).unwrap();
    assert_eq!(
        db.get_source_schema(&schema_id)
            .unwrap()
            .unwrap()
            .columns
            .into_iter()
            .map(|field| field.field_id)
            .collect::<Vec<_>>(),
        field_ids
    );
    assert_eq!(
        db.get_import_source(&source_id).unwrap().unwrap().public_id,
        source_id
    );
}

#[test]
fn opening_v1_rows_adds_nullable_identity_without_backfill() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let mut db = Db::open(&path).unwrap();
        let values = vec![String::new(); schema::COLUMNS.len()];
        let record = base_search::db::ImportRecord {
            hash: base_search::db::canonical_record_hash(&values, None),
            year: None,
            values,
            extra: None,
        };
        db.insert_batch("legacy.xlsx", &[record]).unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    // SQLite refuses to drop a column referenced by a partial index or a
    // trigger, so the v1 simulation removes the dependent identity indexes
    // and triggers first.
    connection
        .execute_batch(
            "DROP INDEX IF EXISTS idx_records_source_id;
             DROP INDEX IF EXISTS idx_records_schema_hash_owner;
             DROP INDEX IF EXISTS idx_records_legacy_schema;
             DROP TRIGGER IF EXISTS records_canonical_schema_insert;
             DROP TRIGGER IF EXISTS records_canonical_schema_update;",
        )
        .unwrap();
    for column in ["source_id", "schema_id"] {
        let exists: bool = connection
            .prepare("PRAGMA table_info(records)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .any(|name| name == column);
        if exists {
            connection
                .execute_batch(&format!("ALTER TABLE records DROP COLUMN {column};"))
                .unwrap();
        }
    }
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('records_schema', '6')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_legacy_row_update
             BEFORE UPDATE ON records
             BEGIN
                 SELECT RAISE(ABORT, 'legacy rows must not be rewritten');
             END;",
        )
        .unwrap();
    drop(connection);

    let db = Db::open(&path).unwrap();
    let identity = db
        .diagnostic_query_rows("SELECT schema_id, source_id FROM records", 1)
        .unwrap();
    assert_eq!(identity, vec![vec!["NULL".to_string(), "NULL".to_string()]]);
    assert!(db.list_source_schemas().unwrap().is_empty());
}

#[test]
fn source_field_query_sort_card_and_export_are_schema_exact() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("money.csv");
    let second = dir.path().join("stock.csv");
    write_csv(&first, &["Amount,Label", "10,Ten", "2,Two"]);
    write_csv(&second, &["Amount,Label", "999,Stock"]);
    let mut db = Db::open(&dir.path().join("source-field.db")).unwrap();
    import_csv(
        &mut db,
        &first,
        &ImportOptions::default()
            .with_sheet_semantics("money.csv", [(0, Some(SemanticField::Value))]),
    );
    import_csv(
        &mut db,
        &second,
        &ImportOptions::default()
            .with_sheet_semantics("stock.csv", [(0, Some(SemanticField::Quantity))]),
    );

    let money_schema = db
        .list_source_schemas()
        .unwrap()
        .into_iter()
        .find(|source_schema| {
            source_schema
                .columns
                .iter()
                .any(|field| field.semantic == Some(SemanticField::Value))
        })
        .unwrap();
    let amount = money_schema
        .columns
        .iter()
        .find(|field| field.header == "Amount")
        .unwrap();
    let query = source_field_query(
        &amount.field_id,
        ConditionOp::Range,
        ConditionValue::Range {
            from: Some("0".to_string()),
            to: Some("100".to_string()),
        },
    );
    assert_eq!(db.count(&query).unwrap(), 2);

    let (fields, ids, rows, _) = db
        .search_page_dynamic_sorted(
            &query,
            10,
            0,
            Some(ResultSort {
                field: amount.field_id.clone(),
                descending: false,
            }),
        )
        .unwrap();
    let amount_index = fields
        .iter()
        .position(|field| field.id == amount.field_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row[amount_index].as_str())
            .collect::<Vec<_>>(),
        vec!["2", "10"]
    );
    let card = db.record_card(ids[0]).unwrap();
    assert!(
        card.fields
            .iter()
            .any(|(label, value)| label == "Amount" && value == "2")
    );
    assert_eq!(
        card.fields
            .iter()
            .filter(|(label, _)| label == "Amount")
            .count(),
        1
    );

    let selected = fields
        .into_iter()
        .filter(|field| field.id == amount.field_id)
        .collect::<Vec<_>>();
    let csv_path = dir.path().join("source-field.csv");
    export::export_selected(
        &db,
        &query,
        &csv_path,
        &selected,
        Some(&ResultSort {
            field: amount.field_id.clone(),
            descending: false,
        }),
        &AtomicBool::new(false),
        |_, _| {},
    )
    .unwrap();
    let bytes = std::fs::read(&csv_path).unwrap();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(&bytes[3..]);
    assert_eq!(
        reader
            .records()
            .map(|row| row.unwrap().get(0).unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["2", "10"]
    );
}

#[test]
fn duplicate_headers_manual_semantics_and_fixed_context_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("profile.csv");
    write_csv(&input, &["Amount,Amount,Name", "10,20,Example"]);
    let options = ImportOptions::default()
        .with_sheet_semantics(
            "profile.csv",
            [
                (0, Some(SemanticField::Value)),
                (1, Some(SemanticField::Quantity)),
            ],
        )
        .with_sheet_fixed_values(
            "profile.csv",
            [
                (SemanticField::Currency, "EUR"),
                (SemanticField::WeightUnit, "kg"),
            ],
        );
    let mut db = Db::open(&dir.path().join("profile.db")).unwrap();
    import_csv(&mut db, &input, &options);

    let source_schema = db.list_source_schemas().unwrap().remove(0);
    assert_eq!(source_schema.fixed_currency.as_deref(), Some("EUR"));
    assert_eq!(source_schema.fixed_weight_unit.as_deref(), Some("kg"));
    assert_eq!(source_schema.columns.len(), 3);
    assert_ne!(
        source_schema.columns[0].field_id,
        source_schema.columns[1].field_id
    );
    assert_eq!(source_schema.columns[0].source_index, 0);
    assert_eq!(source_schema.columns[1].source_index, 1);
    assert_eq!(
        source_schema.columns[0].semantic,
        Some(SemanticField::Value)
    );
    assert_eq!(
        source_schema.columns[1].semantic,
        Some(SemanticField::Quantity)
    );
}

#[test]
fn plain_numeric_query_remains_global_fts_for_arbitrary_fields() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("inventory.csv");
    write_csv(
        &input,
        &["Inventory reference,Name", "8517-ONLY-IN-INVENTORY,Router"],
    );
    let mut db = Db::open(&dir.path().join("numeric.db")).unwrap();
    import_csv(&mut db, &input, &ImportOptions::default());

    let query = Query {
        text: "8517".to_string(),
        ..Query::default()
    };
    assert_eq!(db.count(&query).unwrap(), 1);
}
