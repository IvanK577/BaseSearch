use base_search_core::model::{
    Dimension, FilterOp, IncomingColumn, NewSource, QueryFilter, QuerySort, QuerySpec, Role,
    SortDirection,
};
use base_search_core::query::{record, search};
use base_search_core::summary::summarize;
use base_search_core::workspace::{RecordWriter, Workspace, add_source, publish};
use base_search_core::{Error, Operation};
use calamine::{Data, Reader, open_workbook_auto};
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

fn load(workspace: &Workspace, name: &str, rows: &[&[&str]]) {
    let source = NewSource {
        file_name: name.into(),
        sheet: "Лист 1".into(),
        fingerprint: name.into(),
        columns: [
            ("Date", Some(Role::Date)),
            ("Company", Some(Role::Company)),
            ("Product", Some(Role::Product)),
            ("Value", Some(Role::Value)),
            ("Currency", Some(Role::Currency)),
            ("Weight", Some(Role::Weight)),
            ("Unit", Some(Role::WeightUnit)),
            ("Custom", None),
        ]
        .into_iter()
        .map(|(name, role)| IncomingColumn {
            name: name.into(),
            role,
        })
        .collect(),
        fixed_currency: None,
        fixed_weight_unit: None,
    };
    load_source(workspace, &source, rows);
}

fn load_source(workspace: &Workspace, source: &NewSource, rows: &[&[&str]]) {
    let mut conn = workspace.connect().unwrap();
    let tx = conn.transaction().unwrap();
    let id = add_source(&tx, source).unwrap().unwrap();
    let mut writer = RecordWriter::new(&tx).unwrap();
    for (i, row) in rows.iter().enumerate() {
        let values: Vec<_> = row.iter().map(|value| Some(value.to_string())).collect();
        writer
            .insert(id, source, Some(i as u64 + 2), &values)
            .unwrap();
    }
    writer.finish().unwrap();
    publish(&tx).unwrap();
    tx.commit().unwrap();
}

fn sample() -> (TempDir, Workspace) {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(directory.path().join("v1.sqlite")).unwrap();
    load(
        &workspace,
        "sample.csv",
        &[
            &[
                "2025-01-13",
                "Київ",
                "00101",
                "0.1",
                "USD",
                "1.25",
                "т",
                "рідкісний   001",
            ],
            &[
                "2025-01-14",
                "КИЇВ",
                "00102",
                "0,2",
                "USD",
                "250",
                "g",
                "спільний",
            ],
            &[
                "2025-03-01",
                "Львів",
                "00201",
                "10",
                "EUR",
                "9",
                "m³",
                "рідкісний",
            ],
            &[
                "2025-04-01",
                "",
                "00202",
                "9007199254740993.03",
                "",
                "4",
                "",
                "=1+1",
            ],
            &[
                "2025-04-02",
                "Львів",
                "00203",
                "9",
                "EUR",
                "1",
                "kg",
                "спільний",
            ],
        ],
    );
    (directory, workspace)
}

#[test]
fn fts_includes_custom_columns_and_preserves_raw_provenance() {
    let (_directory, workspace) = sample();
    let info = workspace.info().unwrap();
    let custom = &info
        .columns
        .iter()
        .find(|column| column.label == "Custom")
        .unwrap()
        .id;
    let query = QuerySpec {
        text: "РІДКІСНИЙ 001".into(),
        revision: Some(info.revision),
        ..QuerySpec::default()
    };
    let page = search(&workspace, &query, 0, 50).unwrap();
    assert_eq!(page.total_rows, 1);
    assert_eq!(
        page.rows[0].values[custom].as_deref(),
        Some("рідкісний   001")
    );
    let detail = record(&workspace, page.rows[0].id, Some(page.revision))
        .unwrap()
        .unwrap();
    assert_eq!(detail.source_name, "sample.csv");
    assert_eq!(detail.sheet, "Лист 1");
    assert_eq!(detail.row_number, Some(2));
    assert_eq!(detail.values.len(), 8);
}

