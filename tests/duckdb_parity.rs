#![cfg(feature = "duckdb-olap")]
//! SQLite vs DuckDB analytics parity.
//!
//! The DuckDB projection is a performance path, not a second source of truth:
//! for every supported query it must report exactly the numbers the SQLite
//! analytics report. The fixture deliberately contains flagged duplicate rows,
//! localized country names, decimal-comma numbers, missing values, and
//! mixed-case trademarks — the situations where the two engines historically
//! could drift apart.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use base_search::db::{
    Analytics, AnalyticsPriceMetric, AnalyticsScope, AnalyticsSection, Db, Filters, ImportRecord,
    Query, RecordScope, canonical_record_hash,
};
use base_search::domain::table::{
    ColumnRole, ColumnStorage, SemanticField, SourceColumn, TableShape,
};
use base_search::duckdb_olap;
use base_search::import;
use base_search::schema::{COLUMNS, col_index};

fn write_fixture_xlsx(path: &Path, rows: &[Vec<(&str, &str)>]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (column, def) in COLUMNS.iter().enumerate() {
        sheet.write_string(0, column as u16, def.header).unwrap();
    }
    for (row_index, row) in rows.iter().enumerate() {
        for (name, value) in row {
            let column = col_index(name).unwrap() as u16;
            sheet
                .write_string(row_index as u32 + 1, column, *value)
                .unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn fixture_rows() -> Vec<Vec<(&'static str, &'static str)>> {
    vec![
        vec![
            ("declaration_number", "24UA000000000001U1"),
            ("declaration_date", "15.03.2024"),
            ("sender", "APPLE DISTRIBUTION INTERNATIONAL LTD"),
            ("edrpou", "11111111"),
            ("recipient", "ТОВ АЙФОН УКРАЇНА"),
            ("product_code", "8517130000"),
            ("description", "Apple iPhone 15 smartphone"),
            ("trade_country", "IE"),
            ("dispatch_country", "IE"),
            ("origin_country", "КИТАЙ"),
            ("quantity", "10"),
            ("gross_kg", "12.5"),
            ("net_kg", "10"),
            ("currency_control_value", "1 200,50"),
            ("rfv_usd_kg", "120.05"),
            ("trademark", "Apple"),
        ],
        vec![
            ("declaration_number", "24UA000000000002U2"),
            ("declaration_date", "16.03.2024"),
            ("sender", "SAMSUNG ELECTRONICS"),
            ("edrpou", "22222222"),
            ("recipient", "ТОВ ТЕХНО ІМПОРТ"),
            ("product_code", "8517620000"),
            ("description", "Samsung Galaxy phone"),
            ("trade_country", "KR"),
            ("dispatch_country", "KR"),
            ("origin_country", "VN"),
            ("quantity", "5"),
            ("gross_kg", "7"),
            ("net_kg", "6"),
            ("currency_control_value", "700"),
            ("rfv_usd_kg", "116.66"),
            ("trademark", "Samsung"),
        ],
        vec![
            ("declaration_number", "24UA000000000003U3"),
            ("declaration_date", "17.04.2024"),
            ("sender", "BODEGAS RIOJA S.A."),
            ("edrpou", "33333333"),
            ("recipient", "ТОВ ВИНО СВІТУ"),
            ("product_code", "2204101100"),
            ("description", "Вино виноградне ігристе"),
            ("trade_country", "ES"),
            ("dispatch_country", "ES"),
            ("origin_country", "ES"),
            ("quantity", "100"),
            ("gross_kg", "120"),
            // Missing net weight and a non-numeric value: both engines must
            // treat them as absent, not as zero rows.
            ("net_kg", ""),
            ("currency_control_value", "300,25"),
            ("trademark", ""),
        ],
        vec![
            ("declaration_number", "25UA000000000004U4"),
            ("declaration_date", "12.01.2025"),
            ("sender", "APPLE DISTRIBUTION INTERNATIONAL LTD"),
            ("edrpou", "11111111"),
            ("recipient", "ТОВ АЙФОН УКРАЇНА"),
            ("product_code", "8517130000"),
            ("description", "Apple iPhone 16 smartphone"),
            ("trade_country", "IE"),
            ("dispatch_country", "IE"),
            ("origin_country", "CN"),
            ("quantity", "20"),
            ("gross_kg", "25"),
            ("net_kg", "20"),
            ("currency_control_value", "2 400"),
            ("rfv_usd_kg", "120"),
            ("trademark", "Apple"),
        ],
    ]
}

/// Imports the fixture twice (second file repeats every row and adds one new
/// row), so the database contains flagged duplicates, then builds the DuckDB
/// projection next to it.
fn build_fixture(dir: &Path) -> (Db, std::path::PathBuf) {
    let db_path = dir.join("parity.db");
    let mut db = Db::open(&db_path).unwrap();
    let cancel = AtomicBool::new(false);

    let file_a = dir.join("parity-a.xlsx");
    write_fixture_xlsx(&file_a, &fixture_rows());
    let summary = import::import_file(&mut db, &file_a, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 4);
    assert_eq!(summary.duplicates, 0);

    let mut rows_b = fixture_rows();
    rows_b.push(vec![
        ("declaration_number", "24UA000000000005U5"),
        ("declaration_date", "20.05.2024"),
        ("sender", "SIEMENS AG"),
        ("edrpou", "44444444"),
        ("recipient", "ТОВ ЕЛЕКТРО ТРЕЙД"),
        ("product_code", "8504405500"),
        ("description", "Перетворювач напруги статичний"),
        ("trade_country", "DE"),
        ("dispatch_country", "DE"),
        ("origin_country", "DE"),
        ("quantity", "2"),
        ("gross_kg", "3.5"),
        ("net_kg", "2.5"),
        ("currency_control_value", "500"),
        ("trademark", "Siemens"),
    ]);
    let file_b = dir.join("parity-b.xlsx");
    write_fixture_xlsx(&file_b, &rows_b);
    let summary = import::import_file(&mut db, &file_b, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 5);
    assert_eq!(summary.duplicates, 4);
    assert_eq!(db.total_rows(), 9);

    let projection_path = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection(&db_path, &projection_path).unwrap();
    (db, projection_path)
}

fn generic_record(year: i64, cells: &[(&str, &str)]) -> ImportRecord {
    let extra = serde_json::to_string(cells).unwrap();
    let values = vec![String::new(); COLUMNS.len()];
    ImportRecord {
        hash: canonical_record_hash(&values, Some(&extra)),
        year: Some(year),
        values,
        extra: Some(extra),
    }
}

fn build_generic_fixture(dir: &Path) -> (Db, std::path::PathBuf) {
    let db_path = dir.join("generic.db");
    let mut db = Db::open(&db_path).unwrap();
    let headers = [
        "Order Date",
        "Invoice Number",
        "Supplier",
        "Customer",
        "Company ID",
        "SKU",
        "Product Name",
        "Brand",
        "Origin Country",
        "Shipping Country",
        "Seller Country",
        "Units",
        "Net Weight KG",
        "Gross Weight KG",
        "Amount USD",
    ];
    let shape = TableShape::from_headers(headers.iter().map(|header| (*header).to_string()));
    assert!(shape.columns.iter().all(|column| {
        matches!(column.storage, ColumnStorage::SourceJson) && column.semantic.is_some()
    }));
    db.remember_table_shape(&shape);
    db.remember_extra_headers(headers);

    let rows = vec![
        generic_record(
            2024,
            &[
                ("Order Date", "2024-01-15"),
                ("Invoice Number", "INV-001"),
                ("Supplier", "Nova Supply"),
                ("Customer", "Acme UA"),
                ("Company ID", "1001"),
                ("SKU", "8517130000"),
                ("Product Name", "Nova Phone"),
                ("Brand", "Nova"),
                ("Origin Country", "China"),
                ("Shipping Country", "Poland"),
                ("Seller Country", "Germany"),
                ("Units", "5"),
                ("Net Weight KG", "10"),
                ("Gross Weight KG", "12"),
                ("Amount USD", "1,250"),
            ],
        ),
        generic_record(
            2024,
            &[
                ("Order Date", "2024-02-20"),
                ("Invoice Number", "INV-002"),
                ("Supplier", "Vector GmbH"),
                ("Customer", "Beta UA"),
                ("Company ID", "1002"),
                ("SKU", "8504405500"),
                ("Product Name", "Vector Converter"),
                ("Brand", "Vector"),
                ("Origin Country", "Germany"),
                ("Shipping Country", "Germany"),
                ("Seller Country", "Germany"),
                ("Units", "2"),
                ("Net Weight KG", "5"),
                ("Gross Weight KG", "6"),
                ("Amount USD", "750"),
            ],
        ),
        generic_record(
            2025,
            &[
                ("Order Date", "2025-01-05"),
                ("Invoice Number", "INV-003"),
                ("Supplier", "Nova Supply"),
                ("Customer", "Acme UA"),
                ("Company ID", "1001"),
                ("SKU", "8517130000"),
                ("Product Name", "Nova Phone Pro"),
                ("Brand", "Nova"),
                ("Origin Country", "CN"),
                ("Shipping Country", "Poland"),
                ("Seller Country", "Germany"),
                ("Units", "8"),
                ("Net Weight KG", "20"),
                ("Gross Weight KG", "23"),
                ("Amount USD", "2 000"),
            ],
        ),
    ];
    db.begin_import_file().unwrap();
    assert_eq!(db.insert_batch("generic.csv", &rows).unwrap(), (3, 0));
    db.commit_import_file().unwrap();

    let projection_path = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection_path).unwrap();
    (db, projection_path)
}

fn assert_close(actual: f64, expected: f64, context: &str) {
    assert!(
        (actual - expected).abs() < 0.0001,
        "{context}: sqlite {expected}, duckdb {actual}"
    );
}

fn compare_analytics(duck: &Analytics, sqlite: &Analytics, context: &str) {
    let (d, s) = (&duck.overview, &sqlite.overview);
    assert_eq!(d.row_count, s.row_count, "{context}: row_count");
    assert_eq!(
        d.declaration_count, s.declaration_count,
        "{context}: declarations"
    );
    assert_eq!(d.distinct_senders, s.distinct_senders, "{context}: senders");
    assert_eq!(
        d.distinct_recipients, s.distinct_recipients,
        "{context}: recipients"
    );
    assert_eq!(d.distinct_edrpou, s.distinct_edrpou, "{context}: edrpou");
    assert_eq!(
        d.distinct_trademarks, s.distinct_trademarks,
        "{context}: trademarks"
    );
    assert_eq!(
        d.distinct_product_codes, s.distinct_product_codes,
        "{context}: product codes"
    );
    assert_eq!(
        d.distinct_origin_countries, s.distinct_origin_countries,
        "{context}: origin countries"
    );
    assert_close(d.total_value_usd, s.total_value_usd, context);
    assert_close(d.total_gross_kg, s.total_gross_kg, context);
    assert_close(d.total_net_kg, s.total_net_kg, context);
    assert_close(d.total_quantity, s.total_quantity, context);
    assert_close(d.avg_value_per_net_kg, s.avg_value_per_net_kg, context);

    assert_eq!(duck.months.len(), sqlite.months.len(), "{context}: months");
    for (dm, sm) in duck.months.iter().zip(sqlite.months.iter()) {
        assert_eq!(dm.month, sm.month, "{context}: month key");
        assert_eq!(dm.rows, sm.rows, "{context}: month rows {}", sm.month);
        assert_eq!(
            dm.declarations, sm.declarations,
            "{context}: month declarations {}",
            sm.month
        );
        assert_close(dm.total_value_usd, sm.total_value_usd, context);
        assert_close(dm.total_net_kg, sm.total_net_kg, context);
    }

    compare_section_lists(&duck.company_sections, &sqlite.company_sections, context);
    compare_section_lists(&duck.product_sections, &sqlite.product_sections, context);
    compare_section_lists(&duck.country_sections, &sqlite.country_sections, context);
    compare_price_lists(&duck.price_sections, &sqlite.price_sections, context);
}

fn compare_section_lists(duck: &[AnalyticsSection], sqlite: &[AnalyticsSection], context: &str) {
    assert_eq!(duck.len(), sqlite.len(), "{context}: section count");
    for (ds, ss) in duck.iter().zip(sqlite.iter()) {
        assert_eq!(ds.kind, ss.kind, "{context}: section kind");
        assert_eq!(
            ds.rows.len(),
            ss.rows.len(),
            "{context}: {:?} group count",
            ss.kind
        );
        for (dr, sr) in ds.rows.iter().zip(ss.rows.iter()) {
            let group = format!("{context}: {:?} / {}", ss.kind, sr.label);
            assert_eq!(dr.label, sr.label, "{group}: label order");
            assert_eq!(dr.rows, sr.rows, "{group}: rows");
            assert_eq!(dr.declarations, sr.declarations, "{group}: declarations");
            assert_eq!(dr.companies, sr.companies, "{group}: companies");
            assert_close(dr.total_value_usd, sr.total_value_usd, &group);
            assert_close(dr.total_net_kg, sr.total_net_kg, &group);
            assert_close(dr.total_gross_kg, sr.total_gross_kg, &group);
            assert_close(dr.total_quantity, sr.total_quantity, &group);
            assert_close(dr.share_percent, sr.share_percent, &group);
            assert_close(dr.avg_value_per_net_kg, sr.avg_value_per_net_kg, &group);
        }
    }
}

fn compare_price_lists(
    duck: &[AnalyticsPriceMetric],
    sqlite: &[AnalyticsPriceMetric],
    context: &str,
) {
    assert_eq!(duck.len(), sqlite.len(), "{context}: price metric count");
    for (dm, sm) in duck.iter().zip(sqlite.iter()) {
        let metric = format!("{context}: price {:?}", sm.kind);
        assert_eq!(dm.kind, sm.kind, "{metric}: kind");
        assert_eq!(dm.count, sm.count, "{metric}: count");
        assert_close(dm.average, sm.average, &metric);
        assert_close(dm.minimum, sm.minimum, &metric);
        assert_close(dm.maximum, sm.maximum, &metric);
        assert_close(dm.weighted_average, sm.weighted_average, &metric);
        assert_close(dm.median, sm.median, &metric);
        assert_close(dm.p25, sm.p25, &metric);
        assert_close(dm.p75, sm.p75, &metric);
    }
}

fn filters_query(build: impl FnOnce(&mut Filters)) -> Query {
    let mut filters = Filters::default();
    build(&mut filters);
    Query {
        filters,
        ..Default::default()
    }
}

#[test]
fn duckdb_analytics_match_sqlite_on_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_fixture(dir.path());

    let queries: Vec<(&str, Query)> = vec![
        ("empty query", Query::default()),
        ("year 2024", filters_query(|f| f.year = "2024".into())),
        (
            "edrpou exact",
            filters_query(|f| f.edrpou = "11111111".into()),
        ),
        (
            "origin country synonym",
            filters_query(|f| f.origin_country = "Китай".into()),
        ),
        (
            "trademark case-insensitive exact",
            filters_query(|f| f.trademark = "apple".into()),
        ),
        (
            "product code prefix",
            filters_query(|f| f.product_code = "8517".into()),
        ),
        (
            "combined year + origin",
            filters_query(|f| {
                f.year = "2024".into();
                f.origin_country = "CN".into();
            }),
        ),
        (
            "all imported occurrences",
            Query {
                record_scope: RecordScope::Occurrences,
                ..Default::default()
            },
        ),
        (
            "filtered imported occurrences",
            Query {
                filters: Filters {
                    year: "2024".into(),
                    ..Default::default()
                },
                record_scope: RecordScope::Occurrences,
                ..Default::default()
            },
        ),
    ];

    for (name, query) in &queries {
        for scope in [
            None,
            Some(AnalyticsScope::Companies),
            Some(AnalyticsScope::Products),
            Some(AnalyticsScope::Countries),
            Some(AnalyticsScope::Prices),
        ] {
            let context = format!("{name} / scope {scope:?}");
            let sqlite = db.analytics_scoped(query, 50, scope, 10).unwrap();
            let duck = duckdb_olap::analytics_scoped(&projection, query, 50, scope, 10).unwrap();
            compare_analytics(&duck, &sqlite, &context);
        }
    }
}

/// The staleness check compares projection metadata with the SQLite row count
/// and max id over ALL rows. Duplicate rows are excluded from analytics but
/// must still be counted here, otherwise a projection would always look stale.
#[test]
fn duckdb_projection_meta_counts_all_rows_including_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_fixture(dir.path());

    let meta = duckdb_olap::read_projection_meta(&projection).unwrap();
    assert_eq!(meta.rows, db.total_rows());
    assert_eq!(meta.rows, 9);
    assert_eq!(meta.max_record_id, 9);
    assert_eq!(meta.schema_version, duckdb_olap::PROJECTION_SCHEMA_VERSION);
    assert_eq!(
        meta.rollup_schema_version,
        duckdb_olap::ROLLUP_SCHEMA_VERSION
    );
    assert_eq!(meta.rollup_rules_version, duckdb_olap::ROLLUP_RULES_VERSION);
    assert_eq!(
        meta.rollup_fingerprint,
        duckdb_olap::rollup_contract_fingerprint()
    );
    assert!(!meta.source_generation.is_empty());
    assert!(!meta.source_fingerprint.is_empty());
    assert!(
        duckdb_olap::projection_is_current(&dir.path().join("parity.db"), &projection).unwrap()
    );
}

