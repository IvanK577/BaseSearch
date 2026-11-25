use std::sync::atomic::AtomicBool;

use base_search::db::{Db, Query, ResultSort};
use base_search::{export, import};
use calamine::Reader;

fn imported_fixture() -> (tempfile::TempDir, Db, String, String) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("rows.csv");
    std::fs::write(&source, "Company;Amount\nZulu;2\nAlpha;10\nBeta;1\n").unwrap();

    let mut db = Db::open(&temp.path().join("rows.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &source, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 3);

    let catalog = db.result_fields_cached();
    let company = catalog
        .iter()
        .find(|field| field.label == "Company")
        .unwrap()
        .id
        .clone();
    let amount = catalog
        .iter()
        .find(|field| field.label == "Amount")
        .unwrap()
        .id
        .clone();
    (temp, db, company, amount)
}

#[test]
fn contextual_csv_export_preserves_selected_order_and_result_sort() {
    let (temp, db, company, amount) = imported_fixture();
    let catalog = db.result_fields_cached();
    let fields =
        export::resolve_fields(&catalog, Some(&[amount.clone(), company.clone()])).unwrap();
    let sort = ResultSort {
        field: amount,
        descending: true,
    };
    export::validate_sort(&catalog, Some(&sort)).unwrap();

    let output = temp.path().join("selected.csv");
    let cancel = AtomicBool::new(false);
    let written = export::export_selected(
        &db,
        &Query::default(),
        &output,
        &fields,
        Some(&sort),
        &cancel,
        |_, _| {},
    )
    .unwrap();
    assert_eq!(written, 3);

    let bytes = std::fs::read(output).unwrap();
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF");
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(&bytes[3..]);
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        vec!["Amount", "Company"]
    );
    let rows: Vec<Vec<String>> = reader
        .records()
        .map(|row| row.unwrap().iter().map(str::to_string).collect())
        .collect();
    assert_eq!(
        rows,
        vec![vec!["10", "Alpha"], vec!["2", "Zulu"], vec!["1", "Beta"],]
    );
}

#[test]
fn contextual_xlsx_export_preserves_selected_order_and_result_sort() {
    let (temp, db, company, amount) = imported_fixture();
    let catalog = db.result_fields_cached();
    let fields = export::resolve_fields(&catalog, Some(&[company, amount.clone()])).unwrap();
    let sort = ResultSort {
        field: amount,
        descending: false,
    };
    let output = temp.path().join("selected.xlsx");
    let cancel = AtomicBool::new(false);

    export::export_selected(
        &db,
        &Query::default(),
        &output,
        &fields,
        Some(&sort),
        &cancel,
        |_, _| {},
    )
    .unwrap();

    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(&output).unwrap();
    let range = workbook.worksheet_range_at(0).unwrap().unwrap();
    let values: Vec<Vec<String>> = range
        .rows()
        .map(|row| row.iter().map(ToString::to_string).collect())
        .collect();
    assert_eq!(
        values,
        vec![
            vec!["Company", "Amount"],
            vec!["Beta", "1"],
            vec!["Zulu", "2"],
            vec!["Alpha", "10"],
        ]
    );
}

#[test]
fn contextual_export_can_sort_by_a_column_that_is_not_exported() {
    let (temp, db, company, amount) = imported_fixture();
    let catalog = db.result_fields_cached();
    let fields = export::resolve_fields(&catalog, Some(&[company])).unwrap();
    let sort = ResultSort {
        field: amount,
        descending: true,
    };
    let output = temp.path().join("sort-only.csv");
    let cancel = AtomicBool::new(false);

    export::export_selected(
        &db,
        &Query::default(),
        &output,
        &fields,
        Some(&sort),
        &cancel,
        |_, _| {},
    )
    .unwrap();

    let bytes = std::fs::read(output).unwrap();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(&bytes[3..]);
    let values: Vec<String> = reader
        .records()
        .map(|row| row.unwrap().get(0).unwrap().to_string())
        .collect();
    assert_eq!(values, vec!["Alpha", "Zulu", "Beta"]);
}

#[test]
fn contextual_csv_export_neutralizes_formula_like_field_labels() {
    let (temp, db, company, _amount) = imported_fixture();
    let catalog = db.result_fields_cached();
    let mut fields = export::resolve_fields(&catalog, Some(&[company])).unwrap();
    fields[0].label = "=Unsafe header".to_string();
    let output = temp.path().join("safe-header.csv");
    let cancel = AtomicBool::new(false);

    export::export_selected(
        &db,
        &Query::default(),
        &output,
        &fields,
        None,
        &cancel,
        |_, _| {},
    )
    .unwrap();

    let bytes = std::fs::read(output).unwrap();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(&bytes[3..]);
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        vec!["'=Unsafe header"]
    );
}

#[test]
fn field_selection_rejects_empty_duplicate_unknown_and_excessive_ids() {
    let (_temp, db, company, _amount) = imported_fixture();
    let catalog = db.result_fields_cached();

    assert!(matches!(
        export::resolve_fields(&catalog, Some(&[])),
        Err(export::ExportSelectionError::EmptyFieldSelection)
    ));
    assert!(matches!(
        export::resolve_fields(&catalog, Some(&[company.clone(), company])),
        Err(export::ExportSelectionError::DuplicateField(_))
    ));
    assert!(matches!(
        export::resolve_fields(&catalog, Some(&["unknown-field".to_string()])),
        Err(export::ExportSelectionError::UnknownField(_))
    ));
    let excessive = (0..=export::MAX_EXPORT_FIELDS)
        .map(|index| format!("field-{index}"))
        .collect::<Vec<_>>();
    assert!(matches!(
        export::resolve_fields(&catalog, Some(&excessive)),
        Err(export::ExportSelectionError::TooManyFields { .. })
    ));
}

#[test]
fn contextual_export_rejects_unknown_sort_field_and_honors_cancellation() {
    let (temp, db, company, _amount) = imported_fixture();
    let catalog = db.result_fields_cached();
    let bad_sort = ResultSort {
        field: "unknown-field".to_string(),
        descending: false,
    };
    assert!(matches!(
        export::validate_sort(&catalog, Some(&bad_sort)),
        Err(export::ExportSelectionError::UnknownSortField(_))
    ));

    let fields = export::resolve_fields(&catalog, Some(&[company])).unwrap();
    let output = temp.path().join("cancelled.csv");
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        export::export_selected(
            &db,
            &Query::default(),
            &output,
            &fields,
            None,
            &cancel,
            |_, _| {},
        ),
        Err(export::ExportError::Cancelled)
    ));
    assert!(!output.exists());
}
