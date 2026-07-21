//! Core integration tests: import, deduplication, FTS search,
//! Cyrillic-aware filters, and CSV/XLSX export.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use base_search::db::{
    AnalyticsFilterField, AnalyticsScope, AnalyticsSectionKind, Db, Filters, ImportRecord,
    PivotDim, PivotLimits, PivotMetric, PriceMetricKind, Query, RecordScope, analytics_should_run,
    build_fts_query, canonical_record_hash, extract_year, fts_prefix_terms, parse_number,
    parse_number_grouped, pivot_filter_action,
};
use base_search::domain::table::{ColumnRole, ColumnStorage, SemanticField};
use base_search::export;
use base_search::import::{self, ImportPhase, collapse_ws, normalize_date, normalize_value};
use base_search::olap::{OlapBenchmarkOptions, run_sqlite_benchmark};
use base_search::schema::{COLUMNS, RESULT_COLUMNS, col_index, column_glossary};
use base_search::search::{
    ConditionOp, ConditionValue, FieldRef, LogicOp, QueryCondition, QueryExpr, QueryGroup,
};
use calamine::Reader;

#[test]
fn public_product_name_is_base_search() {
    assert_eq!(base_search::i18n::UA.app_title, "Base Search");
    assert_eq!(base_search::i18n::EN.app_title, "Base Search");
}

fn result_col(name: &str) -> usize {
    RESULT_COLUMNS
        .iter()
        .position(|column| *column == name)
        .unwrap_or_else(|| panic!("missing result column {name}"))
}

#[test]
fn result_table_exposes_all_source_columns() {
    let expected: Vec<&str> = COLUMNS
        .iter()
        .map(|column| column.name)
        .chain(std::iter::once("source_file"))
        .collect();
    assert_eq!(RESULT_COLUMNS.as_slice(), expected.as_slice());
}

#[test]
fn database_storage_maintenance_reports_and_compacts_without_deleting_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("maintenance.db");
    let db = Db::open(&db_path).unwrap();
    db.diagnostic_execute_batch(
        "CREATE TABLE storage_test(value TEXT);
             INSERT INTO storage_test VALUES (zeroblob(100000));
             DROP TABLE storage_test;",
    )
    .unwrap();

    let before = db.storage_info(&db_path).unwrap();
    assert!(before.database_bytes > 0);
    assert!(before.page_size > 0);
    assert!(before.page_count > 0);

    let checkpoint = db.checkpoint_wal_truncate().unwrap();
    assert_eq!(checkpoint.busy, 0);
    db.vacuum_database().unwrap();

    let after = db.storage_info(&db_path).unwrap();
    assert_eq!(db.total_rows(), 0);
    assert_eq!(after.wal_bytes, 0);
    assert!(after.total_file_bytes() <= before.total_file_bytes());
}

#[test]
fn runtime_connection_opens_while_import_transaction_is_active() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("runtime-open-during-import.db");
    let mut writer = Db::open(&db_path).unwrap();

    writer.begin_import_file().unwrap();
    let reader = Db::open_runtime(&db_path).expect("runtime connection should not rerun schema");
    assert_eq!(reader.total_rows(), 0);
    writer.rollback_import_file();
}

#[test]
fn column_glossary_covers_abbreviated_table_headers() {
    for name in [
        "clearance_time",
        "customs_office",
        "declaration_number",
        "trade_country",
        "dispatch_country",
        "origin_country",
        "delivery_terms",
        "delivery_place",
        "quantity",
        "unit",
        "declaration_weight",
        "currency_control_value",
        "movement_feature",
        "field_43",
        "field_43_01",
        "rfv_usd_kg",
        "unit_weight",
        "weight_difference",
        "contract",
        "field_3001",
        "field_3002",
        "field_9610",
        "rmv_net_usd_kg",
        "rmv_usd_extra_unit",
        "rmv_gross_usd_kg",
        "zed_purpose",
        "min_base_usd_kg",
        "min_base_difference",
        "preferential",
        "full_rate",
    ] {
        assert!(
            column_glossary(name).is_some(),
            "missing glossary for {name}"
        );
    }
}

#[test]
fn pivot_dimension_labels_map_to_filter_actions() {
    let action = pivot_filter_action(PivotDim::TradeCountry, "IE").unwrap();
    assert_eq!(action.field, AnalyticsFilterField::TradeCountry);
    assert_eq!(action.value, "IE");

    let action = pivot_filter_action(PivotDim::Recipient, "ТОВ ЕППЛ УКРАЇНА").unwrap();
    assert_eq!(action.field, AnalyticsFilterField::Recipient);
    assert_eq!(action.value, "ТОВ ЕППЛ УКРАЇНА");

    assert_eq!(pivot_filter_action(PivotDim::Month, "2024-03"), None);
    assert_eq!(pivot_filter_action(PivotDim::Year, "2024"), None);
}

#[test]
fn olap_benchmark_runs_sqlite_baseline_scenarios() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("benchmark.xlsx");
    let db_path = dir.path().join("benchmark.db");
    write_test_xlsx(&xlsx, &sample_rows());
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert!(summary.error.is_none());

    let query = Query {
        text: "SIEMENS".to_string(),
        ..Default::default()
    };
    let report = run_sqlite_benchmark(
        &db,
        &query,
        &OlapBenchmarkOptions {
            repeat: 1,
            warmups: 0,
            page_limit: 5,
            section_limit: 5,
            pivot_rows: 5,
            pivot_cols: 5,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(report.backend, "sqlite");
    assert_eq!(report.total_database_rows, 3);
    assert_eq!(report.query.text, "SIEMENS");
    assert_eq!(report.scenarios.len(), 9);
    assert!(
        report
            .scenarios
            .iter()
            .any(|scenario| scenario.name == "Search count" && scenario.output_rows == 1)
    );
    assert!(
        report
            .scenarios
            .iter()
            .any(|scenario| scenario.name == "Pivot: recipient by month")
    );
    for scenario in &report.scenarios {
        assert_eq!(scenario.runs_ms.len(), 1);
        assert!(scenario.maximum_ms >= scenario.minimum_ms);
    }
}

/// Creates a test XLSX file with the full schema column set.
fn write_test_xlsx(path: &Path, rows: &[Vec<(&str, &str)>]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, def) in COLUMNS.iter().enumerate() {
        sheet.write_string(0, col as u16, def.header).unwrap();
    }
    for (r, row) in rows.iter().enumerate() {
        for (name, value) in row {
            let col = col_index(name).unwrap() as u16;
            sheet.write_string(r as u32 + 1, col, *value).unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn write_owned_test_xlsx(path: &Path, rows: &[Vec<(String, String)>]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, def) in COLUMNS.iter().enumerate() {
        sheet.write_string(0, col as u16, def.header).unwrap();
    }
    for (r, row) in rows.iter().enumerate() {
        for (name, value) in row {
            let col = col_index(name).unwrap() as u16;
            sheet
                .write_string(r as u32 + 1, col, value.as_str())
                .unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn sample_rows() -> Vec<Vec<(&'static str, &'static str)>> {
    vec![
        vec![
            ("declaration_number", "24UA100110000001U1"),
            ("declaration_date", "15.03.2024"),
            ("sender", "GUANGZHOU TRADING CO., LTD"),
            ("edrpou", "12345678"),
            ("recipient", "ТОВ «Вінтаж Імпорт»"),
            ("product_code", "0810500000"),
            ("description", "Ківі свіжі, врожай 2023 року"),
            ("trade_country", "CN"),
        ],
        vec![
            ("declaration_number", "24UA100110000002U2"),
            ("declaration_date", "16.03.2024"),
            ("sender", "BODEGAS RIOJA S.A."),
            ("edrpou", "87654321"),
            ("recipient", "ТОВ «Вино Світу»"),
            ("product_code", "2204101100"),
            ("description", "Вино виноградне ігристе, біле"),
            ("trade_country", "ES"),
        ],
        vec![
            ("declaration_number", "24UA100110000003U3"),
            ("declaration_date", "17.03.2024"),
            ("sender", "SIEMENS AG"),
            ("edrpou", "11112222"),
            ("recipient", "ТОВ «Електро Трейд»"),
            ("product_code", "8504405500"),
            ("description", "Перетворювач напруги статичний"),
            ("trade_country", "DE"),
        ],
    ]
}

fn advanced_condition(field: &str, op: ConditionOp, value: ConditionValue) -> QueryExpr {
    QueryExpr::Condition(QueryCondition {
        field: FieldRef::Column(field.to_string()),
        op,
        value,
        negated: false,
    })
}

fn advanced_query(expr: QueryExpr) -> Query {
    Query {
        advanced: Some(expr),
        ..Query::default()
    }
}

#[test]
fn v2_search_ast_empty_checks_ignore_blank_values() {
    let blank = QueryExpr::Condition(QueryCondition {
        field: FieldRef::Column("sender".into()),
        op: ConditionOp::Contains,
        value: ConditionValue::Single("  ".into()),
        negated: false,
    });
    assert!(blank.is_empty());

    let non_blank = QueryExpr::Group(QueryGroup {
        op: LogicOp::And,
        negated: false,
        children: vec![QueryExpr::Condition(QueryCondition {
            field: FieldRef::Column("year".into()),
            op: ConditionOp::Equals,
            value: ConditionValue::Single("2024".into()),
            negated: false,
        })],
    });
    assert!(!non_blank.is_empty());
}

#[test]
fn v2_search_ast_supports_logic_ranges_empty_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("v2-search.xlsx");
    let db_path = dir.path().join("v2-search.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA0000001U1"),
                ("declaration_date", "15.01.2024"),
                ("sender", "ALPHA SUPPLY"),
                ("recipient", "A IMPORT"),
                ("product_code", "8504405500"),
                ("description", "Power converter"),
                ("origin_country", "CN"),
                ("net_kg", "10"),
            ],
            vec![
                ("declaration_number", "24UA0000002U2"),
                ("declaration_date", "20.02.2024"),
                ("sender", "BETA SUPPLY"),
                ("recipient", "B IMPORT"),
                ("product_code", "8504900000"),
                ("description", "Static converter"),
                ("origin_country", "PL"),
                ("net_kg", "15"),
            ],
            vec![
                ("declaration_number", "24UA0000003U3"),
                ("declaration_date", "10.03.2024"),
                ("sender", "GAMMA SUPPLY"),
                ("recipient", "C IMPORT"),
                ("product_code", "2204210000"),
                ("description", ""),
                ("origin_country", "DE"),
                ("net_kg", "30"),
            ],
            vec![
                ("declaration_number", "25UA0000004U4"),
                ("declaration_date", "10.03.2025"),
                ("sender", "ALPHA SUPPLY"),
                ("recipient", "D IMPORT"),
                ("product_code", "8708990000"),
                ("description", "Vehicle part"),
                ("origin_country", "CN"),
                ("net_kg", "5"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});

    let sender_or = QueryExpr::Group(QueryGroup {
        op: LogicOp::Or,
        negated: false,
        children: vec![
            advanced_condition(
                "sender",
                ConditionOp::Contains,
                ConditionValue::Single("ALPHA".into()),
            ),
            advanced_condition(
                "sender",
                ConditionOp::Contains,
                ConditionValue::Single("BETA".into()),
            ),
        ],
    });
    let year_2024 = advanced_condition(
        "year",
        ConditionOp::Equals,
        ConditionValue::Single("2024".into()),
    );
    let query = advanced_query(QueryExpr::Group(QueryGroup {
        op: LogicOp::And,
        negated: false,
        children: vec![sender_or, year_2024],
    }));
    assert_eq!(db.count(&query).unwrap(), 2);

    let not_cn = advanced_query(QueryExpr::Condition(QueryCondition {
        field: FieldRef::Column("origin_country".into()),
        op: ConditionOp::Equals,
        value: ConditionValue::Single("CN".into()),
        negated: true,
    }));
    assert_eq!(db.count(&not_cn).unwrap(), 2);

    let code_prefix = advanced_query(advanced_condition(
        "product_code",
        ConditionOp::StartsWith,
        ConditionValue::Single("8504".into()),
    ));
    assert_eq!(db.count(&code_prefix).unwrap(), 2);

    let numeric_range = advanced_query(advanced_condition(
        "net_kg",
        ConditionOp::Range,
        ConditionValue::Range {
            from: Some("5".into()),
            to: Some("15".into()),
        },
    ));
    assert_eq!(db.count(&numeric_range).unwrap(), 3);

    let date_range = advanced_query(advanced_condition(
        "declaration_date",
        ConditionOp::Range,
        ConditionValue::Range {
            from: Some("2024-01-01".into()),
            to: Some("2024-12-31".into()),
        },
    ));
    assert_eq!(db.count(&date_range).unwrap(), 3);

    let empty_description = advanced_query(advanced_condition(
        "description",
        ConditionOp::IsEmpty,
        ConditionValue::None,
    ));
    assert_eq!(db.count(&empty_description).unwrap(), 1);

    let invalid = advanced_query(advanced_condition(
        "description",
        ConditionOp::Range,
        ConditionValue::Range {
            from: Some("A".into()),
            to: Some("Z".into()),
        },
    ));
    assert!(db.count(&invalid).is_err());
}

#[test]
fn value_filter_uses_the_same_number_style_as_analytics() {
    // A value cell "1.250" is a thousands group (1250) — the way analytics
    // aggregates it. A filter value = 1250 must find that row, and value =
    // 1.25 must not: the filter and the stored/aggregated value must parse
    // the same number style, or the total cannot be reproduced by filtering.
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("value.xlsx");
    let db_path = dir.path().join("value.db");
    write_test_xlsx(
        &xlsx,
        &[vec![
            ("declaration_number", "24UA0000009U9"),
            ("declaration_date", "15.01.2024"),
            ("sender", "ALPHA SUPPLY"),
            ("recipient", "A IMPORT"),
            ("product_code", "8504405500"),
            ("origin_country", "CN"),
            ("net_kg", "10"),
            ("currency_control_value", "1.250"),
        ]],
    );
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});

    let grouped = advanced_query(advanced_condition(
        "currency_control_value",
        ConditionOp::Equals,
        ConditionValue::Single("1250".into()),
    ));
    assert_eq!(db.count(&grouped).unwrap(), 1);

    let decimal = advanced_query(advanced_condition(
        "currency_control_value",
        ConditionOp::Equals,
        ConditionValue::Single("1.25".into()),
    ));
    assert_eq!(db.count(&decimal).unwrap(), 0);
}

#[test]
fn import_search_filter_export() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("test.xlsx");
    let db_path = dir.path().join("data").join("test.db");
    write_test_xlsx(&xlsx, &sample_rows());

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.total_rows, 3);
    assert_eq!(summary.imported, 3);
    assert_eq!(summary.duplicates, 0);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.quality.header_row, 1);
    assert_eq!(summary.quality.source_columns, COLUMNS.len() as u64);
    assert_eq!(summary.quality.recognized_columns, COLUMNS.len() as u64);
    assert_eq!(summary.quality.extra_columns, 0);
    assert_eq!(summary.quality.non_empty_cells, 24);
    assert_eq!(summary.quality.empty_cells, 99);
    assert!(summary.quality.warnings.is_empty());
    assert_eq!(db.total_rows(), 3);
    assert_eq!(db.unindexed_rows(), 0);

    let shape = db.table_shape().unwrap();
    assert_eq!(shape.columns.len(), COLUMNS.len());
    let product_code = shape
        .columns
        .iter()
        .find(|column| column.semantic == Some(SemanticField::ProductCode))
        .unwrap();
    assert_eq!(product_code.source_index, result_col("product_code"));
    assert_eq!(
        product_code.storage,
        ColumnStorage::SchemaColumn("product_code".to_string())
    );
    let value = shape
        .columns
        .iter()
        .find(|column| column.semantic == Some(SemanticField::Value))
        .unwrap();
    assert_eq!(value.source_index, result_col("currency_control_value"));
    assert_eq!(
        value.storage,
        ColumnStorage::SchemaColumn("currency_control_value".to_string())
    );

    // A file with the same rows plus one new row: overlapping rows stay visible
    // in Results, but are flagged as duplicates for Analytics.
    let xlsx2 = dir.path().join("test2.xlsx");
    let mut rows2 = sample_rows();
    rows2.push(vec![
        ("declaration_number", "24UA100110000004U4"),
        ("declaration_date", "18.03.2024"),
        ("sender", "NEW SENDER LLC"),
        ("edrpou", "99999999"),
        ("recipient", "ТОВ «Нове»"),
        ("product_code", "0810500001"),
        ("description", "Ківі свіжі друга партія"),
        ("trade_country", "CN"),
    ]);
    write_test_xlsx(&xlsx2, &rows2);
    let summary2 = import::import_file(&mut db, &xlsx2, &cancel, &mut |_, _, _| {});
    assert_eq!(summary2.error, None);
    assert_eq!(summary2.imported, 4);
    assert_eq!(summary2.duplicates, 3);
    assert_eq!(db.total_rows(), 7);

    // FTS: exact word matching, case-insensitive for Cyrillic text.
    let q = |text: &str| Query {
        text: text.into(),
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert_eq!(db.count(&q("виноградне")).unwrap(), 2);
    assert_eq!(db.count(&q("ВИНОГРАДНЕ")).unwrap(), 2);
    // Explicit prefix search with an asterisk.
    assert_eq!(db.count(&q("виноград*")).unwrap(), 2);
    // Numeric product codes are automatically treated as prefixes.
    assert_eq!(db.count(&q("8504")).unwrap(), 2);
    // Search by declaration number.
    assert_eq!(db.count(&q("24UA100110000002U2")).unwrap(), 2);
    // Multiple words are combined as AND.
    assert_eq!(db.count(&q("вино біле")).unwrap(), 2);
    assert_eq!(db.count(&q("вино червоне")).unwrap(), 0);

    // The date is normalized to ISO and the year is extracted.
    let (_, rows, dups) = db.search_page(&q("8504"), 10, 0).unwrap();
    assert_eq!(rows[0][result_col("declaration_date")], "2024-03-17");
    assert_eq!(dups[0], Some("test.xlsx".to_string()));

    // Filters.
    let filters = Filters {
        year: "2024".into(),
        ..Default::default()
    };
    let fq = Query {
        text: String::new(),
        filters: filters.clone(),
        advanced: None,
        record_scope: RecordScope::Occurrences,
    };
    assert_eq!(db.count(&fq).unwrap(), 7);
    let analytics = db.analytics(&fq, 10).unwrap();
    assert_eq!(analytics.overview.row_count, 7);

    // Recipient filter: Cyrillic text in a different case.
    let filters = Filters {
        recipient: "вінтаж".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: String::new(),
            filters,
            advanced: None,
            record_scope: RecordScope::Occurrences,
        })
        .unwrap(),
        2
    );

    // Product code filter: prefix matching.
    let filters = Filters {
        product_code: "2204".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: String::new(),
            filters,
            advanced: None,
            record_scope: RecordScope::Occurrences,
        })
        .unwrap(),
        2
    );

    // EDRPOU filter: exact match.
    let filters = Filters {
        edrpou: "11112222".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: String::new(),
            filters,
            advanced: None,
            record_scope: RecordScope::Occurrences,
        })
        .unwrap(),
        2
    );

    // Combination: text query plus country filter, with case-insensitive text.
    let filters = Filters {
        trade_country: "es".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: "вино".into(),
            filters,
            advanced: None,
            record_scope: RecordScope::Occurrences,
        })
        .unwrap(),
        2
    );
    // The same query with a different country must return no rows.
    let filters = Filters {
        trade_country: "cn".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: "вино".into(),
            filters,
            advanced: None,
            record_scope: RecordScope::Occurrences,
        })
        .unwrap(),
        0
    );

    // Record details card.
    let (ids, _, dups) = db.search_page(&q("8504"), 10, 0).unwrap();
    assert_eq!(dups[0], Some("test.xlsx".to_string()));
    let card = db.record_card(ids[0]).unwrap();
    assert_eq!(card.source_file, "test2.xlsx");
    assert!(
        card.fields
            .iter()
            .any(|(h, v)| h == "Опис товару" && v.contains("Перетворювач"))
    );

    // CSV export: BOM plus every imported occurrence.
    let all_rows_query = Query {
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    let csv_path = dir.path().join("out.csv");
    let n = export::export(&db, &all_rows_query, &csv_path, &cancel, |_, _| {}).unwrap();
    assert_eq!(n, 7);
    let bytes = std::fs::read(&csv_path).unwrap();
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF");
    let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
    assert!(text.contains("Перетворювач напруги"));
    assert!(text.starts_with("Час оформлення;"));

    // XLSX export: opens successfully and contains all data rows.
    let xlsx_out = dir.path().join("out.xlsx");
    let n = export::export(&db, &all_rows_query, &xlsx_out, &cancel, |_, _| {}).unwrap();
    assert_eq!(n, 7);
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&xlsx_out).unwrap();
    let range = wb.worksheet_range_at(0).unwrap().unwrap();
    assert_eq!(range.height(), 8); // header + 7 rows
}