#[test]
fn duckdb_rollups_match_sqlite_for_json_backed_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_generic_fixture(dir.path());
    let queries: Vec<(&str, Query)> = vec![
        ("empty query", Query::default()),
        (
            "year",
            filters_query(|filters| filters.year = "2024".into()),
        ),
        (
            "company",
            filters_query(|filters| filters.edrpou = "1001".into()),
        ),
        (
            "product prefix",
            filters_query(|filters| filters.product_code = "8517".into()),
        ),
        (
            "trademark",
            filters_query(|filters| filters.trademark = "nova".into()),
        ),
        (
            "origin country",
            filters_query(|filters| filters.origin_country = "CN".into()),
        ),
        (
            "literal product wildcard",
            filters_query(|filters| filters.product_code = "%".into()),
        ),
        (
            "literal description wildcard",
            filters_query(|filters| filters.description = "_".into()),
        ),
    ];

    for (name, query) in &queries {
        for scope in [
            None,
            Some(AnalyticsScope::Companies),
            Some(AnalyticsScope::Products),
            Some(AnalyticsScope::Countries),
            Some(AnalyticsScope::Prices),
        ] {
            let context = format!("generic {name} / scope {scope:?}");
            let sqlite = db.analytics_scoped(query, 50, scope, 10).unwrap();
            let duck = duckdb_olap::analytics_scoped(&projection, query, 50, scope, 10).unwrap();
            compare_analytics(&duck, &sqlite, &context);
        }
    }
}

