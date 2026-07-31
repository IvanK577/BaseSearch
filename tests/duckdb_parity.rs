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
    Analytics, AnalyticsMeasures, AnalyticsPriceMetric, AnalyticsScope, AnalyticsSection,
    AnalyticsSectionKind, AnalyticsWeightTotal, Db, Filters, ImportRecord, PriceMetricKind, Query,
    RecordScope, canonical_record_hash,
};
use base_search::domain::table::{ColumnStorage, SemanticField, TableShape};
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

fn write_table_xlsx(path: &Path, headers: &[&str], rows: &[Vec<&str>]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (column, header) in headers.iter().enumerate() {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    for (row_index, row) in rows.iter().enumerate() {
        for (column, value) in row.iter().enumerate() {
            sheet
                .write_string(row_index as u32 + 1, column as u16, *value)
                .unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn import_with_fixed_context(
    db: &mut Db,
    path: &Path,
    currency: &str,
    weight_unit: &str,
    amount: &str,
    net_weight: &str,
) {
    write_table_xlsx(
        path,
        &["Invoice number", "Product", "Amount", "Net weight"],
        &[vec![
            path.file_stem().unwrap().to_str().unwrap(),
            "Industrial controller",
            amount,
            net_weight,
        ]],
    );
    let options = import::ImportOptions::selected_sheets(["Sheet1"]).with_sheet_fixed_values(
        "Sheet1",
        [
            (SemanticField::Currency, currency),
            (SemanticField::WeightUnit, weight_unit),
        ],
    );
    let summary = import::import_file_with_options(
        db,
        path,
        &options,
        &AtomicBool::new(false),
        &mut |_, _, _| {},
    );
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 1);
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

fn assert_optional_close(actual: Option<f64>, expected: Option<f64>, context: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => assert_close(actual, expected, context),
        _ => panic!("{context}: optional values differ: {actual:?} vs {expected:?}"),
    }
}

fn compare_measures(duck: &AnalyticsMeasures, sqlite: &AnalyticsMeasures, context: &str) {
    assert_eq!(
        duck.currency_totals.len(),
        sqlite.currency_totals.len(),
        "{context}: currency cohort count"
    );
    for expected in &sqlite.currency_totals {
        let actual = duck
            .currency_totals
            .iter()
            .find(|total| total.currency == expected.currency)
            .unwrap_or_else(|| panic!("{context}: missing currency {}", expected.currency));
        assert_eq!(actual.known, expected.known, "{context}: currency known");
        assert_eq!(
            actual.valued_rows, expected.valued_rows,
            "{context}: valued rows"
        );
        assert_close(actual.total_value, expected.total_value, context);
    }

    let compare_weights =
        |actual: &[AnalyticsWeightTotal], expected: &[AnalyticsWeightTotal], label: &str| {
            assert_eq!(actual.len(), expected.len(), "{context}: {label} cohorts");
            for expected in expected {
                let actual = actual
                    .iter()
                    .find(|total| total.source_unit == expected.source_unit)
                    .unwrap_or_else(|| {
                        panic!("{context}: missing {label} unit {}", expected.source_unit)
                    });
                assert_eq!(actual.known, expected.known, "{context}: {label} known");
                assert_eq!(
                    actual.normalized_unit, expected.normalized_unit,
                    "{context}: {label} normalized unit"
                );
                assert_optional_close(actual.factor_to_kg, expected.factor_to_kg, context);
                assert_eq!(
                    actual.weighted_rows, expected.weighted_rows,
                    "{context}: {label} rows"
                );
                assert_close(
                    actual.total_source_weight,
                    expected.total_source_weight,
                    context,
                );
                assert_optional_close(actual.total_kg, expected.total_kg, context);
            }
        };
    compare_weights(
        &duck.net_weight_totals,
        &sqlite.net_weight_totals,
        "net weight",
    );
    compare_weights(
        &duck.gross_weight_totals,
        &sqlite.gross_weight_totals,
        "gross weight",
    );

    assert_eq!(
        duck.value_per_net_weight.len(),
        sqlite.value_per_net_weight.len(),
        "{context}: value/weight cohort count"
    );
    for expected in &sqlite.value_per_net_weight {
        let actual = duck
            .value_per_net_weight
            .iter()
            .find(|metric| metric.currency == expected.currency)
            .unwrap_or_else(|| panic!("{context}: missing ratio {}", expected.currency));
        let mut actual_units = actual.source_weight_units.clone();
        let mut expected_units = expected.source_weight_units.clone();
        actual_units.sort();
        expected_units.sort();
        assert_eq!(actual_units, expected_units, "{context}: ratio units");
        assert_eq!(
            actual.paired_rows, expected.paired_rows,
            "{context}: ratio rows"
        );
        assert_close(actual.total_value, expected.total_value, context);
        assert_close(actual.total_weight, expected.total_weight, context);
        assert_optional_close(actual.value_per_weight, expected.value_per_weight, context);
    }
    assert_eq!(duck.exclusions, sqlite.exclusions, "{context}: exclusions");
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
    assert_close(d.total_quantity, s.total_quantity, context);
    match (d.compatible_usd.as_ref(), s.compatible_usd.as_ref()) {
        (None, None) => {}
        (Some(d), Some(s)) => {
            assert_close(d.total_value_usd, s.total_value_usd, context);
            assert_optional_close(d.avg_value_per_net_kg, s.avg_value_per_net_kg, context);
        }
        _ => panic!("{context}: USD compatibility differs"),
    }
    compare_measures(&d.measures, &s.measures, context);

    assert_eq!(duck.months.len(), sqlite.months.len(), "{context}: months");
    for (dm, sm) in duck.months.iter().zip(sqlite.months.iter()) {
        assert_eq!(dm.month, sm.month, "{context}: month key");
        assert_eq!(dm.rows, sm.rows, "{context}: month rows {}", sm.month);
        assert_eq!(
            dm.declarations, sm.declarations,
            "{context}: month declarations {}",
            sm.month
        );
        match (dm.compatible_usd.as_ref(), sm.compatible_usd.as_ref()) {
            (None, None) => {}
            (Some(dm), Some(sm)) => {
                assert_close(dm.total_value_usd, sm.total_value_usd, context);
                assert_optional_close(dm.avg_value_per_net_kg, sm.avg_value_per_net_kg, context);
            }
            _ => panic!("{context}: month USD compatibility differs"),
        }
        // A month row's `total_value_usd` is `#[serde(skip)]`: `measures` is the
        // only way its money and weight reach the browser, so parity that skips
        // it cannot see a whole column going blank on one engine.
        compare_measures(
            &dm.measures,
            &sm.measures,
            &format!("{context}: month {}", sm.month),
        );
    }

    compare_section_lists(&duck.company_sections, &sqlite.company_sections, context);
    compare_section_lists(&duck.product_sections, &sqlite.product_sections, context);
    compare_section_lists(&duck.country_sections, &sqlite.country_sections, context);
    compare_price_lists(&duck.price_sections, &sqlite.price_sections, context);
}

fn compare_section_lists(duck: &[AnalyticsSection], sqlite: &[AnalyticsSection], context: &str) {
    assert_eq!(duck.len(), sqlite.len(), "{context}: section count");
    for ss in sqlite {
        let ds = duck
            .iter()
            .find(|section| section.kind == ss.kind)
            .unwrap_or_else(|| panic!("{context}: missing {:?} section", ss.kind));
        assert_eq!(ds.kind, ss.kind, "{context}: section kind");
        assert_eq!(
            ds.rows.len(),
            ss.rows.len(),
            "{context}: {:?} group count",
            ss.kind
        );
        for sr in &ss.rows {
            let dr = ds
                .rows
                .iter()
                .find(|row| row.label == sr.label)
                .unwrap_or_else(|| panic!("{context}: missing {:?} / {}", ss.kind, sr.label));
            let group = format!("{context}: {:?} / {}", ss.kind, sr.label);
            assert_eq!(dr.label, sr.label, "{group}: label order");
            assert_eq!(dr.rows, sr.rows, "{group}: rows");
            assert_eq!(dr.declarations, sr.declarations, "{group}: declarations");
            assert_eq!(dr.companies, sr.companies, "{group}: companies");
            assert_close(dr.total_quantity, sr.total_quantity, &group);
            match (dr.compatible_usd.as_ref(), sr.compatible_usd.as_ref()) {
                (None, None) => {}
                (Some(duck_usd), Some(sqlite_usd)) => {
                    assert_close(duck_usd.total_value_usd, sqlite_usd.total_value_usd, &group);
                    assert_optional_close(
                        duck_usd.avg_value_per_net_kg,
                        sqlite_usd.avg_value_per_net_kg,
                        &group,
                    );
                    assert_close(dr.share_percent, sr.share_percent, &group);
                }
                _ => panic!("{group}: USD compatibility differs"),
            }
            compare_measures(&dr.measures, &sr.measures, &group);
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
        if sm.kind == PriceMetricKind::ValuePerNetKg && dm.cohorts.len() != 1 {
            assert_eq!(dm.count, 0, "{metric}: unsafe scalar count");
            continue;
        }
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

/// Group and month rows publish their money, weight and value-per-kg ONLY
/// through `measures`: `total_value_usd` is `#[serde(skip)]` on both, so a row
/// carrying a default `AnalyticsMeasures` reaches the browser as an em dash
/// even though every sum behind it was computed. The projection used to build
/// exactly that row, and because every other compared field still agreed,
/// nothing failed — the numbers simply disappeared from the tables.
#[test]
fn group_and_month_rows_publish_inherited_measures() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_generic_fixture(dir.path());
    let query = Query::default();
    let sqlite = db
        .analytics_scoped(&query, 50, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    let duck =
        duckdb_olap::analytics_scoped(&projection, &query, 50, Some(AnalyticsScope::Companies), 10)
            .unwrap();

    assert!(
        !sqlite.overview.measures.currency_totals.is_empty(),
        "fixture must have a currency cohort for rows to inherit"
    );
    assert!(!duck.months.is_empty(), "fixture must produce months");
    for month in &duck.months {
        assert!(
            !month.measures.currency_totals.is_empty(),
            "month {} publishes no money at all",
            month.month
        );
        assert!(
            !month.measures.net_weight_totals.is_empty(),
            "month {} publishes no weight at all",
            month.month
        );
    }

    let recipients = duck
        .company_sections
        .iter()
        .find(|section| section.kind == AnalyticsSectionKind::Recipients)
        .expect("the fixture maps a recipient column");
    assert!(
        !recipients.rows.is_empty(),
        "fixture must produce recipient group rows"
    );

    let mut group_rows = 0_usize;
    for section in &duck.company_sections {
        for row in &section.rows {
            group_rows += 1;
            assert!(
                !row.measures.currency_totals.is_empty(),
                "group {} publishes no money at all",
                row.label
            );
            assert!(
                !row.measures.net_weight_totals.is_empty(),
                "group {} publishes no weight at all",
                row.label
            );
            // The wire is the contract. `total_value_usd` can only ever appear
            // as part of the flattened `compatible_usd` object — the row's own
            // field of that name is `#[serde(skip)]`. This fixture HAS a USD
            // cohort, so the key is present here; on anything without one (the
            // customs profile, mixed currencies) it disappears entirely, which
            // is precisely why `measures` has to carry the money.
            let wire = serde_json::to_value(row).unwrap();
            assert_eq!(
                wire.get("total_value_usd").is_some(),
                row.compatible_usd.is_some(),
                "group {} serializes money outside the compatibility object",
                row.label
            );
            assert!(
                wire["measures"]["currency_totals"]
                    .as_array()
                    .is_some_and(|totals| !totals.is_empty()),
                "group {} serializes an empty currency cohort",
                row.label
            );
        }
    }
    assert!(group_rows > 0, "fixture must produce group rows");

    compare_analytics(&duck, &sqlite, "generic empty query / companies");
}

/// The Ukrainian "Відправник", "Одержувач" and "Опис" filters.
///
/// DuckDB folds the column with `lower()`, which is Unicode-aware, while the
/// needle was folded with `to_ascii_lowercase`, which leaves Cyrillic exactly
/// as it was. The two could never meet, so every one of these filters answered
/// "no rows" on the projection while SQLite answered correctly — on a Ukrainian
/// customs database, that is most of the app.
#[test]
fn cyrillic_contains_filters_match_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let (db, projection) = build_fixture(dir.path());

    let probes: Vec<(&str, Query)> = vec![
        (
            "recipient as stored (uppercase Cyrillic)",
            filters_query(|filters| filters.recipient = "ТОВ АЙФОН УКРАЇНА".into()),
        ),
        (
            "recipient lowercased by the user",
            filters_query(|filters| filters.recipient = "тов айфон україна".into()),
        ),
        (
            "sender",
            filters_query(|filters| filters.sender = "APPLE DISTRIBUTION INTERNATIONAL LTD".into()),
        ),
        (
            "description",
            filters_query(|filters| filters.description = "Вино виноградне ігристе".into()),
        ),
    ];

    for (name, query) in &probes {
        let sqlite = db
            .analytics_scoped(query, 50, Some(AnalyticsScope::Companies), 10)
            .unwrap();
        let duck = duckdb_olap::analytics_scoped(
            &projection,
            query,
            50,
            Some(AnalyticsScope::Companies),
            10,
        )
        .unwrap();
        assert!(
            sqlite.overview.row_count > 0,
            "{name}: the probe must select rows, otherwise it proves nothing"
        );
        assert_eq!(
            duck.overview.row_count, sqlite.overview.row_count,
            "{name}: the projection selected a different number of rows"
        );
        compare_analytics(&duck, &sqlite, name);
    }
}

/// The month series is the only source of the period caption, so a limit that
/// quietly truncates it makes a ten-year archive describe itself as a four-year
/// one — and makes the two engines return different series for one database.
/// DuckDB kept its own hard limit of 48 after SQLite's became 600.
#[test]
fn month_series_is_not_truncated_at_four_years() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("months.db");
    let mut db = Db::open(&db_path).unwrap();
    let headers = [
        "Order Date",
        "Invoice Number",
        "Customer",
        "Net Weight KG",
        "Amount USD",
    ];
    let shape = TableShape::from_headers(headers.iter().map(|header| (*header).to_string()));
    db.remember_table_shape(&shape);
    db.remember_extra_headers(headers);

    const MONTHS: i64 = 60;
    let rows: Vec<ImportRecord> = (0..MONTHS)
        .map(|index| {
            let year = 2020 + index / 12;
            let date = format!("{year}-{:02}-15", 1 + index % 12);
            let invoice = format!("INV-{index:04}");
            generic_record(
                year,
                &[
                    ("Order Date", date.as_str()),
                    ("Invoice Number", invoice.as_str()),
                    ("Customer", "Acme UA"),
                    ("Net Weight KG", "10"),
                    ("Amount USD", "100"),
                ],
            )
        })
        .collect();
    db.begin_import_file().unwrap();
    assert_eq!(
        db.insert_batch("months.csv", &rows).unwrap(),
        (MONTHS as u64, 0)
    );
    db.commit_import_file().unwrap();

    let projection = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();

    let query = Query::default();
    let sqlite = db.analytics_scoped(&query, 50, None, 10).unwrap();
    assert_eq!(
        sqlite.months.len(),
        MONTHS as usize,
        "the fixture must span more than the old four-year window"
    );
    // Both DuckDB paths carry their own limit: the persisted monthly rollup and
    // the detail scan.
    for (name, analytics) in [
        (
            "rollup",
            duckdb_olap::analytics_scoped(&projection, &query, 50, None, 10).unwrap(),
        ),
        (
            "detail",
            duckdb_olap::analytics_scoped_detail(&projection, &query, 50, None, 10).unwrap(),
        ),
    ] {
        assert_eq!(
            analytics.months.len(),
            sqlite.months.len(),
            "{name}: month series length"
        );
        compare_analytics(&analytics, &sqlite, name);
    }
}

/// Which group rows survive the section limit, and what share each one claims.
///
/// SQLite ranks group rows by the plain `SUM(value)` and computes every share
/// against that same sum, whatever currency the data is in. The projection read
/// the USD-COMPATIBILITY total instead, which is deliberately absent for
/// anything that is not one known USD cohort — including the customs profile
/// this product exists for, whose value column ("ФВ вал.контр") carries no
/// currency at all. It therefore ranked and shared by weight. Every previous
/// parity fixture ranked the same way by value and by weight, so the difference
/// was invisible; here the two rankings are deliberately opposite, and the
/// section limit makes them return different companies for the same question.
#[test]
fn unknown_currency_sections_rank_and_share_by_value_like_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ranking.db");
    let mut db = Db::open(&db_path).unwrap();
    // No currency column and no currency word in any header: exactly the
    // customs situation, so `compatible_usd` is None on both engines.
    let headers = [
        "Order Date",
        "Invoice Number",
        "Customer",
        "Net Weight KG",
        "Amount",
    ];
    let shape = TableShape::from_headers(headers.iter().map(|header| (*header).to_string()));
    db.remember_table_shape(&shape);
    db.remember_extra_headers(headers);

    let rows = vec![
        generic_record(
            2024,
            &[
                ("Order Date", "2024-03-15"),
                ("Invoice Number", "INV-001"),
                ("Customer", "HEAVY AND CHEAP"),
                ("Net Weight KG", "100"),
                ("Amount", "500"),
            ],
        ),
        generic_record(
            2024,
            &[
                ("Order Date", "2024-03-16"),
                ("Invoice Number", "INV-002"),
                ("Customer", "LIGHT AND RICH"),
                ("Net Weight KG", "1"),
                ("Amount", "5000"),
            ],
        ),
        generic_record(
            2024,
            &[
                ("Order Date", "2024-03-17"),
                ("Invoice Number", "INV-003"),
                ("Customer", "IN THE MIDDLE"),
                ("Net Weight KG", "50"),
                ("Amount", "2000"),
            ],
        ),
    ];
    db.begin_import_file().unwrap();
    assert_eq!(db.insert_batch("ranking.csv", &rows).unwrap(), (3, 0));
    db.commit_import_file().unwrap();

    let projection = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();

    // A limit below the number of groups is the whole point: with every group
    // returned, a different order is still the same set of numbers.
    let query = Query::default();
    let sqlite = db
        .analytics_scoped(&query, 2, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    let duck =
        duckdb_olap::analytics_scoped(&projection, &query, 2, Some(AnalyticsScope::Companies), 10)
            .unwrap();

    assert!(
        sqlite.overview.compatible_usd.is_none() && duck.overview.compatible_usd.is_none(),
        "the fixture must have no usable currency, or it does not reproduce the customs case"
    );

    let recipients = |analytics: &Analytics| {
        analytics
            .company_sections
            .iter()
            .find(|section| section.kind == AnalyticsSectionKind::Recipients)
            .expect("the fixture maps a recipient column")
            .rows
            .clone()
    };
    let expected = recipients(&sqlite);
    let actual = recipients(&duck);
    assert_eq!(expected.len(), 2, "the section limit must actually cut");
    assert!(
        !expected.iter().any(|row| row.label == "HEAVY AND CHEAP"),
        "the fixture must rank differently by value than by weight"
    );
    assert_eq!(
        actual.iter().map(|row| &row.label).collect::<Vec<_>>(),
        expected.iter().map(|row| &row.label).collect::<Vec<_>>(),
        "the projection kept a different top-N than SQLite"
    );

    // The share column reads off the same basis, and nothing else in this file
    // compares it when there is no USD cohort — which is the only case where it
    // could ever differ.
    assert!(
        expected[0].share_percent > 50.0,
        "shares must be computed from value, not weight, for this comparison to bite"
    );
    for expected_row in &expected {
        let actual_row = actual
            .iter()
            .find(|row| row.label == expected_row.label)
            .unwrap_or_else(|| panic!("missing group {}", expected_row.label));
        assert_close(
            actual_row.share_percent,
            expected_row.share_percent,
            &format!("share of {}", expected_row.label),
        );
    }

    compare_analytics(&duck, &sqlite, "unknown currency / limited sections");
}

#[test]
fn mixed_currency_detail_never_exposes_a_usd_compatibility_sum() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("mixed-currency.db");
    let input = dir.path().join("mixed-currency.xlsx");
    write_table_xlsx(
        &input,
        &[
            "Invoice number",
            "Product",
            "Amount",
            "Currency",
            "Net weight",
            "Weight unit",
        ],
        &[
            vec!["INV-USD", "Controller", "1000", "USD", "10", "kg"],
            vec!["INV-EUR", "Controller", "900", "EUR", "5", "kg"],
        ],
    );
    let mut db = Db::open(&db_path).unwrap();
    let summary = import::import_file(&mut db, &input, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, 2);
    let projection = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();

    let (analytics, source) =
        duckdb_olap::analytics_scoped_with_source(&projection, &Query::default(), 20, None, 10)
            .unwrap();
    assert_eq!(source, duckdb_olap::DuckAnalyticsSource::Detail);
    assert!(analytics.overview.compatible_usd.is_none());
    assert_eq!(analytics.overview.total_value_usd, 0.0);
    assert!(analytics.overview.measures.compatible_value_total.is_none());
    assert_eq!(analytics.overview.measures.currency_totals.len(), 2);
    let usd = analytics
        .overview
        .measures
        .currency_totals
        .iter()
        .find(|total| total.currency == "USD")
        .unwrap();
    let eur = analytics
        .overview
        .measures
        .currency_totals
        .iter()
        .find(|total| total.currency == "EUR")
        .unwrap();
    assert_close(usd.total_value, 1_000.0, "USD cohort");
    assert_close(eur.total_value, 900.0, "EUR cohort");
    let wire = serde_json::to_value(&analytics.overview).unwrap();
    assert!(wire.get("total_value_usd").is_none());
    assert!(wire.get("avg_value_per_net_kg").is_none());
}

#[test]
fn projection_uses_fixed_currency_context_from_each_source_schema() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("fixed-context.db");
    let mut db = Db::open(&db_path).unwrap();
    import_with_fixed_context(
        &mut db,
        &dir.path().join("fixed-usd.xlsx"),
        "USD",
        "kg",
        "125",
        "5",
    );
    import_with_fixed_context(
        &mut db,
        &dir.path().join("fixed-eur.xlsx"),
        "EUR",
        "kg",
        "80",
        "4",
    );
    let projection = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();

    let analytics =
        duckdb_olap::analytics_scoped(&projection, &Query::default(), 20, None, 10).unwrap();
    let totals = &analytics.overview.measures.currency_totals;
    assert_eq!(totals.len(), 2);
    assert_close(
        totals
            .iter()
            .find(|total| total.currency == "USD")
            .unwrap()
            .total_value,
        125.0,
        "fixed USD",
    );
    assert_close(
        totals
            .iter()
            .find(|total| total.currency == "EUR")
            .unwrap()
            .total_value,
        80.0,
        "fixed EUR",
    );
    assert!(analytics.overview.compatible_usd.is_none());
}

#[test]
fn known_mixed_weight_units_are_normalized_without_mixing_source_totals() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("mixed-units.db");
    let mut db = Db::open(&db_path).unwrap();
    import_with_fixed_context(
        &mut db,
        &dir.path().join("weights-kg.xlsx"),
        "USD",
        "kg",
        "100",
        "10",
    );
    import_with_fixed_context(
        &mut db,
        &dir.path().join("weights-g.xlsx"),
        "USD",
        "g",
        "50",
        "1000",
    );
    let projection = duckdb_olap::default_projection_path(&db_path);
    duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap();

    let (analytics, source) =
        duckdb_olap::analytics_scoped_with_source(&projection, &Query::default(), 20, None, 10)
            .unwrap();
    assert_eq!(source, duckdb_olap::DuckAnalyticsSource::Rollup);
    assert_close(analytics.overview.total_net_kg, 11.0, "normalized kg");
    assert_eq!(analytics.overview.measures.net_weight_totals.len(), 2);
    let grams = analytics
        .overview
        .measures
        .net_weight_totals
        .iter()
        .find(|total| total.source_unit == "g")
        .unwrap();
    assert_close(grams.total_source_weight, 1_000.0, "source grams");
    assert_optional_close(grams.total_kg, Some(1.0), "normalized grams");
    let ratio = analytics
        .overview
        .measures
        .compatible_value_per_net_weight
        .as_ref()
        .unwrap();
    assert_close(ratio.total_value, 150.0, "paired value");
    assert_close(ratio.total_weight, 11.0, "paired kg");
    assert_optional_close(ratio.value_per_weight, Some(150.0 / 11.0), "USD/kg");
    let wire = serde_json::to_value(&analytics.overview).unwrap();
    assert_eq!(wire["total_value_usd"], 150.0);
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

    db.diagnostic_execute(
        "UPDATE source_columns
         SET storage_name = 'column_that_does_not_exist'
         WHERE semantic = 'value' AND storage_kind = 'schema_column'",
    )
    .unwrap();

    let error = duckdb_olap::build_projection_atomic(&db_path, &projection).unwrap_err();
    assert!(error.contains("column_that_does_not_exist"), "{error}");
    let after = duckdb_olap::read_projection_meta(&projection).unwrap();
    assert_eq!(after.built_at, before.built_at);
    assert_eq!(after.source_fingerprint, before.source_fingerprint);
    assert_eq!(after.rollup_fingerprint, before.rollup_fingerprint);
    assert_eq!(after.rollup_rules_version, before.rollup_rules_version);
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp"))
    );
    db.diagnostic_execute(
        "UPDATE source_columns
         SET storage_name = 'currency_control_value'
         WHERE semantic = 'value' AND storage_kind = 'schema_column'",
    )
    .unwrap();
    assert!(duckdb_olap::projection_is_current(&db_path, &projection).unwrap());
}