#[test]
fn record_scope_is_consistent_across_search_analytics_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.xlsx");
    let second = dir.path().join("second.xlsx");
    let db_path = dir.path().join("scope.db");
    let row_a = vec![
        ("declaration_number", "24UA100000000201U1"),
        ("declaration_date", "15.03.2024"),
        ("sender", "ALPHA SUPPLY"),
        ("recipient", "ALPHA IMPORT"),
        ("product_code", "8504405500"),
        ("description", "Scope fixture alpha"),
        ("net_kg", "10"),
        ("currency_control_value", "100"),
    ];
    let row_b = vec![
        ("declaration_number", "24UA100000000202U2"),
        ("declaration_date", "16.03.2024"),
        ("sender", "BETA SUPPLY"),
        ("recipient", "BETA IMPORT"),
        ("product_code", "8517130000"),
        ("description", "Scope fixture beta"),
        ("net_kg", "20"),
        ("currency_control_value", "300"),
    ];
    write_test_xlsx(&first, &[row_a.clone(), row_b]);
    write_test_xlsx(&second, &[row_a]);

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let first_summary = import::import_file(&mut db, &first, &cancel, &mut |_, _, _| {});
    let second_summary = import::import_file(&mut db, &second, &cancel, &mut |_, _, _| {});
    assert_eq!(first_summary.imported, 2);
    assert_eq!(second_summary.imported, 1);
    assert_eq!(second_summary.duplicates, 1);

    let canonical = Query::default();
    assert_eq!(db.count(&canonical).unwrap(), 2);
    assert_eq!(db.search_page(&canonical, 10, 0).unwrap().0.len(), 2);
    assert_eq!(db.analytics(&canonical, 10).unwrap().overview.row_count, 2);
    let canonical_export = dir.path().join("canonical.csv");
    assert_eq!(
        export::export(&db, &canonical, &canonical_export, &cancel, |_, _| {}).unwrap(),
        2
    );

    let occurrences = Query {
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert_eq!(db.count(&occurrences).unwrap(), 3);
    assert_eq!(db.search_page(&occurrences, 10, 0).unwrap().0.len(), 3);
    assert_eq!(
        db.analytics(&occurrences, 10).unwrap().overview.row_count,
        3
    );
    let occurrence_export = dir.path().join("occurrences.csv");
    assert_eq!(
        export::export(&db, &occurrences, &occurrence_export, &cancel, |_, _| {},).unwrap(),
        3
    );
}

#[test]
fn multi_sheet_workbook_imports_every_non_empty_sheet() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("multi-sheet.xlsx");
    let db_path = dir.path().join("multi-sheet.db");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("January").unwrap();
        for (column, def) in COLUMNS.iter().enumerate() {
            sheet.write_string(0, column as u16, def.header).unwrap();
        }
        for (name, value) in [
            ("declaration_number", "24UA100000000301U1"),
            ("declaration_date", "15.01.2024"),
            ("sender", "JANUARY SUPPLY"),
            ("recipient", "MULTI IMPORT"),
            ("product_code", "8504405500"),
            ("description", "January sheet item"),
        ] {
            sheet
                .write_string(1, col_index(name).unwrap() as u16, value)
                .unwrap();
        }
    }
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("February").unwrap();
        for (column, def) in COLUMNS.iter().enumerate() {
            sheet.write_string(0, column as u16, def.header).unwrap();
        }
        for (name, value) in [
            ("declaration_number", "24UA100000000302U2"),
            ("declaration_date", "16.02.2024"),
            ("sender", "FEBRUARY SUPPLY"),
            ("recipient", "MULTI IMPORT"),
            ("product_code", "8517130000"),
            ("description", "February sheet item"),
        ] {
            sheet
                .write_string(1, col_index(name).unwrap() as u16, value)
                .unwrap();
        }
    }
    workbook.add_worksheet().set_name("Empty").unwrap();
    workbook.save(&xlsx).unwrap();

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.total_rows, 2);
    assert_eq!(summary.quality.layout, "multi-sheet workbook");

    let january = Query {
        text: "January sheet item".into(),
        ..Default::default()
    };
    let february = Query {
        text: "February sheet item".into(),
        ..Default::default()
    };
    let (january_ids, _, _) = db.search_page(&january, 10, 0).unwrap();
    let (february_ids, _, _) = db.search_page(&february, 10, 0).unwrap();
    assert_eq!(january_ids.len(), 1);
    assert_eq!(february_ids.len(), 1);
    assert!(
        db.record_card(january_ids[0])
            .unwrap()
            .source_file
            .contains("January")
    );
    assert!(
        db.record_card(february_ids[0])
            .unwrap()
            .source_file
            .contains("February")
    );
}

#[test]
fn selected_sheets_can_be_imported_separately_without_false_file_deduplication() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("selected-sheets.xlsx");
    let db_path = dir.path().join("selected-sheets.db");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    for (sheet_name, sku, description) in [
        ("January", "JAN-1", "January selected row"),
        ("February", "FEB-1", "February selected row"),
    ] {
        let sheet = workbook.add_worksheet().set_name(sheet_name).unwrap();
        for (column, header) in ["SKU", "Description"].iter().enumerate() {
            sheet.write_string(0, column as u16, *header).unwrap();
        }
        sheet.write_string(1, 0, sku).unwrap();
        sheet.write_string(1, 1, description).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let january = import::import_file_with_options(
        &mut db,
        &xlsx,
        &import::ImportOptions::selected_sheets(["January"]),
        &cancel,
        &mut |_, _, _| {},
    );
    assert_eq!(january.error, None);
    assert_eq!(january.imported, 1);
    assert_eq!(db.total_rows(), 1);

    let february = import::import_file_with_options(
        &mut db,
        &xlsx,
        &import::ImportOptions::selected_sheets(["February"]),
        &cancel,
        &mut |_, _, _| {},
    );
    assert_eq!(february.error, None);
    assert_eq!(february.imported, 1);
    assert_eq!(db.total_rows(), 2);

    let january_retry = import::import_file_with_options(
        &mut db,
        &xlsx,
        &import::ImportOptions::selected_sheets(["January"]),
        &cancel,
        &mut |_, _, _| {},
    );
    assert_eq!(january_retry.imported, 0);
    assert!(january_retry.skipped_duplicate_of.is_some());
    assert_eq!(db.total_rows(), 2);
}