#[test]
fn projection_contract_invalidates_data_schema_and_semantic_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut db, projection) = build_fixture(dir.path());
    let db_path = dir.path().join("parity.db");
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    let late_file = dir.path().join("late-import.xlsx");
    write_fixture_xlsx(
        &late_file,
        &[vec![
            ("declaration_number", "25UA000000000099U9"),
            ("declaration_date", "18.02.2025"),
            ("sender", "LATE SUPPLIER"),
            ("edrpou", "99999999"),
            ("recipient", "LATE RECIPIENT"),
            ("product_code", "9999999999"),
            ("description", "Late imported product"),
            ("trade_country", "US"),
            ("dispatch_country", "US"),
            ("origin_country", "US"),
            ("quantity", "1"),
            ("gross_kg", "1"),
            ("net_kg", "1"),
            ("currency_control_value", "100"),
            ("trademark", "Late"),
        ]],
    );
    let cancel = AtomicBool::new(false);
    let imported = import::import_file(&mut db, &late_file, &cancel, &mut |_, _, _| {});
    assert_eq!(imported.error, None);
    assert_eq!(imported.imported, 1);
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    let rows_before = db.total_rows();
    let max_id_before = db.max_record_id();
    db.diagnostic_execute("UPDATE records SET value_num = value_num + 1 WHERE id = 1")
        .unwrap();
    assert_eq!(db.total_rows(), rows_before);
    assert_eq!(db.max_record_id(), max_id_before);
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    db.diagnostic_execute("DELETE FROM records WHERE id = 1")
        .unwrap();
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    let product_column = db
        .table_shape()
        .unwrap()
        .columns
        .into_iter()
        .find(|column| column.semantic == Some(SemanticField::ProductCode))
        .unwrap();
    assert!(db.set_column_semantic(&product_column.id, None));
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());

    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
    db.diagnostic_execute("ALTER TABLE records ADD COLUMN contract_probe TEXT")
        .unwrap();
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
}

#[test]
fn failed_rebuild_preserves_last_projection_and_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_fixture(dir.path());
    let db_path = dir.path().join("parity.db");
    let before = duckdb_olap::read_projection_meta(&projection).unwrap();

    let invalid_shape = TableShape {
        columns: vec![SourceColumn {
            id: "broken_value".to_string(),
            header: "Broken value".to_string(),
            source_index: 0,
            role: ColumnRole::Money,
            semantic: Some(SemanticField::Value),
            storage: ColumnStorage::SchemaColumn("column_that_does_not_exist".to_string()),
        }],
    };
    db.meta_set(
        "table_shape_v1",
        &serde_json::to_string(&invalid_shape).unwrap(),
    );

    let error = duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap_err();
    assert!(error.contains("column_that_does_not_exist"), "{error}");
    let after = duckdb_olap::read_projection_meta(&projection).unwrap();
    assert_eq!(after.built_at, before.built_at);
    assert_eq!(after.source_fingerprint, before.source_fingerprint);
    assert_eq!(after.rollup_fingerprint, before.rollup_fingerprint);
    assert_eq!(after.rollup_rules_version, before.rollup_rules_version);
    assert!(!duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp"))
    );
}
