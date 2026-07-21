use std::sync::atomic::AtomicBool;

use base_search::db::{Db, Query};
use base_search::domain::table::{ColumnStorage, SemanticField};
use base_search::import;
use base_search::schema::{RESULT_COLUMNS, col_index};

fn result_col(name: &str) -> usize {
    RESULT_COLUMNS
        .iter()
        .position(|column| *column == name)
        .unwrap_or_else(|| panic!("missing result column {name}"))
}

fn import_xlsx(path: &std::path::Path, headers: &[&str], rows: &[Vec<&str>]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (column, header) in headers.iter().enumerate() {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    for (row_index, row) in rows.iter().enumerate() {
        for (column, value) in row.iter().enumerate() {
            sheet
                .write_string((row_index + 1) as u32, column as u16, *value)
                .unwrap();
        }
    }
    workbook.save(path).unwrap();
}

#[test]
fn arbitrary_csv_without_known_semantics_previews_and_imports_every_column() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("measurements.csv");
    std::fs::write(
        &source,
        "Laboratory export;;\nNebula key;Flux coefficient;Operator note\nN-01;12.5;stable\n",
    )
    .unwrap();

    let preview = import::peek_file(&source, 5).unwrap();
    let sheet = &preview.sheets[0];
    assert_eq!(sheet.header_row, 2);
    assert_eq!(sheet.layout, "generic table");
    assert!(sheet.columns.iter().all(|column| column.semantic.is_none()));

    let mut db = Db::open(&dir.path().join("measurements.db")).unwrap();
    let summary = import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);
    assert_eq!(summary.quality.source_columns, 3);
    assert_eq!(summary.quality.recognized_columns, 0);

    let shape = db.table_shape().unwrap();
    assert_eq!(shape.columns.len(), 3);
    assert!(shape.columns.iter().all(|column| {
        column.semantic.is_none() && column.storage == ColumnStorage::SourceJson
    }));
    let (_, ids, _, _) = db.search_page_dynamic(&Query::default(), 10, 0).unwrap();
    let card = db.record_card(ids[0]).unwrap();
    assert_eq!(
        card.fields,
        vec![
            ("Nebula key".to_string(), "N-01".to_string()),
            ("Flux coefficient".to_string(), "12.5".to_string()),
            ("Operator note".to_string(), "stable".to_string()),
        ]
    );
}

#[test]
fn multilingual_aliases_map_reordered_columns_without_a_document_profile() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("reordered.xlsx");
    import_xlsx(
        &source,
        &[
            "Сума рахунку",
            "Артикул товару",
            "Назва покупця",
            "Дата документа",
            "Опис позиції",
            "Валюта",
            "Одиниця ваги",
        ],
        &[vec![
            "1250.50",
            "SKU-42",
            "ТОВ Приклад",
            "2024-03-15",
            "Промисловий контролер",
            "USD",
            "kg",
        ]],
    );

    let preview = import::peek_file(&source, 5).unwrap();
    let semantics = preview.sheets[0]
        .columns
        .iter()
        .map(|column| column.semantic)
        .collect::<Vec<_>>();
    assert_eq!(
        semantics,
        vec![
            Some(SemanticField::Value),
            Some(SemanticField::ProductCode),
            Some(SemanticField::Recipient),
            Some(SemanticField::Date),
            Some(SemanticField::Description),
            Some(SemanticField::Currency),
            Some(SemanticField::WeightUnit),
        ]
    );

    let mut db = Db::open(&dir.path().join("reordered.db")).unwrap();
    let summary = import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.quality.recognized_columns, 5);

    let (_, rows, _) = db.search_page(&Query::default(), 10, 0).unwrap();
    assert_eq!(rows[0][result_col("currency_control_value")], "1250.50");
    assert_eq!(rows[0][result_col("product_code")], "SKU-42");
    assert_eq!(rows[0][result_col("recipient")], "ТОВ Приклад");
    assert_eq!(rows[0][result_col("declaration_date")], "2024-03-15");
    assert_eq!(rows[0][result_col("description")], "Промисловий контролер");
}