#[test]
fn detail_returns_source_header_order_spelling_and_duplicate_columns() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(directory.path().join("headers.sqlite")).unwrap();
    for (file, headers, values) in [
        (
            "first.csv",
            ["Company", "Memo", "Memo"],
            ["first", "a", "b"],
        ),
        (
            "second.csv",
            ["memo", "MEMO", "company"],
            ["c", "d", "second"],
        ),
    ] {
        let source = NewSource {
            file_name: file.into(),
            sheet: "Sheet".into(),
            fingerprint: file.into(),
            columns: headers
                .into_iter()
                .map(|header| IncomingColumn {
                    name: header.into(),
                    role: header
                        .eq_ignore_ascii_case("company")
                        .then_some(Role::Company),
                })
                .collect(),
            fixed_currency: None,
            fixed_weight_unit: None,
        };
        load_source(&workspace, &source, &[&values]);
    }
    let page = search(&workspace, &QuerySpec::default(), 1, 1).unwrap();
    let row = &page.rows[0];
    assert!(
        serde_json::to_value(row)
            .unwrap()
            .get("sourceColumns")
            .is_none()
    );
    let detail = record(&workspace, row.id, Some(page.revision))
        .unwrap()
        .unwrap();
    let labels: Vec<_> = detail
        .source_columns
        .iter()
        .map(|column| column.label.as_str())
        .collect();
    assert_eq!(labels, ["memo", "MEMO", "company"]);
    let values: Vec<_> = detail
        .source_columns
        .iter()
        .map(|column| detail.values[&column.id].as_deref().unwrap())
        .collect();
    assert_eq!(values, ["c", "d", "second"]);
    assert_eq!(
        serde_json::to_value(&detail).unwrap()["sourceColumns"][2]["role"],
        "company"
    );
}

#[test]
fn search_rejects_stale_revision_after_import() {
    let (_directory, workspace) = sample();
    let first = search(&workspace, &QuerySpec::default(), 0, 50).unwrap();
    load(
        &workspace,
        "second.csv",
        &[&["2025-01-13", "Київ", "003", "2", "EUR", "1", "kg", ""]],
    );
    let query = QuerySpec {
        revision: Some(first.revision),
        ..QuerySpec::default()
    };
    assert!(matches!(
        search(&workspace, &query, 0, 50),
        Err(Error::StaleRevision { .. })
    ));
    assert_eq!(workspace.info().unwrap().row_count, 6);
}

#[test]
fn reopening_v1_keeps_existing_counts_and_updates_interleaved_source_counts() {
    let (_directory, workspace) = sample();
    let before = workspace.info().unwrap();
    let conn = workspace.connect().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS count_source_rows AFTER INSERT ON records BEGIN
             UPDATE sources SET rows = rows + 1 WHERE id = new.source_id;
         END;
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let workspace = Workspace::open(workspace.path()).unwrap();
    assert_eq!(workspace.info().unwrap(), before);
    let mut conn = workspace.connect().unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let legacy_trigger: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'count_source_rows')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!legacy_trigger);

    let tx = conn.transaction().unwrap();
    let source = |name: &str| NewSource {
        file_name: name.into(),
        sheet: String::new(),
        fingerprint: name.into(),
        columns: vec![IncomingColumn {
            name: "Message".into(),
            role: None,
        }],
        fixed_currency: None,
        fixed_weight_unit: None,
    };
    let first = source("first.csv");
    let second = source("second.csv");
    let first_id = add_source(&tx, &first).unwrap().unwrap();
    let second_id = add_source(&tx, &second).unwrap().unwrap();
    let mut writer = RecordWriter::new(&tx).unwrap();
    for (id, source) in [(first_id, &first), (second_id, &second), (first_id, &first)] {
        writer
            .insert(id, source, None, &[Some("append".into())])
            .unwrap();
    }
    writer.finish().unwrap();
    publish(&tx).unwrap();
    tx.commit().unwrap();

    let after = workspace.info().unwrap();
    assert_eq!(after.row_count, 8);
    assert_eq!(
        after
            .sources
            .iter()
            .map(|source| source.rows)
            .collect::<Vec<_>>(),
        [5, 2, 1]
    );
    for (text, expected) in [("рідкісний", 2), ("append", 3)] {
        let query = QuerySpec {
            text: text.into(),
            ..QuerySpec::default()
        };
        assert_eq!(
            search(&workspace, &query, 0, 50).unwrap().total_rows,
            expected
        );
    }
}

