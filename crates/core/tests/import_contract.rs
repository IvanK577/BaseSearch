use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use base_search_core::{
    Operation,
    import::{ImportFileRequest, ImportSheetRequest, import_files, preview_file},
    model::Role,
    workspace::Workspace,
};
use rusqlite::Connection;
use rust_xlsxwriter::Workbook;
use serde_json::Value;

fn request(path: &Path) -> ImportFileRequest {
    ImportFileRequest {
        path: path.to_str().unwrap().into(),
        sheets: preview_file(path)
            .unwrap()
            .sheets
            .into_iter()
            .filter(|sheet| !sheet.headers.is_empty())
            .map(|sheet| ImportSheetRequest {
                key: sheet.key,
                headers: sheet.headers,
                roles: sheet.suggested_roles,
                fixed_currency: None,
                fixed_weight_unit: None,
            })
            .collect(),
    }
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn stored_cell(conn: &Connection, row_id: i64, ordinal: i64) -> Value {
    let (raw, key): (String, String) = conn
        .query_row(
            "SELECT r.raw, c.column_id FROM records r JOIN source_columns c
             ON c.source_id = r.source_id WHERE r.id = ?1 AND c.ordinal = ?2",
            [row_id, ordinal],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    serde_json::from_str::<Value>(&raw).unwrap()[key].clone()
}

fn run_import(
    workspace: &Workspace,
    files: &[ImportFileRequest],
) -> base_search_core::import::ImportReport {
    import_files(
        workspace,
        files,
        &Operation::new(&AtomicBool::new(false), &|_| {}),
    )
    .unwrap()
}

#[test]
fn previews_roles_and_preserves_csv_strings_and_missing_cells() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.csv");
    fs::write(
        &path,
        "Date,Company,SKU,SKU\n2025-02-03,  Orion  ,0007,0008\n2025-02-04,\"Line\nbreak\",0009\n",
    )
    .unwrap();
    let preview = preview_file(&path).unwrap();
    let sheet = &preview.sheets[0];
    assert_eq!(sheet.headers, ["Date", "Company", "SKU", "SKU"]);
    assert_eq!(
        sheet.suggested_roles,
        [
            Some(Role::Date),
            Some(Role::Company),
            Some(Role::Product),
            None
        ]
    );
    assert_eq!(
        sheet.rows[1],
        [
            Some("2025-02-04".into()),
            Some("Line\nbreak".into()),
            Some("0009".into()),
            None
        ]
    );

    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let mut request = request(&path);
    request.sheets[0].roles = vec![Some(Role::Date), None, None, Some(Role::Product)];
    let report = run_import(&workspace, &[request]);
    assert_eq!(report.inserted_rows, 2);
    let conn = workspace.connect().unwrap();
    assert_eq!(stored_cell(&conn, 1, 1), "  Orion  ");
    assert_eq!(stored_cell(&conn, 1, 2), "0007");
    assert_eq!(stored_cell(&conn, 1, 3), "0008");
    assert_eq!(stored_cell(&conn, 2, 3), Value::Null);
    let (product, row_number): (String, i64) = conn
        .query_row(
            "SELECT product, row_number FROM records WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((product.as_str(), row_number), ("0008", 2));
}

#[test]
fn same_width_header_reorder_requires_a_fresh_preview() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reordered.csv");
    fs::write(&path, "Product,Company\nWidget,Orion\n").unwrap();
    let stale_request = request(&path);
    fs::write(&path, "Company,Product\nOrion,Widget\n").unwrap();

    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let report = run_import(&workspace, &[stale_request]);
    assert_eq!(report.inserted_rows, 0);
    let error = report.files[0].error.as_deref().unwrap();
    assert!(error.contains("предварительный просмотр"), "{error}");
    let conn = workspace.connect().unwrap();
    assert_eq!(count(&conn, "sources"), 0);
    assert_eq!(count(&conn, "records"), 0);

    assert_eq!(run_import(&workspace, &[request(&path)]).inserted_rows, 1);
    let saved: (String, String) = conn
        .query_row("SELECT company, product FROM records", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(saved, ("Orion".into(), "Widget".into()));
}

#[test]
fn customs_preview_prefers_explicit_net_weight_and_excludes_gross() {
    let dir = tempfile::tempdir().unwrap();
    let both = dir.path().join("customs.csv");
    for net_header in ["Нетто кг", "Вага нетто", "Net Weight"] {
        fs::write(&both, format!("Брутто кг,Вага,{net_header}\n120,100,95\n")).unwrap();
        let preview = preview_file(&both).unwrap();
        assert_eq!(
            preview.sheets[0].suggested_roles,
            [None, None, Some(Role::Weight)]
        );
    }

    let gross_only = dir.path().join("gross.csv");
    fs::write(&gross_only, "Gross Weight (kg),Брутто кг\n120,120\n").unwrap();
    let preview = preview_file(&gross_only).unwrap();
    assert_eq!(preview.sheets[0].suggested_roles, [None, None]);
}

#[test]
fn xlsx_selection_preserves_sheet_coordinates_and_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.xlsx");
    let mut book = Workbook::new();
    let cover = book.add_worksheet().set_name("Cover").unwrap();
    cover.write_string(0, 0, "Title").unwrap();
    cover.write_string(1, 0, "Ignored").unwrap();
    let data = book.add_worksheet().set_name("Selected").unwrap();
    data.write_string(2, 1, "Product").unwrap();
    data.write_string(2, 3, "Amount").unwrap();
    data.write_string(5, 1, "00019").unwrap();
    data.write_number(5, 3, 13.75).unwrap();
    book.save(&path).unwrap();

    let preview = preview_file(&path).unwrap();
    assert_eq!(preview.sheets.len(), 2);
    assert_eq!(preview.sheets[1].name, "Selected");
    assert_eq!(preview.sheets[1].headers, ["Product", "", "Amount"]);
    let mut request = request(&path);
    request.sheets.remove(0);
    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let report = run_import(&workspace, &[request]);
    assert_eq!(report.inserted_rows, 1);
    let conn = workspace.connect().unwrap();
    let (sheet, row): (String, i64) = conn
        .query_row(
            "SELECT sheet, row_number FROM sources JOIN records ON sources.id = records.source_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((sheet.as_str(), row), ("Selected", 6));
    assert_eq!(stored_cell(&conn, 1, 0), "00019");
    assert_eq!(stored_cell(&conn, 1, 1), Value::Null);
    assert_eq!(stored_cell(&conn, 1, 2), "13.75");
}

#[test]
fn skips_the_same_file_and_sheet_but_keeps_identical_rows_in_distinct_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.csv");
    let second = dir.path().join("second.csv");
    fs::write(&first, "Product\n0007\n").unwrap();
    fs::write(&second, "Product\n0007\n0008\n").unwrap();
    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let report = run_import(
        &workspace,
        &[request(&first), request(&first), request(&second)],
    );
    assert_eq!(report.inserted_rows, 3);
    assert_eq!(report.skipped_sheets, 1);
    assert_eq!(report.files[1].skipped_sheets, 1);
    assert!(report.files[1].error.is_none());
    let conn = workspace.connect().unwrap();
    assert_eq!(count(&conn, "sources"), 2);
    assert_eq!(stored_cell(&conn, 1, 0), "0007");
    assert_eq!(stored_cell(&conn, 2, 0), "0007");
}

#[test]
fn failed_file_rolls_back_rows_and_source_and_batch_continues() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.csv");
    let valid = dir.path().join("valid.csv");
    let mut raw = String::from("Product\n");
    for number in 1..1_001 {
        raw.push_str(&format!("{number}\n"));
    }
    let mut raw = raw.into_bytes();
    raw.extend_from_slice(b"\xff\n");
    fs::write(&broken, raw).unwrap();
    fs::write(&valid, "Product\n0007\n").unwrap();
    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let report = run_import(&workspace, &[request(&broken), request(&valid)]);
    assert!(report.files[0].error.is_some());
    assert_eq!(report.files[0].inserted_rows, 0);
    assert_eq!(report.inserted_rows, 1);
    let conn = workspace.connect().unwrap();
    assert_eq!(count(&conn, "sources"), 1);
    assert_eq!(count(&conn, "records"), 1);
    assert_eq!(stored_cell(&conn, 1, 0), "0007");
}