#[test]
fn delimited_text_import_detects_csv_and_tsv_without_losing_quoted_values() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("orders.csv");
    let tsv_path = dir.path().join("inventory.tsv");
    let db_path = dir.path().join("delimited.db");
    std::fs::write(
        &csv_path,
        "\u{feff}Order Date;Product Name;Amount EUR;Origin Country\n\
         2024-01-15;\"Laptop, Pro\";1299.50;DE\n\
         2024-01-16;\"Desk\nstand\";49.90;PL\n",
    )
    .unwrap();
    std::fs::write(
        &tsv_path,
        "Date\tSKU\tDescription\tQuantity\n2024-02-01\tSKU-77\tMonitor arm\t5\n",
    )
    .unwrap();

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let csv_summary = import::import_file(&mut db, &csv_path, &cancel, &mut |_, _, _| {});
    let tsv_summary = import::import_file(&mut db, &tsv_path, &cancel, &mut |_, _, _| {});
    assert_eq!(csv_summary.error, None);
    assert_eq!(csv_summary.imported, 2);
    assert_eq!(tsv_summary.error, None);
    assert_eq!(tsv_summary.imported, 1);

    for text in ["Laptop", "Desk stand", "Monitor arm"] {
        assert_eq!(
            db.count(&Query {
                text: text.into(),
                ..Default::default()
            })
            .unwrap(),
            1,
            "missing imported delimited row for {text}"
        );
    }
    assert_eq!(
        db.count(&Query {
            filters: Filters {
                year: "2024".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap(),
        3
    );
}

#[test]
fn csv_export_neutralizes_formula_leading_cells() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("formula.xlsx");
    let db_path = dir.path().join("formula.db");
    write_test_xlsx(
        &xlsx,
        &[vec![
            ("declaration_number", "24UA100110000001U1"),
            ("declaration_date", "15.03.2024"),
            ("sender", "=WEBSERVICE(\"https://example.invalid\")"),
            ("edrpou", "12345678"),
            ("recipient", "+SUM(1,1)"),
            ("product_code", "8504405500"),
            ("description", "@HYPERLINK(\"https://example.invalid\")"),
        ]],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);

    let csv_path = dir.path().join("formula.csv");
    export::export(&db, &Query::default(), &csv_path, &cancel, |_, _| {}).unwrap();
    let bytes = std::fs::read(&csv_path).unwrap();
    let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
    assert!(text.contains("'=WEBSERVICE"));
    assert!(text.contains("'+SUM"));
    assert!(text.contains("'@HYPERLINK"));
}

#[test]
fn export_rejects_unsupported_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("data.xlsx");
    let db_path = dir.path().join("data.db");
    write_test_xlsx(&xlsx, &sample_rows());

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);

    let result = export::export(
        &db,
        &Query::default(),
        &dir.path().join("wrong.xls"),
        &cancel,
        |_, _| {},
    );
    assert!(matches!(
        result,
        Err(export::ExportError::UnsupportedExtension(_))
    ));
}

#[test]
fn analytics_summarizes_filtered_rows_by_value_and_company() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("analytics.xlsx");
    let db_path = dir.path().join("data").join("analytics.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100110000101U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "NIMBUS DISTRIBUTION INTERNATIONAL LTD"),
                ("edrpou", "11111111"),
                ("recipient", "ТОВ «АЙФОН УКРАЇНА»"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget 15 smartphone"),
                ("trade_country", "IE"),
                ("origin_country", "CN"),
                ("quantity", "10"),
                ("gross_kg", "12.5"),
                ("net_kg", "10"),
                ("currency_control_value", "1 200.50"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000102U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "NIMBUS OPERATIONS EUROPE"),
                ("edrpou", "22222222"),
                ("recipient", "ТОВ «ТЕХНО ІМПОРТ»"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget parts"),
                ("trade_country", "IE"),
                ("origin_country", "CN"),
                ("quantity", "2"),
                ("gross_kg", "3,5"),
                ("net_kg", "2,5"),
                ("currency_control_value", "300,25"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000103U3"),
                ("declaration_date", "17.03.2024"),
                ("sender", "SAMSUNG ELECTRONICS"),
                ("edrpou", "33333333"),
                ("recipient", "ТОВ «ТЕХНО ІМПОРТ»"),
                ("product_code", "8517130000"),
                ("description", "Samsung smartphone"),
                ("trade_country", "KR"),
                ("origin_country", "VN"),
                ("quantity", "5"),
                ("gross_kg", "7"),
                ("net_kg", "6"),
                ("currency_control_value", "700"),
                ("trademark", "Samsung"),
            ],
            vec![
                ("declaration_number", "25UA100110000104U4"),
                ("declaration_date", "12.01.2025"),
                ("sender", "NIMBUS DISTRIBUTION INTERNATIONAL LTD"),
                ("edrpou", "11111111"),
                ("recipient", "ТОВ «АЙФОН УКРАЇНА»"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget 16 smartphone"),
                ("trade_country", "IE"),
                ("origin_country", "CN"),
                ("quantity", "20"),
                ("gross_kg", "25"),
                ("net_kg", "20"),
                ("currency_control_value", "2 400"),
                ("trademark", "Nimbus"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 4);

    let q = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let analytics = db.analytics(&q, 5).unwrap();
    assert_eq!(analytics.overview.row_count, 2);
    assert_eq!(analytics.overview.distinct_senders, 2);
    assert_eq!(analytics.overview.distinct_recipients, 2);
    assert_eq!(analytics.overview.distinct_trademarks, 1);
    assert_close(analytics.overview.total_value_usd, 1500.75);
    assert_close(analytics.overview.total_gross_kg, 16.0);
    assert_close(analytics.overview.total_net_kg, 12.5);
    assert_close(analytics.overview.total_quantity, 12.0);

    assert_eq!(analytics.top_recipients[0].label, "ТОВ «АЙФОН УКРАЇНА»");
    assert_eq!(analytics.top_recipients[0].rows, 1);
    assert_close(analytics.top_recipients[0].total_value_usd, 1200.50);
    assert_eq!(
        analytics.top_senders[0].label,
        "NIMBUS DISTRIBUTION INTERNATIONAL LTD"
    );
    assert_eq!(analytics.top_trademarks[0].label, "Nimbus");
    assert_eq!(analytics.top_product_codes[0].label, "8517130000");
    assert_eq!(analytics.top_origin_countries[0].label, "CN");
    assert_eq!(analytics.top_origin_countries[0].rows, 2);

    // Monthly dynamics: both Nimbus-2024 rows fall into the same month.
    assert_eq!(analytics.months.len(), 1);
    assert_eq!(analytics.months[0].month, "2024-03");
    assert_eq!(analytics.months[0].rows, 2);
    assert_eq!(analytics.months[0].declarations, 2);
    assert_close(analytics.months[0].total_value_usd, 1500.75);

    // Without the year filter the months are listed chronologically.
    let q_all = Query {
        text: "Nimbus".into(),
        ..Default::default()
    };
    let analytics_all = db.analytics(&q_all, 5).unwrap();
    let months: Vec<&str> = analytics_all
        .months
        .iter()
        .map(|m| m.month.as_str())
        .collect();
    assert_eq!(months, vec!["2024-03", "2025-01"]);
    assert_eq!(analytics_all.months[1].rows, 1);
    assert_close(analytics_all.months[1].total_value_usd, 2400.0);

    // R-7 continuous percentiles for 120.0, 120.05 and 120.1 interpolate
    // halfway between adjacent values at the first and third quartiles.
    let price = &analytics_all.price_sections[0];
    assert_eq!(price.kind, PriceMetricKind::ValuePerNetKg);
    assert_eq!(price.count, 3);
    assert_close(price.median, 120.05);
    assert_close(price.p25, 120.025);
    assert_close(price.p75, 120.075);

    // Product codes grouped at the 4-digit HS level.
    let products = db
        .analytics_scoped(&q_all, 5, Some(AnalyticsScope::Products), 4)
        .unwrap();
    assert_eq!(products.top_product_codes[0].label, "8517");
    assert_eq!(products.top_product_codes[0].rows, 3);

    // Overview-only scope skips the heavy section queries.
    let overview_only = db.analytics_scoped(&q_all, 5, None, 10).unwrap();
    assert_eq!(overview_only.overview.row_count, 3);
    assert!(overview_only.company_sections.is_empty());
    assert!(overview_only.price_sections.is_empty());

    // Company dossier for EDRPOU 11111111 (the Nimbus importer, 2 rows total).
    let profile = db.company_profile("11111111", 10).unwrap();
    assert_eq!(profile.edrpou, "11111111");
    assert_eq!(profile.names, vec!["ТОВ «АЙФОН УКРАЇНА»".to_string()]);
    assert_eq!(profile.overview.row_count, 2);
    assert_close(profile.overview.total_value_usd, 3600.5);
    assert_eq!(profile.top_products[0].label, "8517130000");
    assert_eq!(
        profile.top_senders[0].label,
        "NIMBUS DISTRIBUTION INTERNATIONAL LTD"
    );
    assert_eq!(profile.top_origin_countries[0].label, "CN");
    assert_eq!(
        analytics_section(&profile.product_sections, AnalyticsSectionKind::Trademarks).rows[0]
            .label,
        "Nimbus"
    );
    assert_eq!(
        analytics_section(
            &profile.country_sections,
            AnalyticsSectionKind::OriginCountries
        )
        .rows[0]
            .label,
        "CN"
    );
    assert_eq!(
        price_metric(&profile.price_sections, PriceMetricKind::ValuePerNetKg).count,
        2
    );
    // Both months in which this company imported are present.
    let months: Vec<&str> = profile.months.iter().map(|m| m.month.as_str()).collect();
    assert_eq!(months, vec!["2024-03", "2025-01"]);

    // Pivot: recipients (rows) by origin country (columns), counting rows.
    let pivot = db
        .pivot(
            &q_all,
            PivotDim::Recipient,
            PivotDim::OriginCountry,
            PivotMetric::Rows,
            PivotLimits { rows: 25, cols: 18 },
            "others",
        )
        .unwrap();
    // Two recipients, one origin country.
    assert_eq!(pivot.col_labels, vec!["CN".to_string()]);
    assert_eq!(pivot.row_labels.len(), 2);
    assert_eq!(pivot.grand_total, 3.0);
    assert!(pivot.row_filterable);
    assert!(pivot.col_filterable);
    // The matrix and totals are internally consistent.
    let sum_cells: f64 = pivot.cells.iter().flat_map(|r| r.iter()).sum();
    assert_eq!(sum_cells, pivot.grand_total);
    assert_eq!(pivot.col_totals[0], 3.0);

    // Pivot by month with the value metric: two months, value matches overview.
    let pivot_m = db
        .pivot(
            &q_all,
            PivotDim::Recipient,
            PivotDim::Month,
            PivotMetric::Value,
            PivotLimits { rows: 25, cols: 18 },
            "others",
        )
        .unwrap();
    assert_eq!(
        pivot_m.col_labels,
        vec!["2024-03".to_string(), "2025-01".to_string()]
    );
    assert!(pivot_m.row_filterable);
    assert!(!pivot_m.col_filterable);
    // Nimbus value only: 1200.50 + 300.25 (2024-03) + 2400 (2025-01).
    assert_close(pivot_m.grand_total, 3900.75);
}

#[test]
fn price_metric_weighted_averages_use_their_real_denominators() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("metric-denominators.xlsx");
    let db_path = dir.path().join("metric-denominators.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100000000101U1"),
                ("declaration_date", "15.03.2024"),
                ("description", "Metric denominator row A"),
                ("quantity", "10"),
                ("gross_kg", "100"),
                ("net_kg", "1"),
                ("rfv_usd_kg", "10"),
                ("rmv_net_usd_kg", "20"),
                ("rmv_usd_extra_unit", "100"),
                ("rmv_gross_usd_kg", "10"),
                ("min_base_usd_kg", "30"),
            ],
            vec![
                ("declaration_number", "24UA100000000102U2"),
                ("declaration_date", "16.03.2024"),
                ("description", "Metric denominator row B"),
                ("quantity", "1"),
                ("gross_kg", "1"),
                ("net_kg", "100"),
                ("rfv_usd_kg", "20"),
                ("rmv_net_usd_kg", "40"),
                ("rmv_usd_extra_unit", "200"),
                ("rmv_gross_usd_kg", "20"),
                ("min_base_usd_kg", "60"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);

    let analytics = db.analytics(&Query::default(), 10).unwrap();
    assert_close(
        price_metric(&analytics.price_sections, PriceMetricKind::RfvUsdKg).weighted_average,
        (10.0 * 1.0 + 20.0 * 100.0) / 101.0,
    );
    assert_close(
        price_metric(&analytics.price_sections, PriceMetricKind::RmvNetUsdKg).weighted_average,
        (20.0 * 1.0 + 40.0 * 100.0) / 101.0,
    );
    assert_close(
        price_metric(&analytics.price_sections, PriceMetricKind::RmvUsdExtraUnit).weighted_average,
        (100.0 * 10.0 + 200.0 * 1.0) / 11.0,
    );
    assert_close(
        price_metric(&analytics.price_sections, PriceMetricKind::RmvGrossUsdKg).weighted_average,
        (10.0 * 100.0 + 20.0 * 1.0) / 101.0,
    );
    assert_close(
        price_metric(&analytics.price_sections, PriceMetricKind::MinBaseUsdKg).weighted_average,
        (30.0 * 1.0 + 60.0 * 100.0) / 101.0,
    );
}

#[test]
fn undervaluation_flags_rows_below_code_median() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("under.xlsx");
    let db_path = dir.path().join("data").join("under.db");
    // Twenty comparable baseline rows plus one extreme low-price row meet the
    // production minimum sample contract.
    let mut rows = Vec::new();
    for i in 0..=20 {
        let value = if i == 20 {
            "10"
        } else {
            Box::leak(format!("{}", 100 + i).into_boxed_str()) as &str
        };
        rows.push(vec![
            (
                "declaration_number",
                Box::leak(format!("24UA{i:09}U1").into_boxed_str()) as &str,
            ),
            ("declaration_date", "15.03.2024"),
            ("sender", "FOREIGN SUPPLIER LTD"),
            ("edrpou", "55550000"),
            ("recipient", "ТОВ «Тест»"),
            ("product_code", "1234567890"),
            ("description", "Тестовий товар"),
            ("net_kg", "10"),
            ("currency_control_value", value),
            ("contract", "USD"),
            ("unit", "kg"),
        ]);
    }
    write_test_xlsx(&xlsx, &rows);

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 21);

    let shape = db.table_shape().unwrap();
    for (source, semantic) in [
        ("contract", SemanticField::Currency),
        ("unit", SemanticField::WeightUnit),
    ] {
        let id = shape
            .columns
            .iter()
            .find(|column| {
                matches!(&column.storage, ColumnStorage::SchemaColumn(name) if name == source)
            })
            .unwrap()
            .id
            .clone();
        assert!(db.set_column_semantic(&id, Some(semantic)));
    }

    let uv = db.undervaluation(&Query::default(), 0.5, 20, 100).unwrap();
    assert_eq!(uv.checked_codes, 1);
    assert_eq!(uv.rows.len(), 1);
    let flagged = &uv.rows[0];
    assert_eq!(flagged.product_code, "1234567890");
    assert_close(flagged.price_per_kg, 1.0);
    assert_close(flagged.code_median, 10.9);
    assert!(flagged.ratio < 0.5);
}

#[test]
fn generic_table_semantics_drive_undervaluation_scan() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("generic_under.xlsx");
    let db_path = dir.path().join("generic_under.db");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Order Date",
        "Invoice No",
        "Supplier",
        "Buyer",
        "SKU",
        "Description",
        "Amount",
        "Net weight",
        "Currency",
        "Weight unit",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    for r in 0..=20 {
        let row = vec![
            "2024-03-15".to_string(),
            format!("INV-{r:03}"),
            "ACME SUPPLY".to_string(),
            "Buyer A".to_string(),
            "SKU-LOW".to_string(),
            "Generic widget".to_string(),
            if r == 20 {
                "10".to_string()
            } else {
                (100 + r).to_string()
            },
            "10".to_string(),
            "USD".to_string(),
            "kg".to_string(),
        ];
        for (c, v) in row.iter().enumerate() {
            sheet.write_string((r + 1) as u32, c as u16, v).unwrap();
        }
    }
    workbook.save(&xlsx).unwrap();

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.imported, 21);

    for (id, semantic) in [
        ("order_date", SemanticField::Date),
        ("invoice_no", SemanticField::DeclarationNumber),
        ("supplier", SemanticField::Sender),
        ("buyer", SemanticField::Recipient),
        ("sku", SemanticField::ProductCode),
        ("description", SemanticField::Description),
        ("amount", SemanticField::Value),
        ("net_weight", SemanticField::NetWeight),
        ("currency", SemanticField::Currency),
        ("weight_unit", SemanticField::WeightUnit),
    ] {
        assert!(
            db.set_column_semantic(id, Some(semantic)),
            "missing shape column {id}"
        );
    }

    let uv = db.undervaluation(&Query::default(), 0.5, 20, 100).unwrap();
    assert_eq!(uv.checked_codes, 1);
    assert_eq!(uv.rows.len(), 1);
    let flagged = &uv.rows[0];
    assert_eq!(flagged.product_code, "SKU-LOW");
    assert_eq!(flagged.recipient, "Buyer A");
    assert_eq!(flagged.sender, "ACME SUPPLY");
    assert_eq!(flagged.declaration_date, "2024-03-15");
    assert_close(flagged.price_per_kg, 1.0);
    assert_close(flagged.code_median, 10.9);
    assert!(flagged.ratio < 0.5);
}