#[test]
fn monthly_summary_uses_exact_totals_and_previous_calendar_month() {
    let (_directory, workspace) = sample();
    let query = QuerySpec::default();
    let summary = summarize(&workspace, &query, Dimension::Month).unwrap();
    assert_eq!(summary.total_rows, 5);
    assert_eq!(summary.net_weight_kg.as_deref(), Some("1251.25"));
    assert_eq!(summary.unknown_unit_rows, 2);
    assert!(
        summary
            .currency_totals
            .iter()
            .any(|item| { item.currency.as_deref() == Some("USD") && item.amount == "0.3" })
    );
    assert!(
        summary
            .currency_totals
            .iter()
            .any(|item| { item.currency.is_none() && item.amount == "9007199254740993.03" })
    );
    let march = summary
        .groups
        .iter()
        .find(|group| group.key == "2025-03")
        .unwrap();
    assert_eq!(march.change_percent, None);
    let april = summary
        .groups
        .iter()
        .find(|group| group.key == "2025-04")
        .unwrap();
    assert_eq!(april.change_percent, Some(100.0));
    assert_eq!(april.share_percent, 40.0);
    let filtered = QuerySpec {
        filters: vec![april.filter.clone()],
        ..QuerySpec::default()
    };
    assert_eq!(search(&workspace, &filtered, 0, 50).unwrap().total_rows, 2);
}

#[test]
fn csv_exports_entire_matching_scope_in_decimal_sort_order() {
    let (directory, workspace) = sample();
    let info = workspace.info().unwrap();
    let product = &info
        .columns
        .iter()
        .find(|column| column.label == "Product")
        .unwrap()
        .id;
    let query = QuerySpec {
        filters: vec![QueryFilter {
            field: "@value".into(),
            op: FilterOp::From,
            value: "9".into(),
        }],
        sort: Some(QuerySort {
            field: "@value".into(),
            direction: SortDirection::Asc,
        }),
        revision: Some(info.revision),
        ..QuerySpec::default()
    };
    let first = search(&workspace, &query, 0, 1).unwrap();
    assert_eq!(first.total_rows, 3);
    assert_eq!(first.rows[0].values[product].as_deref(), Some("00203"));
    let output = directory.path().join("view.csv");
    let count = base_search_core::export::export(
        &workspace,
        &query,
        &output,
        Some(std::slice::from_ref(product)),
        &Operation::new(&AtomicBool::new(false), &|_| {}),
    )
    .unwrap();
    assert_eq!(count, 3);
    let mut csv = csv::Reader::from_path(output).unwrap();
    assert_eq!(&csv.headers().unwrap()[0], "Product");
    let rows: Vec<_> = csv
        .records()
        .map(|row| row.unwrap()[0].to_string())
        .collect();
    assert_eq!(rows, ["00203", "00201", "00202"]);
}