#[test]
fn cancellation_keeps_only_previously_committed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.csv");
    let mut raw = String::from("Product\n");
    for number in 1..15_001 {
        raw.push_str(&format!("{number}\n"));
    }
    fs::write(&path, raw).unwrap();
    let first = dir.path().join("first.csv");
    fs::write(&first, "Product\nprior\n").unwrap();

    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let progress = |report: base_search_core::Progress| {
        if report.processed >= 1_000 && report.message.contains("interrupted.csv") {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let result = import_files(
        &workspace,
        &[request(&first), request(&path)],
        &Operation::new(&cancel, &progress),
    );
    assert!(matches!(result, Err(base_search_core::Error::Cancelled)));
    let conn = workspace.connect().unwrap();
    assert_eq!(count(&conn, "sources"), 1);
    assert_eq!(count(&conn, "records"), 1);
    assert_eq!(stored_cell(&conn, 1, 0), "prior");
}

#[test]
fn delimited_records_keep_explicit_empty_fields_and_trailing_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.tsv");
    fs::write(&path, "Product\tValue\tComment\n\n\t\t\n0007\t\n").unwrap();
    let workspace = Workspace::open(dir.path().join("new.db")).unwrap();
    let report = run_import(&workspace, &[request(&path)]);
    assert_eq!(report.inserted_rows, 2);
    let conn = workspace.connect().unwrap();
    assert_eq!(stored_cell(&conn, 1, 0), "");
    assert_eq!(stored_cell(&conn, 1, 2), "");
    assert_eq!(stored_cell(&conn, 2, 0), "0007");
    assert_eq!(stored_cell(&conn, 2, 1), "");
    assert_eq!(stored_cell(&conn, 2, 2), Value::Null);
}