#[test]
fn analytics_section_can_load_all_group_rows_beyond_visible_top() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("many_recipients.xlsx");
    let db_path = dir.path().join("data").join("many_recipients.db");
    let rows: Vec<Vec<(String, String)>> = (1..=12)
        .map(|idx| {
            vec![
                (
                    "declaration_number".to_string(),
                    format!("24UA100110{:06}U1", idx),
                ),
                ("declaration_date".to_string(), "15.03.2024".to_string()),
                (
                    "sender".to_string(),
                    "NIMBUS DISTRIBUTION INTERNATIONAL LTD".to_string(),
                ),
                ("edrpou".to_string(), format!("{idx:08}")),
                ("recipient".to_string(), format!("NIMBUS IMPORTER {idx:02}")),
                ("product_code".to_string(), "8517130000".to_string()),
                (
                    "description".to_string(),
                    "Nimbus Widget smartphone".to_string(),
                ),
                ("trade_country".to_string(), "IE".to_string()),
                ("dispatch_country".to_string(), "IE".to_string()),
                ("origin_country".to_string(), "CN".to_string()),
                ("quantity".to_string(), "1".to_string()),
                ("gross_kg".to_string(), "1.2".to_string()),
                ("net_kg".to_string(), "1".to_string()),
                (
                    "currency_control_value".to_string(),
                    (idx * 100).to_string(),
                ),
                ("trademark".to_string(), "Nimbus".to_string()),
            ]
        })
        .collect();
    write_owned_test_xlsx(&xlsx, &rows);

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 12);

    let q = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let top = db
        .analytics_scoped(&q, 5, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    let top_recipients = analytics_section(&top.company_sections, AnalyticsSectionKind::Recipients);
    assert_eq!(top_recipients.rows.len(), 5);

    let full = db
        .analytics_section(&q, AnalyticsSectionKind::Recipients, 10, 50)
        .unwrap();
    assert_eq!(full.rows.len(), 12);
    assert_eq!(full.rows[0].label, "NIMBUS IMPORTER 12");
    assert_eq!(full.rows[11].label, "NIMBUS IMPORTER 01");
    let filter = full.rows[0].filter_action.as_ref().unwrap();
    assert_eq!(filter.field, AnalyticsFilterField::Recipient);
    assert_eq!(filter.value, "NIMBUS IMPORTER 12");
}