#[test]
fn repeated_headers_use_samples_conservatively_and_keep_raw_parts_visible() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("repeated.xlsx");
    import_xlsx(
        &source,
        &[
            "Одержувач",
            "Одержувач",
            "Номер декларації",
            "Номер декларації",
            "Номер декларації",
            "Код товару",
            "Опис товару",
        ],
        &[
            vec![
                "37642136",
                "ТОВ Приклад",
                "UA209230",
                "2024",
                "102880",
                "9405500000",
                "Освітлювальні прилади",
            ],
            vec![
                "32818783",
                "ТОВ Інший",
                "UA209060",
                "2024",
                "1479",
                "7005103000",
                "Скло листове",
            ],
        ],
    );

    let preview = import::peek_file(&source, 5).unwrap();
    assert_eq!(preview.sheets[0].columns.len(), 7);
    assert_eq!(
        preview.sheets[0].columns[0].semantic,
        Some(SemanticField::CompanyCode)
    );
    assert_eq!(
        preview.sheets[0].columns[1].semantic,
        Some(SemanticField::Recipient)
    );

    let mut db = Db::open(&dir.path().join("repeated.db")).unwrap();
    let summary = import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);

    let (_, rows, _) = db.search_page(&Query::default(), 10, 0).unwrap();
    assert_eq!(rows[0][result_col("edrpou")], "32818783");
    assert_eq!(rows[0][result_col("recipient")], "ТОВ Інший");
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA209060/2024/1479"
    );

    let fields = db.result_fields().unwrap();
    let declaration_fields = fields
        .iter()
        .filter(|field| field.label.starts_with("Номер декларації"))
        .count();
    assert_eq!(declaration_fields, 3);
    let (fields, _, dynamic_rows, _) = db.search_page_dynamic(&Query::default(), 10, 0).unwrap();
    let values = fields
        .iter()
        .zip(&dynamic_rows[0])
        .filter(|(field, _)| field.label.starts_with("Номер декларації"))
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(values, vec!["UA209060", "2024", "1479"]);
}

#[test]
fn manual_mapping_wins_over_automatic_alias_inference() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("manual-wins.xlsx");
    import_xlsx(
        &source,
        &["Product code", "Description"],
        &[vec!["Not a code", "SKU-OVERRIDE"]],
    );
    let options = import::ImportOptions::selected_sheets(["Sheet1"]).with_sheet_semantics(
        "Sheet1",
        [
            (0, Some(SemanticField::Description)),
            (1, Some(SemanticField::ProductCode)),
        ],
    );
    let mut db = Db::open(&dir.path().join("manual-wins.db")).unwrap();
    let summary = import::import_file_with_options(
        &mut db,
        &source,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert_eq!(summary.error, None);
    let (_, rows, _) = db.search_page(&Query::default(), 10, 0).unwrap();
    assert_eq!(rows[0][col_index("description").unwrap()], "Not a code");
    assert_eq!(rows[0][col_index("product_code").unwrap()], "SKU-OVERRIDE");
}

#[test]
fn fixed_currency_and_weight_unit_are_materialized_without_source_columns() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixed-values.xlsx");
    import_xlsx(
        &source,
        &["Item", "Amount", "Net weight"],
        &[vec!["Industrial controller", "1250.50", "10"]],
    );
    let options = import::ImportOptions::selected_sheets(["Sheet1"]).with_sheet_fixed_values(
        "Sheet1",
        [
            (SemanticField::Currency, " USD "),
            (SemanticField::WeightUnit, "kg"),
        ],
    );
    let mut db = Db::open(&dir.path().join("fixed-values.db")).unwrap();
    let summary = import::import_file_with_options(
        &mut db,
        &source,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let shape = db.table_shape().unwrap();
    assert_eq!(shape.columns.len(), 3);
    assert_eq!(
        shape
            .columns
            .iter()
            .map(|column| column.header.as_str())
            .collect::<Vec<_>>(),
        vec!["Item", "Amount", "Net weight"]
    );
    let (_, rows, _) = db.search_page(&Query::default(), 10, 0).unwrap();
    assert_eq!(rows[0][col_index("contract").unwrap()], "USD");
    assert_eq!(rows[0][col_index("unit").unwrap()], "kg");
}

#[test]
fn fixed_values_reject_non_context_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("invalid-fixed.csv");
    std::fs::write(&source, "Name\nExample\n").unwrap();
    let options = import::ImportOptions::default()
        .with_sheet_fixed_values("invalid-fixed.csv", [(SemanticField::ProductCode, "SKU-1")]);
    let mut db = Db::open(&dir.path().join("invalid-fixed.db")).unwrap();
    let summary = import::import_file_with_options(
        &mut db,
        &source,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert!(
        summary
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("Currency or WeightUnit") })
    );
    assert_eq!(db.total_rows(), 0);
}