#[test]
fn export_keeps_the_search_snapshot_during_a_concurrent_import() {
    let (directory, workspace) = sample();
    let revision = workspace.info().unwrap().revision;
    let query = QuerySpec {
        revision: Some(revision),
        ..QuerySpec::default()
    };
    let output = directory.path().join("snapshot.csv");
    let count = base_search_core::export::export(
        &workspace,
        &query,
        &output,
        None,
        &Operation::new(&AtomicBool::new(false), &|progress| {
            if progress.processed == 0 {
                load(
                    &workspace,
                    "interleaved.csv",
                    &[&["2025-08-01", "Новий", "00555", "5", "EUR", "1", "kg", "new"]],
                );
            }
        }),
    )
    .unwrap();
    assert_eq!(count, 5);
    assert_eq!(workspace.info().unwrap().row_count, 6);
    let mut csv = csv::Reader::from_path(output).unwrap();
    let rows: Vec<_> = csv.records().collect::<Result<_, _>>().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(&rows[3][7], "'=1+1");
}

#[test]
fn xlsx_exports_raw_codes_and_formula_text_as_literal_cells() {
    let (directory, workspace) = sample();
    let output = directory.path().join("view.xlsx");
    base_search_core::export::export(
        &workspace,
        &QuerySpec::default(),
        &output,
        None,
        &Operation::new(&AtomicBool::new(false), &|_| {}),
    )
    .unwrap();
    let mut workbook = open_workbook_auto(output).unwrap();
    let data = workbook.worksheet_range_at(0).unwrap().unwrap();
    assert_eq!(data.get((1, 2)), Some(&Data::String("00101".into())));
    assert_eq!(data.get((4, 7)), Some(&Data::String("=1+1".into())));
}

#[test]
fn cancelling_xlsx_before_publication_preserves_existing_output() {
    let (directory, workspace) = sample();
    let output = directory.path().join("existing.xlsx");
    std::fs::write(&output, b"previous output").unwrap();
    let cancelled = AtomicBool::new(false);
    let progress = |progress: base_search_core::Progress| {
        if progress.processed > 0 && progress.total == Some(progress.processed) {
            cancelled.store(true, Ordering::Relaxed);
        }
    };
    let result = base_search_core::export::export(
        &workspace,
        &QuerySpec::default(),
        &output,
        None,
        &Operation::new(&cancelled, &progress),
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(std::fs::read(output).unwrap(), b"previous output");
}

#[test]
fn earliest_visible_month_still_uses_the_previous_month_outside_the_window() {
    let (directory, _) = sample();
    let workspace = Workspace::open(directory.path().join("months.sqlite")).unwrap();
    let dates: Vec<_> = (0..201)
        .map(|index| format!("{:04}-{:02}-01", 2000 + index / 12, 1 + index % 12))
        .collect();
    let rows: Vec<_> = dates
        .iter()
        .map(|date| [date.as_str(), "", "", "", "", "", "", ""])
        .collect();
    let rows: Vec<&[&str]> = rows.iter().map(|row| row.as_slice()).collect();
    load(&workspace, "months.csv", &rows);
    let summary = summarize(&workspace, &QuerySpec::default(), Dimension::Month).unwrap();
    assert_eq!(summary.total_groups, 201);
    assert_eq!(summary.groups.len(), 200);
    assert_eq!(summary.groups[0].key, "2000-02");
    assert_eq!(summary.groups[0].change_percent, Some(0.0));
    assert_eq!(summary.total_rows, 201);
}

#[test]
fn amount_with_unrepresentable_precision_is_counted_as_unknown() {
    let (directory, _) = sample();
    let workspace = Workspace::open(directory.path().join("exact.sqlite")).unwrap();
    load(
        &workspace,
        "exact.csv",
        &[
            &[
                "",
                "",
                "",
                "1.00000000000000000000000000001e0",
                "EUR",
                "",
                "",
                "",
            ],
            &["", "", "", "900719925474099303e-2", "EUR", "", "", ""],
        ],
    );
    let summary = summarize(&workspace, &QuerySpec::default(), Dimension::Company).unwrap();
    assert_eq!(summary.unknown_value_rows, 1);
    assert_eq!(summary.currency_totals[0].amount, "9007199254740993.03");
}