#[test]
fn analytics_trademark_filter_is_field_specific() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("trademark_filter.xlsx");
    let db_path = dir.path().join("data").join("trademark_filter.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100110000301U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "NIMBUS DISTRIBUTION INTERNATIONAL LTD"),
                ("edrpou", "11111111"),
                ("recipient", "IPHONE UKRAINE LLC"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget smartphone"),
                ("origin_country", "CN"),
                ("net_kg", "10"),
                ("currency_control_value", "1200"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000302U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "ORIFLAME"),
                ("edrpou", "22222222"),
                ("recipient", "COSMETICS IMPORT LLC"),
                ("product_code", "3401110000"),
                ("description", "Soap Nimbus & Cinnamon"),
                ("origin_country", "PL"),
                ("net_kg", "30"),
                ("currency_control_value", "500"),
                ("trademark", "ORIFLAME"),
            ],
            vec![
                ("declaration_number", "24UA100110000303U3"),
                ("declaration_date", "17.03.2024"),
                ("sender", "GLOBAL SUPPLY LTD"),
                ("edrpou", "33333333"),
                ("recipient", "DECOR IMPORT LLC"),
                ("product_code", "9503000000"),
                ("description", "Decorative desk globe"),
                ("origin_country", "CN"),
                ("net_kg", "20"),
                ("currency_control_value", "700"),
                ("trademark", "NIMBUS GLOBE"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 3);

    let broad = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    assert_eq!(db.count(&broad).unwrap(), 3);

    let exact_brand = Query {
        text: String::new(),
        filters: Filters {
            year: "2024".into(),
            trademark: "Nimbus".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let analytics = db.analytics(&exact_brand, 10).unwrap();
    assert_eq!(analytics.overview.row_count, 1);
    assert_close(analytics.overview.total_value_usd, 1200.0);
    assert_eq!(analytics.top_trademarks[0].label, "Nimbus");
}

#[test]
fn analytics_country_sections_use_normalized_country_keys() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("country_keys.xlsx");
    let db_path = dir.path().join("data").join("country_keys.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100110000401U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "NIMBUS DISTRIBUTION INTERNATIONAL LTD"),
                ("edrpou", "11111111"),
                ("recipient", "IPHONE UKRAINE LLC"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget smartphone"),
                ("origin_country", "CN"),
                ("net_kg", "10"),
                ("currency_control_value", "1000"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000402U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "NIMBUS OPERATIONS EUROPE"),
                ("edrpou", "22222222"),
                ("recipient", "TECH IMPORT LLC"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget parts"),
                ("origin_country", "КИТАЙ"),
                ("net_kg", "5"),
                ("currency_control_value", "500"),
                ("trademark", "Nimbus"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);

    let q = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let analytics = db.analytics(&q, 10).unwrap();
    assert_eq!(analytics.overview.distinct_origin_countries, 1);
    let origin = analytics_section(
        &analytics.country_sections,
        AnalyticsSectionKind::OriginCountries,
    );
    assert_eq!(origin.rows.len(), 1);
    assert_eq!(origin.rows[0].label, "CN");
    assert_eq!(origin.rows[0].rows, 2);
}

#[test]
fn analytics_ignores_placeholder_labels_in_business_groups() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("placeholder_labels.xlsx");
    let db_path = dir.path().join("data").join("placeholder_labels.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100110000501U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "0"),
                ("edrpou", "0"),
                ("recipient", "0"),
                ("product_code", "0"),
                ("description", "Nimbus device"),
                ("origin_country", "0"),
                ("net_kg", "10"),
                ("currency_control_value", "10000"),
                ("trademark", "0"),
            ],
            vec![
                ("declaration_number", "24UA100110000502U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "REAL SUPPLIER LTD"),
                ("edrpou", "12345678"),
                ("recipient", "REAL IMPORT LLC"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget"),
                ("origin_country", "CN"),
                ("net_kg", "5"),
                ("currency_control_value", "100"),
                ("trademark", "Nimbus"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);

    let q = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let analytics = db.analytics(&q, 10).unwrap();

    assert_eq!(analytics.overview.row_count, 2);
    assert_eq!(analytics.overview.distinct_senders, 1);
    assert_eq!(analytics.overview.distinct_recipients, 1);
    assert_eq!(analytics.overview.distinct_edrpou, 1);
    assert_eq!(analytics.overview.distinct_trademarks, 1);
    assert_eq!(analytics.overview.distinct_product_codes, 1);
    assert_eq!(analytics.overview.distinct_origin_countries, 1);

    let senders = analytics_section(&analytics.company_sections, AnalyticsSectionKind::Senders);
    assert_eq!(senders.rows[0].label, "REAL SUPPLIER LTD");
    let edrpou = analytics_section(&analytics.company_sections, AnalyticsSectionKind::Edrpou);
    assert_eq!(edrpou.rows[0].label, "12345678");
    let trademarks = analytics_section(
        &analytics.product_sections,
        AnalyticsSectionKind::Trademarks,
    );
    assert_eq!(trademarks.rows[0].label, "Nimbus");
}

#[test]
fn analytics_builds_decision_sections_for_trade_questions() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("analytics_sections.xlsx");
    let db_path = dir.path().join("data").join("analytics_sections.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA100110000201U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "NIMBUS DISTRIBUTION INTERNATIONAL LTD"),
                ("edrpou", "11111111"),
                ("recipient", "IPHONE UKRAINE LLC"),
                ("product_code", "8517130000"),
                ("description", "Nimbus Widget 15 smartphone"),
                ("trade_country", "IE"),
                ("dispatch_country", "IE"),
                ("origin_country", "CN"),
                ("quantity", "10"),
                ("gross_kg", "12.5"),
                ("net_kg", "10"),
                ("currency_control_value", "1 200.50"),
                ("rfv_usd_kg", "120.05"),
                ("rmv_net_usd_kg", "119.5"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000201U1"),
                ("declaration_date", "16.03.2024"),
                ("sender", "NIMBUS OPERATIONS EUROPE"),
                ("edrpou", "22222222"),
                ("recipient", "TECH IMPORT LLC"),
                ("product_code", "8517790000"),
                ("description", "Nimbus Widget parts"),
                ("trade_country", "IE"),
                ("dispatch_country", "PL"),
                ("origin_country", "US"),
                ("quantity", "2"),
                ("gross_kg", "3,5"),
                ("net_kg", "2,5"),
                ("currency_control_value", "300,25"),
                ("rfv_usd_kg", "120.1"),
                ("rmv_net_usd_kg", "121"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000202U2"),
                ("declaration_date", "17.03.2024"),
                ("sender", "NIMBUS OPERATIONS EUROPE"),
                ("edrpou", "22222222"),
                ("recipient", "TECH IMPORT LLC"),
                ("product_code", "8517790000"),
                ("description", "Nimbus service replacement unit"),
                ("trade_country", "US"),
                ("dispatch_country", "US"),
                ("origin_country", "US"),
                ("quantity", "1"),
                ("gross_kg", "bad"),
                ("net_kg", ""),
                ("currency_control_value", "not a number"),
                ("rfv_usd_kg", ""),
                ("rmv_net_usd_kg", "bad"),
                ("trademark", "Nimbus"),
            ],
            vec![
                ("declaration_number", "24UA100110000203U3"),
                ("declaration_date", "17.03.2024"),
                ("sender", "SAMSUNG ELECTRONICS"),
                ("edrpou", "33333333"),
                ("recipient", "TECH IMPORT LLC"),
                ("product_code", "8517130000"),
                ("description", "Samsung smartphone"),
                ("trade_country", "KR"),
                ("dispatch_country", "KR"),
                ("origin_country", "VN"),
                ("quantity", "5"),
                ("gross_kg", "7"),
                ("net_kg", "6"),
                ("currency_control_value", "700"),
                ("trademark", "Samsung"),
            ],
        ],
    );

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 4);

    let q = Query {
        text: "Nimbus".into(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        advanced: None,
        record_scope: Default::default(),
    };
    let analytics = db.analytics(&q, 10).unwrap();
    assert_eq!(analytics.overview.row_count, 3);
    assert_eq!(analytics.overview.declaration_count, 2);
    assert_eq!(analytics.overview.distinct_senders, 2);
    assert_eq!(analytics.overview.distinct_recipients, 2);
    assert_eq!(analytics.overview.distinct_edrpou, 2);
    assert_eq!(analytics.overview.distinct_product_codes, 2);
    assert_eq!(analytics.overview.distinct_trademarks, 1);
    assert_eq!(analytics.overview.distinct_origin_countries, 2);
    assert_eq!(analytics.overview.distinct_dispatch_countries, 3);
    assert_eq!(analytics.overview.distinct_trade_countries, 2);
    assert_close(analytics.overview.total_value_usd, 1500.75);
    assert_close(analytics.overview.total_net_kg, 12.5);
    assert_close(analytics.overview.avg_value_per_net_kg, 120.06);

    let recipients = analytics_section(
        &analytics.company_sections,
        AnalyticsSectionKind::Recipients,
    );
    assert_eq!(recipients.rows[0].label, "IPHONE UKRAINE LLC");
    assert_close(recipients.rows[0].share_percent, 79.9933);
    assert_eq!(recipients.rows[0].declarations, 1);
    assert_close(recipients.rows[0].avg_value_per_net_kg, 120.05);
    let filter = recipients.rows[0].filter_action.as_ref().unwrap();
    assert_eq!(filter.field, AnalyticsFilterField::Recipient);
    assert_eq!(filter.value, "IPHONE UKRAINE LLC");

    let codes = analytics_section(
        &analytics.product_sections,
        AnalyticsSectionKind::ProductCodes,
    );
    assert_eq!(codes.rows[0].label, "8517130000");
    assert_eq!(codes.rows[0].companies, 1);
    assert_eq!(codes.rows[1].label, "8517790000");
    assert_eq!(codes.rows[1].companies, 1);

    let trademarks = analytics_section(
        &analytics.product_sections,
        AnalyticsSectionKind::Trademarks,
    );
    assert_eq!(trademarks.rows[0].label, "Nimbus");
    assert_eq!(trademarks.rows[0].companies, 2);

    let origin = analytics_section(
        &analytics.country_sections,
        AnalyticsSectionKind::OriginCountries,
    );
    assert_eq!(origin.rows[0].label, "CN");
    assert_eq!(origin.rows[1].label, "US");
    let dispatch = analytics_section(
        &analytics.country_sections,
        AnalyticsSectionKind::DispatchCountries,
    );
    assert_eq!(dispatch.rows.len(), 3);
    let trade = analytics_section(
        &analytics.country_sections,
        AnalyticsSectionKind::TradeCountries,
    );
    assert_eq!(trade.rows[0].label, "IE");

    let value_per_kg = price_metric(&analytics.price_sections, PriceMetricKind::ValuePerNetKg);
    assert_eq!(value_per_kg.count, 2);
    assert_close(value_per_kg.average, 120.075);
    assert_close(value_per_kg.minimum, 120.05);
    assert_close(value_per_kg.maximum, 120.1);
    assert_close(value_per_kg.weighted_average, 120.06);

    let rfv = price_metric(&analytics.price_sections, PriceMetricKind::RfvUsdKg);
    assert_eq!(rfv.count, 2);
    assert_close(rfv.average, 120.075);
    assert_close(rfv.minimum, 120.05);
    assert_close(rfv.maximum, 120.1);
    let rmv = price_metric(&analytics.price_sections, PriceMetricKind::RmvNetUsdKg);
    assert_eq!(rmv.count, 2);
    assert_close(rmv.average, 120.25);

    assert!(!analytics_should_run(&Query::default()));
    assert!(analytics_should_run(&q));
}

/// Shape-driven analytics: a generic table's value column lives in `extra` and
/// carries no meaning until the user assigns it. Once assigned, analytics sums
/// it — proving analytics is driven by the recorded shape, not only the fixed
/// customs schema.
#[test]
fn user_assigned_semantic_drives_generic_table_analytics() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("generic_value.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Дата",
        "Номер декларації",
        "Відправник",
        "Одержувач",
        "Код товару",
        "Опис товару",
        "Custom metric X", // deliberately unknown until the user maps it
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let rows = [
        [
            "15.03.2024",
            "UA1/2024/1",
            "ACME",
            "ТОВ A",
            "8504405500",
            "Widget one",
            "1500.50",
        ],
        [
            "16.03.2024",
            "UA1/2024/2",
            "ACME",
            "ТОВ B",
            "8504405500",
            "Widget two",
            "1000",
        ],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            sheet.write_string(r as u32 + 1, c as u16, *v).unwrap();
        }
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("generic_value.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);

    let q = Query {
        text: String::new(),
        filters: Filters {
            year: "2024".into(),
            ..Default::default()
        },
        ..Default::default()
    };

    // No value semantic yet -> the total is zero.
    let before = db.analytics_scoped(&q, 10, None, 10).unwrap();
    assert_eq!(before.overview.row_count, 2);
    assert_close(before.overview.total_value_usd, 0.0);

    // The generic column was captured into the shape with no meaning.
    let shape = db.table_shape().expect("table shape recorded");
    let column = shape
        .columns
        .iter()
        .find(|column| column.id == "custom_metric_x")
        .expect("custom metric column in shape");
    assert!(column.semantic.is_none());

    // The user assigns it the value meaning (what the column-mapping UI does).
    assert!(db.set_column_semantic("custom_metric_x", Some(SemanticField::Value)));

    // Analytics now sums the value from the extra JSON for this generic table.
    let after = db.analytics_scoped(&q, 10, None, 10).unwrap();
    assert_close(after.overview.total_value_usd, 2500.50);
}

#[test]
fn generic_table_semantics_drive_sections_and_months() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("generic_analytics.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Order Date",
        "Invoice No",
        "Supplier Name",
        "Buyer Name",
        "Buyer Code",
        "SKU",
        "Item Description",
        "Amount USD",
        "Net kg",
        "Ship From",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let rows = [
        [
            "15.03.2024",
            "INV-001",
            "ACME SUPPLY",
            "Buyer A",
            "BUYER-1",
            "SKU-42",
            "Widget one",
            "1500.50",
            "10",
            "China",
        ],
        [
            "2024/04/16",
            "INV-002",
            "ACME SUPPLY",
            "Buyer B",
            "BUYER-2",
            "SKU-42",
            "Widget two",
            "1000",
            "5",
            "CN",
        ],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            sheet.write_string(r as u32 + 1, c as u16, *v).unwrap();
        }
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("generic_analytics.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.imported, 2);

    for (id, semantic) in [
        ("order_date", SemanticField::Date),
        ("invoice_no", SemanticField::DeclarationNumber),
        ("supplier_name", SemanticField::Sender),
        ("buyer_name", SemanticField::Recipient),
        ("buyer_code", SemanticField::CompanyCode),
        ("sku", SemanticField::ProductCode),
        ("item_description", SemanticField::Description),
        ("amount_usd", SemanticField::Value),
        ("net_kg", SemanticField::NetWeight),
        ("ship_from", SemanticField::OriginCountry),
    ] {
        assert!(
            db.set_column_semantic(id, Some(semantic)),
            "missing shape column {id}"
        );
    }

    let analytics = db.analytics(&Query::default(), 10).unwrap();
    assert_eq!(analytics.overview.row_count, 2);
    assert_eq!(analytics.overview.declaration_count, 2);
    assert_eq!(analytics.overview.distinct_senders, 1);
    assert_eq!(analytics.overview.distinct_recipients, 2);
    assert_eq!(analytics.overview.distinct_edrpou, 2);
    assert_eq!(analytics.overview.distinct_product_codes, 1);
    assert_eq!(analytics.overview.distinct_origin_countries, 1);
    assert_close(analytics.overview.total_value_usd, 2500.50);
    assert_close(analytics.overview.total_net_kg, 15.0);

    assert_eq!(
        analytics
            .months
            .iter()
            .map(|row| row.month.as_str())
            .collect::<Vec<_>>(),
        vec!["2024-03", "2024-04"]
    );
    assert_close(analytics.months[0].total_value_usd, 1500.50);
    assert_close(analytics.months[1].total_value_usd, 1000.0);

    let recipients = analytics_section(
        &analytics.company_sections,
        AnalyticsSectionKind::Recipients,
    );
    assert_eq!(recipients.rows.len(), 2);
    assert_eq!(recipients.rows[0].label, "Buyer A");
    assert_close(recipients.rows[0].total_value_usd, 1500.50);
    assert!(recipients.rows[0].filter_action.is_some());

    let products = analytics_section(
        &analytics.product_sections,
        AnalyticsSectionKind::ProductCodes,
    );
    assert_eq!(products.rows[0].label, "SKU-42");
    assert_eq!(products.rows[0].rows, 2);
    assert_close(products.rows[0].total_value_usd, 2500.50);

    let origin = analytics_section(
        &analytics.country_sections,
        AnalyticsSectionKind::OriginCountries,
    );
    assert_eq!(origin.rows[0].label, "CN");
    assert_eq!(origin.rows[0].rows, 2);

    let pivot = db
        .pivot(
            &Query::default(),
            PivotDim::Recipient,
            PivotDim::Month,
            PivotMetric::Value,
            PivotLimits { rows: 25, cols: 18 },
            "others",
        )
        .unwrap();
    assert_eq!(
        pivot.row_labels,
        vec!["Buyer A".to_string(), "Buyer B".to_string()]
    );
    assert_eq!(
        pivot.col_labels,
        vec!["2024-03".to_string(), "2024-04".to_string()]
    );
    assert_close(pivot.grand_total, 2500.50);
    assert_close(pivot.cells[0][0], 1500.50);
    assert_close(pivot.cells[1][1], 1000.0);
    assert!(pivot.row_filterable);
    assert!(!pivot.col_filterable);

    let value_per_kg = price_metric(&analytics.price_sections, PriceMetricKind::ValuePerNetKg);
    assert_eq!(value_per_kg.count, 2);
    assert_close(value_per_kg.average, 175.025);
    assert_close(value_per_kg.weighted_average, 166.7);

    let profile = db.company_profile("BUYER-1", 10).unwrap();
    assert_eq!(profile.edrpou, "BUYER-1");
    assert_eq!(profile.names, vec!["Buyer A".to_string()]);
    assert_eq!(profile.overview.row_count, 1);
    assert_close(profile.overview.total_value_usd, 1500.50);
    assert_close(profile.overview.total_net_kg, 10.0);
    assert_eq!(profile.top_products[0].label, "SKU-42");
    assert_eq!(
        profile
            .months
            .iter()
            .map(|row| row.month.as_str())
            .collect::<Vec<_>>(),
        vec!["2024-03"]
    );
}

#[test]
fn common_generic_headers_auto_drive_analytics() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("common_headers.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = ["Brand", "Recipient", "Product code", "Value USD", "Net kg"];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let rows = [
        ["Apple", "Alpha Import LLC", "851713", "1200", "10"],
        ["Apple", "Beta Trade LLC", "851713", "900", "8"],
        ["Samsung", "Alpha Import LLC", "851714", "700", "7"],
        ["Apple", "Gamma Market LLC", "847130", "1500", "5"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            sheet.write_string(r as u32 + 1, c as u16, *v).unwrap();
        }
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("common_headers.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.imported, 4);

    let shape = db.table_shape().expect("table shape recorded");
    for (id, semantic) in [
        ("brand", SemanticField::Trademark),
        ("recipient", SemanticField::Recipient),
        ("product_code", SemanticField::ProductCode),
        ("value_usd", SemanticField::Value),
        ("net_kg", SemanticField::NetWeight),
    ] {
        let column = shape
            .columns
            .iter()
            .find(|column| column.id == id)
            .unwrap_or_else(|| panic!("missing shape column {id}"));
        assert_eq!(column.semantic, Some(semantic));
    }

    let query = Query {
        text: "Apple".into(),
        ..Default::default()
    };
    let analytics = db.analytics(&query, 10).unwrap();
    assert_eq!(analytics.overview.row_count, 3);
    assert_eq!(analytics.overview.distinct_recipients, 3);
    assert_eq!(analytics.overview.distinct_product_codes, 2);
    assert_eq!(analytics.overview.distinct_trademarks, 1);
    assert_close(analytics.overview.total_value_usd, 3600.0);
    assert_close(analytics.overview.total_net_kg, 23.0);

    let recipients = analytics_section(
        &analytics.company_sections,
        AnalyticsSectionKind::Recipients,
    );
    assert_eq!(recipients.rows.len(), 3);
    assert_eq!(recipients.rows[0].label, "Gamma Market LLC");
    assert_close(recipients.rows[0].total_value_usd, 1500.0);

    let products = analytics_section(
        &analytics.product_sections,
        AnalyticsSectionKind::ProductCodes,
    );
    assert_eq!(products.rows[0].label, "851713");
    assert_eq!(products.rows[0].rows, 2);
    assert_close(products.rows[0].total_value_usd, 2100.0);
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 0.0001,
        "expected {expected}, got {actual}"
    );
}

fn analytics_section(
    sections: &[base_search::db::AnalyticsSection],
    kind: AnalyticsSectionKind,
) -> &base_search::db::AnalyticsSection {
    sections
        .iter()
        .find(|section| section.kind == kind)
        .unwrap_or_else(|| panic!("missing section {kind:?}"))
}

fn price_metric(
    metrics: &[base_search::db::AnalyticsPriceMetric],
    kind: PriceMetricKind,
) -> &base_search::db::AnalyticsPriceMetric {
    metrics
        .iter()
        .find(|metric| metric.kind == kind)
        .unwrap_or_else(|| panic!("missing metric {kind:?}"))
}

/// Registry-style format: repeated headers, a declaration number split into
/// three parts, and an Excel serial date.
#[test]
fn import_registry_format() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("registry.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Тип декларації",
        "Тип декларації",
        "Номер декларації",
        "Номер декларації",
        "Номер декларації",
        "Дата оформлення",
        "Митниця оформлення",
        "Торгуюча країна",
        "Код товара",
        "Найменування товару",
        "Отримувач",
        "Отримувач",
        "Відпправник",
        "Вага брутто, кг",
        "Вага нетто, кг",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let row = [
        "40",
        "ДЕ",
        "UA209230",
        "2024",
        "102880",
        "",
        "ЛЬВІВСЬКА МИТНИЦЯ",
        "ПОЛЬЩА",
        "9405500000",
        "Освітлювальні прилади неелектричні",
        "37642136",
        "ТОВ ЮСК УКРАЇНА",
        "JYSK SP Z O O",
        "",
        "",
    ];
    for (c, v) in row.iter().enumerate() {
        if !v.is_empty() {
            sheet.write_string(1, c as u16, *v).unwrap();
        }
    }
    sheet.write_number(1, 5, 45597.0).unwrap(); // date = 2024-11-01
    sheet.write_number(1, 13, 27.95).unwrap();
    sheet.write_number(1, 14, 27.391).unwrap();
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = Query {
        text: "освітлювальні".into(),
        ..Default::default()
    };
    let (ids, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][result_col("declaration_date")], "2024-11-01"); // date converted from serial number
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA209230/2024/102880"
    ); // declaration number joined
    assert_eq!(rows[0][result_col("edrpou")], "37642136"); // EDRPOU is read from the first recipient column
    assert_eq!(rows[0][result_col("declaration_type")], "40/ДЕ");

    let card = db.record_card(ids[0]).unwrap();
    let has = |value: &str| card.fields.iter().any(|(_, v)| v == value);
    assert!(has("ТОВ ЮСК УКРАЇНА"));
    assert!(has("JYSK SP Z O O"));
    assert!(has("40"));
    assert!(has("ДЕ"));
    assert!(has("27.95"));

    // The year is extracted from the converted date.
    let filters = Filters {
        year: "2024".into(),
        ..Default::default()
    };
    assert_eq!(
        db.count(&Query {
            text: String::new(),
            filters,
            advanced: None,
            record_scope: Default::default(),
        })
        .unwrap(),
        1
    );
}

/// Registry-style variant: product-name column, separate recipient code/name
/// columns, and known source typos in declaration and sender headers.
#[test]
fn import_registry_format_im12_variant() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("registry12.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Тип декларації",
        "Тип декларації",
        "Номер декларації",
        "Номер деклараціх",
        "Номер декларації",
        "Дата оформлення",
        "Митниця оформлення",
        "Код товару",
        "Назва товару",
        "Код фірми отримувача",
        "Назва фірми отримувача",
        "Назва фірми відправиника",
        "Вартість, $/кг",
        "Ознака товару  в контейнері",
        "Метод визначення митної вартості",
        "Фактурна вартість, $",
        "Мито, грн.",
        "Акциз, грн.",
        "ПДВ, грн.",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let row = [
        "40",
        "АА",
        "UA209060",
        "",
        "1479",
        "",
        "ЛЬВІВСЬКА МИТНИЦЯ",
        "7005103000",
        "СКЛО CLIMAGUARD PREMIUM2",
        "32818783",
        "ТОВ ГЮАЛОС",
        "GUARDIAN CZESTOCHOWA SP Z O O",
        "",
        "0",
        "1",
        "",
        "120.5",
        "0",
        "",
    ];
    for (c, v) in row.iter().enumerate() {
        if !v.is_empty() {
            sheet.write_string(1, c as u16, *v).unwrap();
        }
    }
    sheet.write_number(1, 3, 2024.0).unwrap(); // middle declaration-number part
    sheet.write_number(1, 5, 45627.0).unwrap(); // date = 2024-12-01
    sheet.write_number(1, 12, 0.5756).unwrap();
    sheet.write_number(1, 15, 8473.2).unwrap();
    sheet.write_number(1, 18, 74216.87).unwrap();
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = Query {
        text: "climaguard".into(),
        ..Default::default()
    };
    let (ids, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][result_col("declaration_date")], "2024-12-01");
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA209060/2024/1479"
    ); // all 3 parts, including the typo column
    assert_eq!(rows[0][result_col("edrpou")], "32818783");
    assert_eq!(rows[0][result_col("declaration_type")], "40/АА");

    let card = db.record_card(ids[0]).unwrap();
    let has = |value: &str| card.fields.iter().any(|(_, v)| v == value);
    assert!(has("ТОВ ГЮАЛОС"));
    assert!(has("GUARDIAN CZESTOCHOWA SP Z O O"));
    assert!(has("40"));
    assert!(has("АА"));
    assert!(has("0.5756"));
    assert!(has("8473.2"));
    assert!(has("0"));
    assert!(has("1"));
    assert!(has("120.5"));
    assert!(has("74216.87"));
}

#[test]
fn import_wide_2026_import_layout_maps_participants() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("wide-import-2026.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Тип декларации",
        "Тип декларации",
        "Тип декларации",
        "Тип декларации",
        "Номер декларации",
        "Номер декларации",
        "Номер декларации",
        "Дата оформления",
        "Таможня оформления",
        "Пост оформления",
        "Торгующая страна",
        "Страна отправления",
        "КРАЇНА ПОХОДЖЕННЯ",
        "ОЗНАКА ТОВАРУ В КОНТЕЙНЕРІ",
        "НОМЕР КОНТЕЙНЕРА",
        "Условия поставки",
        "Условия поставки",
        "Валюта контракта",
        "Валюта контракта",
        "Транспорт на границе",
        "Транспорт на границе",
        "Транспорт на границе",
        "Транспорт на территории страны",
        "Транспорт на территории страны",
        "Транспорт на границе",
        "Таможня на границе",
        "Таможня на границе",
        "Пост на границе",
        "Номер товара",
        "КОД ТОВАРУ",
        "ОПИС ТОВАРУ",
        "Получатель",
        "ОТРИМУВАЧ",
        "Обратная сторона",
        "",
        "",
        "ВІДПРАВНИК",
        "Контрактодержатель",
        "Контрактодержатель",
        "Метод",
        "Доп.ед",
        "Доп.ед",
        "Вес брутто, кг",
        "Вес нетто, кг",
        "Фактурная стоимость, грн.",
        "Фактурная стоимость, $",
        "Курс, $",
        "Таможенная стоимость, грн.",
        "Таможенная стоимость, $",
        "ЦІНА, $/кг",
        "Пошлина, грн.",
        "Акциз, грн.",
        "ПДВ, грн.",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let row = [
        "40",
        "0",
        "ZZ",
        "AA",
        "UA305090",
        "2026",
        "1160",
        "2026-03-01",
        "ЗАКАРПАТСЬКА МИТНИЦЯ",
        "ПУНКТ ПРОПУСКУ УЖГОРОД",
        "СЛОВАЧЧИНА",
        "СЛОВАЧЧИНА",
        "КИТАЙ",
        "",
        "",
        "DAP",
        "UA KVITNEVE",
        "980",
        "UAH",
        "30",
        "ВАНТАЖНИЙ АВТОМОБІЛЬ",
        "AO5940XP",
        "0",
        "НЕВІДОМИЙ",
        "",
        "UA305090",
        "ЗАКАРПАТСЬКА МИТНИЦЯ",
        "ПУНКТ ПРОПУСКУ УЖГОРОД",
        "14",
        "8516310090",
        "МАШИНИ ЕЛЕКТРОМЕХАНІЧНІ ПОБУТОВІ",
        "34474821",
        "ТОВ ГРУП СЕБ УКРАЇНА",
        "",
        "",
        "",
        "GROUPE SEB SLOVENSKO SPOL S R O",
        "34474821",
        "ТОВ ГРУП СЕБ УКРАЇНА",
        "1",
        "96",
        "ШТ",
        "92.45",
        "67.68",
        "45432.96",
        "1051.49",
        "43.208",
        "45476.12",
        "1052.49",
        "15.55",
        "909.52",
        "0",
        "9277.13",
    ];
    for (c, v) in row.iter().enumerate() {
        sheet.write_string(1, c as u16, *v).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = Query {
        text: "seb".into(),
        ..Default::default()
    };
    let (ids, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA305090/2026/1160"
    );
    assert_eq!(rows[0][result_col("declaration_type")], "40/0/ZZ/AA");
    assert_eq!(rows[0][result_col("edrpou")], "34474821");

    let card = db.record_card(ids[0]).unwrap();
    let has = |value: &str| card.fields.iter().any(|(_, v)| v == value);
    assert!(has("ТОВ ГРУП СЕБ УКРАЇНА"));
    assert!(has("GROUPE SEB SLOVENSKO SPOL S R O"));
    assert!(has("1051.49"));
    assert!(has("1"));
    assert!(has("15.55"));
    assert!(has("909.52"));
    assert!(has("9277.13"));
}

#[test]
fn import_wide_2026_export_layout_maps_sender_company() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("wide-export-2026.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Тип декларации",
        "Тип декларации",
        "Тип декларации",
        "Тип декларации",
        "Номер декларации",
        "Номер декларации",
        "Номер декларации",
        "Дата оформления",
        "Таможня оформления",
        "Пост оформления",
        "Торгующая страна",
        "Страна отправления",
        "КРАЇНА ПРИХНАЧЕННЯ",
        "ОЗНАКА ТОВАРУ В КОНТЕЙНЕРІ",
        "НОМЕР КОЛНТЕЙНЕРА",
        "Условия поставки",
        "Условия поставки",
        "Валюта контракта",
        "Валюта контракта",
        "Транспорт на границе",
        "Транспорт на границе",
        "Транспорт на границе",
        "Транспорт на территории страны",
        "Транспорт на территории страны",
        "Транспорт на границе",
        "Таможня на границе",
        "Таможня на границе",
        "Пост на границе",
        "Номер товара",
        "КОД ТОВАРУ",
        "ОПИС ТОВАРУ",
        "Отправитель",
        "ВІДПРАВНИК",
        "Обратная сторона",
        "",
        "",
        "ОТРИМУВАЧ",
        "Контрактодержатель",
        "Контрактодержатель",
        "Метод",
        "Доп.ед",
        "Доп.ед",
        "Вес брутто, кг",
        "Вес нетто, кг",
        "Фактурная стоимость, грн.",
        "Фактурная стоимость, $",
        "Курс, $",
        "Таможенная стоимость, грн.",
        "Таможенная стоимость, $",
        "ЦІНА, $/кг",
        "Пошлина, грн.",
        "Акциз, грн.",
        "ПДВ, грн.",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let row = [
        "10",
        "0",
        "ZZ",
        "AA",
        "UA401060",
        "2026",
        "2699",
        "2026-03-01",
        "ВІННИЦЬКА МИТНИЦЯ",
        "МИТНИЙ ПОСТ ГАЙСИН",
        "ВІРМЕНІЯ",
        "УКРАЇНА",
        "ВІРМЕНІЯ",
        "",
        "",
        "FCA",
        "UA ЛАДИЖИН",
        "840",
        "USD",
        "30",
        "ВАНТАЖНИЙ АВТОМОБІЛЬ",
        "CE7554EX",
        "30",
        "ВАНТАЖНИЙ АВТОМОБІЛЬ",
        "CE7554EX",
        "UA408050",
        "ЧЕРНІВЕЦЬКА МИТНИЦЯ",
        "ПУНКТ ПРОПУСКУ ПОРУБНЕ",
        "2",
        "207129000",
        "ТУШКА КУРЧАТИ БРОЙЛЕРА",
        "30830662",
        "ПРАТ МИРОНІВСЬКА ПФ",
        "",
        "",
        "",
        "ALEX AND HOLDING LLC",
        "30830662",
        "ПРАТ МИРОНІВСЬКА ПФ",
        "",
        "0",
        "КГ",
        "10367.93",
        "9386",
        "567771.72",
        "13140.4",
        "43.208",
        "567771.72",
        "13140",
        "1.3999",
        "0",
        "",
        "",
    ];
    for (c, v) in row.iter().enumerate() {
        sheet.write_string(1, c as u16, *v).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = Query {
        text: "миронівська".into(),
        ..Default::default()
    };
    let (ids, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA401060/2026/2699"
    );
    assert_eq!(rows[0][result_col("edrpou")], "30830662");

    let card = db.record_card(ids[0]).unwrap();
    let has = |value: &str| card.fields.iter().any(|(_, v)| v == value);
    assert!(has("ПРАТ МИРОНІВСЬКА ПФ"));
    assert!(has("ALEX AND HOLDING LLC"));
    assert!(has("ВІРМЕНІЯ"));
    assert!(has("13140.4"));
}

/// Generic detector: an external export with Russian headers and title rows
/// above the actual table header.
#[test]
fn import_generic_format_with_title_rows() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("generic.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    // Two title rows before the table header.
    sheet
        .write_string(0, 0, "Реестр импортных операций")
        .unwrap();
    sheet.write_string(1, 0, "за март 2024 года").unwrap();
    let headers = [
        "Дата",
        "Номер декларации",
        "Отправитель",
        "Получатель",
        "Код ТН ВЭД",
        "Описание товара",
        "Страна происхождения",
        "Вес нетто",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(2, c as u16, *h).unwrap();
    }
    let row = [
        "15.03.2024",
        "UA100100/2024/55555",
        "ACME GMBH",
        "ООО Ромашка",
        "8504405500",
        "Преобразователь напряжения статический",
        "DE",
        "12.5",
    ];
    for (c, v) in row.iter().enumerate() {
        sheet.write_string(3, c as u16, *v).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = Query {
        text: "преобразователь".into(),
        ..Default::default()
    };
    let (ids, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][result_col("declaration_date")], "2024-03-15");
    assert_eq!(
        rows[0][result_col("declaration_number")],
        "UA100100/2024/55555"
    );
    let card = db.record_card(ids[0]).unwrap();
    assert!(card.fields.iter().any(|(_, v)| v == "ACME GMBH"));
}

#[test]
fn import_preview_uses_the_same_detected_header_as_the_real_import() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("preview-title-rows.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .write_string(0, 0, "Quarterly inventory report")
        .unwrap();
    sheet.write_string(1, 0, "Generated automatically").unwrap();
    for (column, header) in ["SKU", "Product name", "Warehouse", "Quantity"]
        .iter()
        .enumerate()
    {
        sheet.write_string(2, column as u16, *header).unwrap();
    }
    for (column, value) in ["A-42", "Desktop monitor", "Kyiv", "12"].iter().enumerate() {
        sheet.write_string(3, column as u16, *value).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let preview = import::peek_file(&xlsx, 5).unwrap();
    assert_eq!(preview.sheets.len(), 1);
    assert_eq!(preview.sheets[0].header_row, 3);
    assert_eq!(preview.sheets[0].layout, "generic table");
    assert_eq!(
        preview.sheets[0]
            .columns
            .iter()
            .map(|column| column.header.as_str())
            .collect::<Vec<_>>(),
        vec!["SKU", "Product name", "Warehouse", "Quantity"]
    );
    assert_eq!(preview.sheets[0].columns[0].sample, "A-42");
    assert_eq!(
        preview.sheets[0].columns[0].semantic,
        Some(SemanticField::ProductCode)
    );
    assert_eq!(preview.sheets[0].columns[0].role, ColumnRole::Code);
}

#[test]
fn delimited_preview_and_import_detect_the_same_title_rows_and_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("preview-title-rows.csv");
    std::fs::write(
        &csv,
        "Quarterly inventory report;;;\nGenerated automatically;;;\nSKU;Product name;Warehouse;Quantity\nA-42;Desktop monitor;Kyiv;12\n",
    )
    .unwrap();

    let preview = import::peek_file(&csv, 5).unwrap();
    assert_eq!(preview.sheets[0].header_row, 3);
    assert_eq!(preview.sheets[0].columns[0].header, "SKU");
    assert_eq!(preview.sheets[0].columns[0].sample, "A-42");
    assert_eq!(
        preview.sheets[0].columns[3].semantic,
        Some(SemanticField::Quantity)
    );

    let mut db = Db::open(&dir.path().join("preview-title-rows.db")).unwrap();
    let summary = import::import_file(&mut db, &csv, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.quality.header_row, 3);
    assert_eq!(summary.imported, 1);
}

#[test]
fn manual_sheet_mapping_materializes_semantics_for_unknown_headers() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("manual-mapping.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet().set_name("Data").unwrap();
    for (column, header) in ["Alpha", "Beta", "Gamma"].iter().enumerate() {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    for (column, value) in ["2024-03-15", "ACME IMPORTS", "1,250.50"]
        .iter()
        .enumerate()
    {
        sheet.write_string(1, column as u16, *value).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let preview = import::peek_file(&xlsx, 5).unwrap();
    assert!(
        preview.sheets[0]
            .columns
            .iter()
            .all(|column| column.semantic.is_none())
    );

    let options = import::ImportOptions::selected_sheets(["Data"]).with_sheet_semantics(
        "Data",
        [
            (0, Some(SemanticField::Date)),
            (1, Some(SemanticField::Recipient)),
            (2, Some(SemanticField::Value)),
        ],
    );
    let mut db = Db::open(&dir.path().join("manual-mapping.db")).unwrap();
    let summary = import::import_file_with_options(
        &mut db,
        &xlsx,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let query = Query {
        filters: Filters {
            year: "2024".to_string(),
            recipient: "ACME IMPORTS".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(db.count(&query).unwrap(), 1);
    let analytics = db.analytics(&query, 10).unwrap();
    assert!((analytics.overview.total_value_usd - 1250.50).abs() < 0.001);

    let retry = import::import_file_with_options(
        &mut db,
        &xlsx,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert_eq!(retry.imported, 0);
    assert!(retry.skipped_duplicate_of.is_some());
}

/// Universality: columns this build does not model are still imported verbatim
/// into the `extra` store, shown on the record card, and reachable by search.
#[test]
fn import_captures_unmapped_columns_as_extra() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("extra.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Дата",
        "Номер декларації",
        "Відправник",
        "Одержувач",
        "Код товару",
        "Опис товару",
        "Контейнер",     // not in the schema -> captured as extra
        "Номер інвойсу", // not in the schema -> captured as extra
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let row = [
        "15.03.2024",
        "UA100100/2024/77777",
        "ACME LTD",
        "ТОВ Тест",
        "8504405500",
        "Static converter",
        "CONTAINERX9Z",
        "INVOICEQ7W",
    ];
    for (c, v) in row.iter().enumerate() {
        sheet.write_string(1, c as u16, *v).unwrap();
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("extra.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let q = |text: &str| Query {
        text: text.into(),
        ..Default::default()
    };
    let (ids, _rows, _dups) = db.search_page(&q("converter"), 10, 0).unwrap();
    assert_eq!(ids.len(), 1);

    // Unmapped columns are preserved as first-class source fields on the card,
    // in file order.
    let card = db.record_card(ids[0]).unwrap();
    let card_pairs: Vec<(String, String)> = card
        .fields
        .iter()
        .filter(|(h, _)| h == "Контейнер" || h == "Номер інвойсу")
        .cloned()
        .collect();
    assert_eq!(
        card_pairs,
        vec![
            ("Контейнер".to_string(), "CONTAINERX9Z".to_string()),
            ("Номер інвойсу".to_string(), "INVOICEQ7W".to_string()),
        ]
    );
    assert!(card.extra.is_empty());

    // ...and they are reachable through full-text search.
    assert_eq!(db.count(&q("CONTAINERX9Z")).unwrap(), 1);
    assert_eq!(db.count(&q("INVOICEQ7W")).unwrap(), 1);

    let catalog = db.field_catalog().unwrap();
    assert!(catalog.iter().any(|field| field.label == headers[6]
        && field.id.starts_with("source:")
        && field.source == FieldRef::Extra(headers[6].to_string())));
    let extra_query = Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::Extra(headers[6].to_string()),
            op: ConditionOp::Equals,
            value: ConditionValue::Single("CONTAINERX9Z".into()),
            negated: false,
        })),
        ..Query::default()
    };
    assert_eq!(db.count(&extra_query).unwrap(), 1);

    // A fully-mapped file produces no extra columns.
    let full = dir.path().join("full.xlsx");
    let full_db = dir.path().join("full.db");
    write_test_xlsx(&full, &sample_rows());
    let mut db2 = Db::open(&full_db).unwrap();
    import::import_file(&mut db2, &full, &cancel, &mut |_, _, _| {});
    let (ids2, _, _) = db2.search_page(&q("виноградне"), 10, 0).unwrap();
    assert!(db2.record_card(ids2[0]).unwrap().extra.is_empty());
}

#[test]
fn import_arbitrary_table_preserves_all_columns_in_results_filters_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("arbitrary.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = ["SKU", "Product Name", "Category", "Price EUR", "Warehouse"];
    for (c, h) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    let rows = [
        ["SKU-42", "Laptop Pro", "Electronics", "1299.50", "Berlin"],
        ["SKU-77", "Office chair", "Furniture", "240", "Lviv"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, value) in row.iter().enumerate() {
            sheet
                .write_string((r + 1) as u32, c as u16, *value)
                .unwrap();
        }
    }
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("arbitrary.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.quality.layout, "generic table");
    assert_eq!(summary.quality.header_row, 1);
    assert_eq!(summary.quality.source_columns, 5);
    assert_eq!(summary.quality.recognized_columns, 2);
    assert_eq!(summary.quality.extra_columns, 3);
    assert_eq!(summary.quality.non_empty_cells, 10);
    assert_eq!(summary.quality.empty_cells, 0);
    assert!(
        summary
            .quality
            .warnings
            .iter()
            .any(|warning| warning.contains("Only 2 semantic fields were recognized"))
    );

    let log = db.import_log(1);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].quality.layout, "generic table");
    assert_eq!(log[0].quality.source_columns, 5);
    assert_eq!(log[0].quality.extra_columns, 3);

    let shape = db.table_shape().unwrap();
    assert_eq!(shape.columns.len(), 5);
    assert_eq!(shape.columns[0].id, "sku");
    assert_eq!(shape.columns[0].header, "SKU");
    assert_eq!(shape.columns[0].role, ColumnRole::Code);
    assert_eq!(
        shape.columns[0].storage,
        ColumnStorage::SchemaColumn("product_code".to_string())
    );
    assert_eq!(
        shape.columns[1].storage,
        ColumnStorage::SchemaColumn("description".to_string())
    );
    assert_eq!(shape.columns[3].id, "price_eur");
    assert_eq!(shape.columns[3].role, ColumnRole::Money);
    assert_eq!(shape.columns[4].header, "Warehouse");
    assert_eq!(shape.columns[0].semantic, Some(SemanticField::ProductCode));
    assert_eq!(shape.columns[1].semantic, Some(SemanticField::Description));
    assert!(
        shape.columns[2..]
            .iter()
            .all(|column| column.semantic.is_none())
    );

    let q = Query {
        text: "Laptop".into(),
        ..Default::default()
    };
    assert_eq!(db.count(&q).unwrap(), 1);
    let (fields, ids, rows, _dups) = db.search_page_dynamic(&q, 10, 0).unwrap();
    assert_eq!(ids.len(), 1);
    let sku_idx = fields.iter().position(|f| f.id == "source:sku").unwrap();
    let price_idx = fields
        .iter()
        .position(|f| f.id == "source:price_eur")
        .unwrap();
    assert_eq!(rows[0][sku_idx], "SKU-42");
    assert_eq!(rows[0][price_idx], "1299.50");

    let card = db.record_card(ids[0]).unwrap();
    assert_eq!(
        card.fields,
        vec![
            ("SKU".to_string(), "SKU-42".to_string()),
            ("Product Name".to_string(), "Laptop Pro".to_string()),
            ("Category".to_string(), "Electronics".to_string()),
            ("Price EUR".to_string(), "1299.50".to_string()),
            ("Warehouse".to_string(), "Berlin".to_string()),
        ]
    );
    assert!(card.extra.is_empty());

    let price_query = Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::Extra("Price EUR".to_string()),
            op: ConditionOp::Range,
            value: ConditionValue::Range {
                from: Some("1000".to_string()),
                to: Some("2000".to_string()),
            },
            negated: false,
        })),
        ..Default::default()
    };
    assert_eq!(db.count(&price_query).unwrap(), 1);

    let csv_path = dir.path().join("arbitrary.csv");
    let exported = export::export(&db, &Query::default(), &csv_path, &cancel, |_, _| {}).unwrap();
    assert_eq!(exported, 2);
    let csv = std::fs::read_to_string(csv_path).unwrap();
    assert!(csv.contains("SKU;Product Name;Category;Price EUR;Warehouse"));
    assert!(csv.contains("SKU-42;Laptop Pro;Electronics;1299.50;Berlin"));
}

#[test]
fn generic_excel_serial_date_drives_year_filters() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("serial-date.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = ["Order Date", "Product Name", "Value USD"];
    for (c, header) in headers.iter().enumerate() {
        sheet.write_string(0, c as u16, *header).unwrap();
    }
    sheet.write_number(1, 0, 45366.0).unwrap();
    sheet.write_string(1, 1, "Serial date product").unwrap();
    sheet.write_string(1, 2, "100").unwrap();
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("serial-date.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);

    let query = Query {
        filters: Filters {
            year: "2024".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(db.count(&query).unwrap(), 1);
    let (ids, _rows, _dups) = db.search_page(&query, 10, 0).unwrap();
    let card = db.record_card(ids[0]).unwrap();
    assert!(
        card.fields
            .iter()
            .any(|(header, value)| header == "Order Date" && value == "2024-03-15")
    );
}

/// The database must not keep a WAL the size of the imported data: after an
/// import completes, the write-ahead log is checkpointed and truncated back to
/// a small bounded file.
#[test]
fn import_leaves_a_small_truncated_wal() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("wal.xlsx");
    let db_path = dir.path().join("wal.db");
    // Enough rows to make the WAL clearly larger than the post-import bound.
    let rows: Vec<Vec<(String, String)>> = (0..20_000)
        .map(|i| {
            vec![
                (
                    "declaration_number".to_string(),
                    format!("24UA{:09}U{}", i, i % 10),
                ),
                ("declaration_date".to_string(), "15.03.2024".to_string()),
                ("sender".to_string(), format!("WAL SENDER {i}")),
                ("edrpou".to_string(), format!("{:08}", i % 100_000_000)),
                ("recipient".to_string(), "WAL IMPORT LLC".to_string()),
                ("product_code".to_string(), "8517130000".to_string()),
                (
                    "description".to_string(),
                    format!("Bulk import row {i} with some descriptive text"),
                ),
                ("net_kg".to_string(), "1.5".to_string()),
                ("currency_control_value".to_string(), "100".to_string()),
            ]
        })
        .collect();
    write_owned_test_xlsx(&xlsx, &rows);

    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 20_000);
    drop(db);

    let wal_bytes = std::fs::metadata(dir.path().join("wal.db-wal"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    // journal_size_limit is 64 MB; a healthy post-import WAL is far below it.
    assert!(
        wal_bytes <= 64 * 1024 * 1024,
        "WAL was not truncated after import: {wal_bytes} bytes"
    );
}

/// Reimporting the same file is skipped by content hash without parsing Excel.
#[test]
fn duplicate_file_skipped_by_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("data.xlsx");
    write_test_xlsx(&xlsx, &sample_rows());
    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);

    let first = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(first.error, None);
    assert_eq!(first.imported, 3);
    assert_eq!(first.skipped_duplicate_of, None);

    // The same file is skipped completely.
    let second = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(second.error, None);
    assert_eq!(second.skipped_duplicate_of, Some("data.xlsx".to_string()));
    assert_eq!(second.total_rows, 0);

    // The same table under another filename is skipped and points to the original.
    let copy = dir.path().join("copy.xlsx");
    std::fs::copy(&xlsx, &copy).unwrap();
    let third = import::import_file(&mut db, &copy, &cancel, &mut |_, _, _| {});
    assert_eq!(third.skipped_duplicate_of, Some("data.xlsx".to_string()));
    assert_eq!(db.total_rows(), 3);
}

#[test]
fn canonical_hash_marks_same_normalized_record_as_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("canonical.db");
    let mut db = Db::open(&db_path).unwrap();

    let mut values = vec![String::new(); COLUMNS.len()];
    values[col_index("declaration_number").unwrap()] = "24UA100110000001U1".to_string();
    values[col_index("declaration_date").unwrap()] = "2024-03-15".to_string();
    values[col_index("sender").unwrap()] = "ACME LTD".to_string();
    values[col_index("recipient").unwrap()] = "Demo Import LLC".to_string();
    values[col_index("product_code").unwrap()] = "8504405500".to_string();
    values[col_index("description").unwrap()] = "Static converter".to_string();

    let first = ImportRecord {
        hash: canonical_record_hash(&values, None),
        year: Some(2024),
        values: values.clone(),
        extra: None,
    };
    let second = ImportRecord {
        hash: canonical_record_hash(&values, None),
        year: Some(2024),
        values,
        extra: None,
    };

    assert_eq!(db.insert_batch("layout-a.xlsx", &[first]).unwrap(), (1, 0));
    assert_eq!(db.insert_batch("layout-b.xlsx", &[second]).unwrap(), (1, 1));
}

#[test]
fn records_schema_migration_preserves_rows_and_invalidates_old_file_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("legacy.xlsx");
    write_test_xlsx(&xlsx, &sample_rows());
    let db_path = dir.path().join("legacy.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);

    let first = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(first.error, None);
    assert_eq!(db.total_rows(), 3);
    db.meta_set("records_schema", "1");
    db.diagnostic_execute("UPDATE import_log SET file_hash = 'legacy-hash'")
        .unwrap();
    drop(db);

    let mut db = Db::open(&db_path).unwrap();
    assert_eq!(db.total_rows(), 3);
    assert_eq!(db.find_import_by_hash("legacy-hash"), None);
    assert_eq!(db.unindexed_rows(), 3);
    db.index_fts(&cancel, |_, _| {}).unwrap();
    assert_eq!(
        db.count(&Query {
            text: "виноградне".into(),
            ..Default::default()
        })
        .unwrap(),
        1
    );
}

#[test]
fn records_schema_migration_preserves_extra_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("extra-migration.db");
    let mut db = Db::open(&db_path).unwrap();

    let mut values = vec![String::new(); COLUMNS.len()];
    values[col_index("declaration_number").unwrap()] = "24UA100110000001U1".to_string();
    values[col_index("declaration_date").unwrap()] = "2024-03-15".to_string();
    values[col_index("sender").unwrap()] = "ACME LTD".to_string();
    values[col_index("recipient").unwrap()] = "Demo Import LLC".to_string();
    values[col_index("product_code").unwrap()] = "8504405500".to_string();
    values[col_index("description").unwrap()] = "Static converter".to_string();
    let extra = serde_json::to_string(&vec![("Container", "CONT-42")]).unwrap();
    let record = ImportRecord {
        hash: canonical_record_hash(&values, Some(&extra)),
        year: Some(2024),
        values,
        extra: Some(extra),
    };

    assert_eq!(db.insert_batch("generic.xlsx", &[record]).unwrap(), (1, 0));
    db.meta_set("records_schema", "1");
    drop(db);

    let db = Db::open(&db_path).unwrap();
    let (ids, _, _) = db
        .search_page(
            &Query {
                text: "CONT-42".into(),
                ..Default::default()
            },
            10,
            0,
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        db.record_card(ids[0]).unwrap().extra,
        vec![("Container".to_string(), "CONT-42".to_string())]
    );
}

#[test]
fn cancelled_import_rolls_back_inserted_batches() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("large.xlsx");
    let db_path = dir.path().join("large.db");
    let mut rows = Vec::new();
    for i in 0..8_300 {
        rows.push(vec![
            (
                "declaration_number".to_string(),
                format!("24UA100110{:06}U1", i),
            ),
            ("declaration_date".to_string(), "15.03.2024".to_string()),
            ("sender".to_string(), format!("SUPPLIER {i}")),
            ("edrpou".to_string(), format!("{:08}", i % 100_000_000)),
            ("recipient".to_string(), "ROLLBACK TEST LLC".to_string()),
            ("product_code".to_string(), "8517130000".to_string()),
            (
                "description".to_string(),
                format!("Rollback import row {i}"),
            ),
            ("trade_country".to_string(), "CN".to_string()),
        ]);
    }
    write_owned_test_xlsx(&xlsx, &rows);

    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |phase, done, _| {
        if phase == ImportPhase::Inserting && done >= 8_192 {
            cancel.store(true, Ordering::Relaxed);
        }
    });

    assert_eq!(summary.error, None);
    assert!(summary.cancelled);
    assert_eq!(db.total_rows(), 0);

    let retry_cancel = AtomicBool::new(false);
    let retry = import::import_file(&mut db, &xlsx, &retry_cancel, &mut |_, _, _| {});
    assert_eq!(retry.error, None);
    assert!(!retry.cancelled);
    assert_eq!(retry.imported, 8_300);
    assert_eq!(retry.duplicates, 0);
    assert_eq!(db.total_rows(), 8_300);
}

#[test]
fn unknown_two_column_table_imports_as_universal_data() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("generic.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.write_string(0, 0, "Номер МД").unwrap();
    sheet.write_string(0, 1, "Что-то другое").unwrap();
    sheet.write_string(1, 0, "24UA1").unwrap();
    workbook.save(&xlsx).unwrap();

    let db_path = dir.path().join("test.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);
    assert_eq!(db.total_rows(), 1);

    let q = Query {
        text: "24UA1".into(),
        ..Default::default()
    };
    assert_eq!(db.count(&q).unwrap(), 1);
    let (fields, ids, rows, _) = db.search_page_dynamic(&q, 10, 0).unwrap();
    let md_idx = fields
        .iter()
        .position(|field| field.id == "source:номер_мд")
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(rows[0][md_idx], "24UA1");
}

#[test]
fn contains_ci_works() {
    use base_search::db::contains_ci;
    assert!(contains_ci("ТОВ «Вінтаж Імпорт»", "вінтаж"));
    assert!(contains_ci("SIEMENS AG", "siemens"));
    assert!(contains_ci("ВИНОГРАД", "виноград"));
    assert!(contains_ci("abc", ""));
    assert!(!contains_ci("ТОВ «Вінтаж»", "вино"));
    assert!(!contains_ci("", "вино"));
    assert!(contains_ci("ааб", "аб")); // overlapping prefix
}

#[test]
fn fts_prefix_terms_keeps_long_tokens() {
    // Company names full of one- and two-letter tokens must still produce FTS
    // prefix terms from their distinctive long words — otherwise the filter
    // falls back to a full-table scan (the v1.1 slowdown).
    let terms = fts_prefix_terms("JYSK SP Z O O METEORYTOWA 13 GDANSK").unwrap();
    assert!(terms.contains("\"JYSK\"*"));
    assert!(terms.contains("\"METEORYTOWA\"*"));
    assert!(terms.contains("\"GDANSK\"*"));
    // Short tokens are dropped, not fatal.
    assert!(!terms.contains("\"Z\""));
    assert!(!terms.contains("\"SP\"*"));
    // A name made only of short tokens yields no usable terms.
    assert_eq!(fts_prefix_terms("S A"), None);
    assert_eq!(fts_prefix_terms(""), None);
}

/// A sender/recipient filter must return the same rows whether or not FTS
/// narrowing kicks in, even when the name is full of short tokens.
#[test]
fn sender_filter_with_short_tokens_is_correct() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("senders.xlsx");
    let db_path = dir.path().join("data").join("senders.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA0000001U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "JYSK SP Z O O METEORYTOWA 13 GDANSK"),
                ("edrpou", "10000001"),
                ("recipient", "ТОВ «Перший»"),
                ("product_code", "9405500000"),
                ("description", "Світильники"),
            ],
            vec![
                ("declaration_number", "24UA0000002U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "OTHER COMPANY LLC"),
                ("edrpou", "10000002"),
                ("recipient", "ТОВ «Другий»"),
                ("product_code", "9405500000"),
                ("description", "Світильники інші"),
            ],
        ],
    );
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});

    let filters = Filters {
        sender: "JYSK SP Z O O METEORYTOWA 13 GDANSK".into(),
        ..Default::default()
    };
    let q = Query {
        text: String::new(),
        filters,
        advanced: None,
        record_scope: Default::default(),
    };
    assert_eq!(db.count(&q).unwrap(), 1);
    let (_, rows, _dups) = db.search_page(&q, 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][result_col("edrpou")], "10000001");
}

#[test]
fn bare_numeric_search_is_scoped_to_product_code_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = dir.path().join("numeric.xlsx");
    let db_path = dir.path().join("numeric.db");
    write_test_xlsx(
        &xlsx,
        &[
            vec![
                ("declaration_number", "24UA0000001U1"),
                ("declaration_date", "15.03.2024"),
                ("sender", "POWER SUPPLY CO"),
                ("edrpou", "10000001"),
                ("recipient", "TECH IMPORT LLC"),
                ("product_code", "8504405500"),
                ("description", "Static converter"),
            ],
            vec![
                ("declaration_number", "24UA0000002U2"),
                ("declaration_date", "16.03.2024"),
                ("sender", "MANUALS CO"),
                ("edrpou", "10000002"),
                ("recipient", "DOCS IMPORT LLC"),
                ("product_code", "4901990000"),
                ("description", "Manual references 8504 compliance"),
            ],
        ],
    );
    let cancel = AtomicBool::new(false);
    let mut db = Db::open(&db_path).unwrap();
    import::import_file(&mut db, &xlsx, &cancel, &mut |_, _, _| {});

    let code_query = Query {
        text: "8504".into(),
        ..Default::default()
    };
    assert_eq!(db.count(&code_query).unwrap(), 1);
    let (_, rows, _) = db.search_page(&code_query, 10, 0).unwrap();
    assert_eq!(rows[0][result_col("product_code")], "8504405500");

    let text_query = Query {
        text: "compliance".into(),
        ..Default::default()
    };
    assert_eq!(db.count(&text_query).unwrap(), 1);
}

#[test]
fn fts_query_builder() {
    assert_eq!(build_fts_query("вино, сок"), "\"вино\" \"сок\"");
    assert_eq!(build_fts_query("вин*"), "\"вин\"*");
    assert_eq!(build_fts_query("8504"), "\"8504\"*");
    assert_eq!(build_fts_query("код 850440"), "\"код\" \"850440\"*");
    assert_eq!(build_fts_query("  "), "");
    assert_eq!(build_fts_query("24UA100110"), "\"24UA100110\"");
}

#[test]
fn value_normalization() {
    use calamine::Data;
    assert_eq!(normalize_value(&Data::Float(8504405500.0)), "8504405500");
    assert_eq!(normalize_value(&Data::Float(12.5)), "12.5");
    assert_eq!(
        normalize_value(&Data::String("  а   б\n в ".into())),
        "а б в"
    );
    assert_eq!(normalize_value(&Data::Empty), "");
    assert_eq!(collapse_ws("  один\tдва  "), "один два");
    assert_eq!(normalize_date("31.12.2024"), "2024-12-31");
    assert_eq!(normalize_date("1.3.2024"), "2024-03-01");
    assert_eq!(normalize_date("2024-12-31"), "2024-12-31");
    // YYYY.MM.DD and single-digit month/day are canonicalized so the monthly
    // analytics filter (which matches the "YYYY-MM" prefix) still sees them.
    assert_eq!(normalize_date("2024.12.31"), "2024-12-31");
    assert_eq!(normalize_date("2024/12/31"), "2024-12-31");
    assert_eq!(normalize_date("2024-1-5"), "2024-01-05");
    // A non-date with four leading digits and an out-of-range month is left as-is.
    assert_eq!(normalize_date("1234-56-78"), "1234-56-78");
    assert_eq!(normalize_date("не дата"), "не дата");
    assert_eq!(extract_year("2024-03-15"), Some(2024));
    assert_eq!(extract_year("15.03.2024"), Some(2024));
    assert_eq!(extract_year("120245"), None);
    assert_eq!(extract_year(""), None);
    assert_eq!(parse_number("1 234,56"), Some(1234.56));
    assert_eq!(parse_number("1,234.56"), Some(1234.56));
    assert_eq!(parse_number("$ 300,25"), Some(300.25));
    assert_eq!(parse_number("13804.656"), Some(13804.656));
    assert_eq!(parse_number("20560.176"), Some(20560.176));
    assert_eq!(parse_number("0.368"), Some(0.368));
    assert_eq!(parse_number(""), None);
    // Scientific notation (Excel text renderings) must keep the exponent.
    assert_eq!(parse_number("1.5E3"), Some(1500.0));
    assert_eq!(parse_number("1.23E+08"), Some(123_000_000.0));
    assert_eq!(parse_number("2e-3"), Some(0.002));
    assert_eq!(parse_number("-1,5e2"), Some(-150.0));
    // Exponent shape with trailing text must be rejected, not silently
    // harvested into a wrong integer ("5e3 kg" must not become 53).
    assert_eq!(parse_number("5e3 kg"), None);
    assert_eq!(parse_number("2E-3 unit"), None);
    assert_eq!(parse_number("1.5e2 kg"), None);
    // Space groups make the remaining separator decimal even with 3 digits.
    assert_eq!(parse_number("1 200,500"), Some(1200.5));
    // Multiple same separators are thousands groups.
    assert_eq!(parse_number("1.200.300"), Some(1_200_300.0));
    // The ambiguous lone-separator-3-digits form is style-dependent:
    // weights read decimals, totals/quantities read a thousands group.
    assert_eq!(parse_number("1.250"), Some(1.25));
    assert_eq!(parse_number_grouped("1.250"), Some(1250.0));
    assert_eq!(parse_number_grouped("12,500"), Some(12500.0));
    // Decisive shapes are identical in both styles.
    assert_eq!(parse_number_grouped("0.368"), Some(0.368));
    assert_eq!(parse_number_grouped("13804.656"), Some(13804.656));
    assert_eq!(parse_number_grouped("1 234,56"), Some(1234.56));
    assert_eq!(parse_number_grouped("$ 300,25"), Some(300.25));
}
