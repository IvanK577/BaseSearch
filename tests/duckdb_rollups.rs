#![cfg(feature = "duckdb-olap")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Instant;

use base_search::db::{
    Analytics, AnalyticsMeasures, AnalyticsScope, Db, ImportRecord, Query, canonical_record_hash,
};
use base_search::domain::table::{SemanticField, TableShape};
use base_search::duckdb_olap::{self, DuckAnalyticsSource};
use base_search::engines::{AnalyticsEngine, DuckDbAnalyticsEngine};
use base_search::schema::COLUMNS;
use duckdb::Connection;

const HEADERS: [&str; 17] = [
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
    "Amount",
    "Currency",
    "Weight Unit",
];

fn record(cells: &[(&str, String)]) -> ImportRecord {
    let extra = serde_json::to_string(cells).unwrap();
    let values = vec![String::new(); COLUMNS.len()];
    ImportRecord {
        hash: canonical_record_hash(&values, Some(&extra)),
        year: cells
            .iter()
            .find(|(header, _)| *header == "Order Date")
            .and_then(|(_, value)| value.get(..4))
            .and_then(|value| value.parse().ok()),
        values,
        extra: Some(extra),
    }
}

fn row(
    date: &str,
    invoice: &str,
    customer: &str,
    code: &str,
    amount: &str,
    currency: &str,
    net_kg: &str,
) -> ImportRecord {
    record(&[
        ("Order Date", date.to_string()),
        ("Invoice Number", invoice.to_string()),
        ("Supplier", "Global Supplier".to_string()),
        ("Customer", customer.to_string()),
        ("Company ID", customer.replace(' ', "")),
        ("SKU", code.to_string()),
        ("Product Name", format!("Product {code}")),
        ("Brand", "Base".to_string()),
        ("Origin Country", "CN".to_string()),
        ("Shipping Country", "PL".to_string()),
        ("Seller Country", "DE".to_string()),
        ("Units", "2".to_string()),
        ("Net Weight KG", net_kg.to_string()),
        ("Gross Weight KG", "12".to_string()),
        ("Amount", amount.to_string()),
        ("Currency", currency.to_string()),
        ("Weight Unit", "kg".to_string()),
    ])
}

fn build_fixture(dir: &Path, records: &[ImportRecord]) -> (Db, PathBuf, PathBuf) {
    let sqlite_path = dir.join("rollups.db");
    let mut db = Db::open(&sqlite_path).unwrap();
    let shape = TableShape::from_headers(HEADERS.iter().map(|header| (*header).to_string()));
    for semantic in [
        SemanticField::Value,
        SemanticField::Currency,
        SemanticField::WeightUnit,
        SemanticField::ProductCode,
    ] {
        assert!(
            shape
                .columns
                .iter()
                .any(|column| column.semantic == Some(semantic)),
            "missing inferred semantic {semantic:?}"
        );
    }
    db.remember_table_shape(&shape);
    db.remember_extra_headers(HEADERS);
    db.begin_import_file().unwrap();
    let inserted = db.insert_batch("rollups.csv", records).unwrap();
    assert_eq!(inserted.0, records.len() as u64);
    db.commit_import_file().unwrap();

    let projection_path = duckdb_olap::default_projection_path(&sqlite_path);
    duckdb_olap::build_projection_atomic(&sqlite_path, &projection_path).unwrap();
    (db, sqlite_path, projection_path)
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= 1e-8 * actual.abs().max(expected.abs()).max(1.0),
        "{label}: expected {expected}, got {actual}"
    );
}

fn assert_optional_close(actual: Option<f64>, expected: Option<f64>, label: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => assert_close(actual, expected, label),
        _ => panic!("{label}: optional values differ: {actual:?} vs {expected:?}"),
    }
}

fn assert_usd_compatibility_match(
    actual: Option<&base_search::db::AnalyticsUsdCompatibility>,
    expected: Option<&base_search::db::AnalyticsUsdCompatibility>,
    label: &str,
) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => {
            assert_close(actual.total_value_usd, expected.total_value_usd, label);
            assert_optional_close(
                actual.avg_value_per_net_kg,
                expected.avg_value_per_net_kg,
                label,
            );
        }
        _ => panic!("{label}: USD compatibility differs"),
    }
}

/// Money and weight as a group or month row actually publishes them.
///
/// Both rows keep `total_value_usd` out of the serialized form, so `measures`
/// is the only place their figures exist on the wire. A rollup that returns the
/// row with a default `AnalyticsMeasures` agrees with SQLite on every other
/// field and still renders an empty table.
fn assert_measures_match(actual: &AnalyticsMeasures, expected: &AnalyticsMeasures, label: &str) {
    assert_eq!(
        actual.currency_totals.len(),
        expected.currency_totals.len(),
        "{label}: currency cohorts"
    );
    for expected_total in &expected.currency_totals {
        let actual_total = actual
            .currency_totals
            .iter()
            .find(|total| total.currency == expected_total.currency)
            .unwrap_or_else(|| panic!("{label}: missing currency {}", expected_total.currency));
        assert_eq!(
            actual_total.valued_rows, expected_total.valued_rows,
            "{label}: valued rows"
        );
        assert_close(actual_total.total_value, expected_total.total_value, label);
    }
    assert_eq!(
        actual.net_weight_totals.len(),
        expected.net_weight_totals.len(),
        "{label}: net weight cohorts"
    );
    for expected_total in &expected.net_weight_totals {
        let actual_total = actual
            .net_weight_totals
            .iter()
            .find(|total| total.source_unit == expected_total.source_unit)
            .unwrap_or_else(|| panic!("{label}: missing unit {}", expected_total.source_unit));
        assert_eq!(
            actual_total.weighted_rows, expected_total.weighted_rows,
            "{label}: weighted rows"
        );
        assert_close(
            actual_total.total_source_weight,
            expected_total.total_source_weight,
            label,
        );
        assert_optional_close(actual_total.total_kg, expected_total.total_kg, label);
    }
}

fn assert_analytics_match(actual: &Analytics, expected: &Analytics) {
    let (actual_overview, expected_overview) = (&actual.overview, &expected.overview);
    assert_eq!(actual_overview.row_count, expected_overview.row_count);
    assert_eq!(
        actual_overview.declaration_count,
        expected_overview.declaration_count
    );
    assert_eq!(
        actual_overview.distinct_senders,
        expected_overview.distinct_senders
    );
    assert_eq!(
        actual_overview.distinct_recipients,
        expected_overview.distinct_recipients
    );
    assert_eq!(
        actual_overview.distinct_edrpou,
        expected_overview.distinct_edrpou
    );
    assert_eq!(
        actual_overview.distinct_product_codes,
        expected_overview.distinct_product_codes
    );
    assert_usd_compatibility_match(
        actual_overview.compatible_usd.as_ref(),
        expected_overview.compatible_usd.as_ref(),
        "overview",
    );
    assert_eq!(
        actual_overview.measures.currency_totals.len(),
        expected_overview.measures.currency_totals.len()
    );
    for expected_total in &expected_overview.measures.currency_totals {
        let actual_total = actual_overview
            .measures
            .currency_totals
            .iter()
            .find(|total| total.currency == expected_total.currency)
            .unwrap_or_else(|| panic!("missing currency {}", expected_total.currency));
        assert_eq!(actual_total.known, expected_total.known);
        assert_eq!(actual_total.valued_rows, expected_total.valued_rows);
        assert_close(
            actual_total.total_value,
            expected_total.total_value,
            "currency total",
        );
    }
    assert_close(
        actual_overview.total_quantity,
        expected_overview.total_quantity,
        "quantity",
    );
    assert_eq!(actual.months.len(), expected.months.len());
    for (actual, expected) in actual.months.iter().zip(&expected.months) {
        assert_eq!(actual.month, expected.month);
        assert_eq!(actual.rows, expected.rows);
        assert_eq!(actual.declarations, expected.declarations);
        assert_usd_compatibility_match(
            actual.compatible_usd.as_ref(),
            expected.compatible_usd.as_ref(),
            "month",
        );
        assert_measures_match(
            &actual.measures,
            &expected.measures,
            &format!("month {}", expected.month),
        );
    }
    for (actual_sections, expected_sections) in [
        (&actual.company_sections, &expected.company_sections),
        (&actual.product_sections, &expected.product_sections),
        (&actual.country_sections, &expected.country_sections),
    ] {
        assert_eq!(actual_sections.len(), expected_sections.len());
        for expected in expected_sections {
            let actual = actual_sections
                .iter()
                .find(|section| section.kind == expected.kind)
                .unwrap_or_else(|| panic!("missing section {:?}", expected.kind));
            assert_eq!(actual.kind, expected.kind);
            assert_eq!(actual.rows.len(), expected.rows.len());
            for expected in &expected.rows {
                let actual = actual
                    .rows
                    .iter()
                    .find(|row| row.label == expected.label)
                    .unwrap_or_else(|| panic!("missing group {}", expected.label));
                assert_eq!(actual.label, expected.label);
                assert_eq!(actual.rows, expected.rows);
                assert_eq!(actual.declarations, expected.declarations);
                assert_eq!(actual.companies, expected.companies);
                assert_usd_compatibility_match(
                    actual.compatible_usd.as_ref(),
                    expected.compatible_usd.as_ref(),
                    "group",
                );
                if actual.compatible_usd.is_some() {
                    assert_close(actual.share_percent, expected.share_percent, "group share");
                }
                assert_measures_match(
                    &actual.measures,
                    &expected.measures,
                    &format!("group {}", expected.label),
                );
            }
        }
    }
    assert_eq!(actual.price_sections.len(), expected.price_sections.len());
    for (actual, expected) in actual.price_sections.iter().zip(&expected.price_sections) {
        assert_eq!(actual.kind, expected.kind);
        if actual.kind == base_search::db::PriceMetricKind::ValuePerNetKg
            && actual.cohorts.len() != 1
        {
            assert_eq!(actual.count, 0);
            continue;
        }
        assert_eq!(actual.count, expected.count);
        assert_close(actual.average, expected.average, "price average");
        assert_close(actual.minimum, expected.minimum, "price minimum");
        assert_close(actual.maximum, expected.maximum, "price maximum");
        assert_close(actual.median, expected.median, "price median");
        assert_close(actual.p25, expected.p25, "price p25");
        assert_close(actual.p75, expected.p75, "price p75");
    }
}

#[test]
fn persisted_rollups_match_sqlite_and_detail_duckdb_for_single_usd() {
    let dir = tempfile::tempdir().unwrap();
    let records = vec![
        row(
            "2024-01-15",
            "INV-001",
            "ACME 1001",
            "8517130000",
            "1000",
            "USD",
            "10",
        ),
        row(
            "2024-02-20",
            "INV-002",
            "BETA 1002",
            "8504405500",
            "750",
            "usd",
            "5",
        ),
        row(
            "2025-01-05",
            "INV-003",
            "ACME 1001",
            "8517130000",
            "2000",
            "USD",
            "20",
        ),
    ];
    let (db, sqlite_path, projection_path) = build_fixture(dir.path(), &records);
    let meta = duckdb_olap::read_projection_meta(&projection_path).unwrap();
    assert_eq!(
        meta.rollup_schema_version,
        duckdb_olap::ROLLUP_SCHEMA_VERSION
    );
    assert_eq!(meta.rollup_rules_version, duckdb_olap::ROLLUP_RULES_VERSION);
    assert_eq!(
        meta.rollup_fingerprint,
        duckdb_olap::rollup_contract_fingerprint()
    );

    let projection = Connection::open(&projection_path).unwrap();
    let baseline_rows: i64 = projection
        .query_row(
            "SELECT COUNT(*) FROM rollup_price_per_kg_baselines",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(baseline_rows > 0);
    drop(projection);

    for query in [
        Query::default(),
        Query {
            filters: base_search::db::Filters {
                year: "2024".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
    ] {
        for scope in [
            None,
            Some(AnalyticsScope::Companies),
            Some(AnalyticsScope::Products),
            Some(AnalyticsScope::Countries),
            Some(AnalyticsScope::Prices),
        ] {
            let sqlite = db.analytics_scoped(&query, 50, scope, 10).unwrap();
            let detail =
                duckdb_olap::analytics_scoped_detail(&projection_path, &query, 50, scope, 10)
                    .unwrap();
            let (rollup, source) =
                duckdb_olap::analytics_scoped_with_source(&projection_path, &query, 50, scope, 10)
                    .unwrap();
            assert_eq!(source, DuckAnalyticsSource::Rollup);
            assert_analytics_match(&detail, &sqlite);
            assert_analytics_match(&rollup, &sqlite);
        }
    }

    let engine = DuckDbAnalyticsEngine::new(&sqlite_path, &projection_path);
    assert!(engine.is_current().unwrap());
    let via_contract = engine
        .analytics(&Query::default(), 50, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    let sqlite = db
        .analytics_scoped(&Query::default(), 50, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    assert_analytics_match(&via_contract, &sqlite);
}

#[test]
fn mixed_currency_rollups_are_partitioned_and_never_used_as_usd() {
    let dir = tempfile::tempdir().unwrap();
    let records = vec![
        row(
            "2024-01-15",
            "INV-USD",
            "ACME 1001",
            "8517130000",
            "1000",
            "USD",
            "10",
        ),
        row(
            "2024-01-16",
            "INV-EUR",
            "BETA 1002",
            "8517130000",
            "900",
            "EUR",
            "10",
        ),
    ];
    let (db, _, projection_path) = build_fixture(dir.path(), &records);
    let projection = Connection::open(&projection_path).unwrap();
    let (total, mode): (Option<f64>, String) = projection
        .query_row(
            "SELECT total_value_usd, monetary_mode FROM rollup_overview
             WHERE record_scope = 'canonical' AND year_key = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(total, None);
    assert_eq!(mode, "partitioned");

    let mut statement = projection
        .prepare(
            "SELECT currency_key, total_value FROM rollup_currency_totals
             WHERE record_scope = 'canonical' AND year_key = 0
             ORDER BY currency_key",
        )
        .unwrap();
    let currency_totals = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(statement);
    assert_eq!(currency_totals.len(), 2);
    assert_eq!(currency_totals[0].0, "EUR");
    assert_close(currency_totals[0].1, 900.0, "EUR total");
    assert_eq!(currency_totals[1].0, "USD");
    assert_close(currency_totals[1].1, 1000.0, "USD total");
    drop(projection);

    let query = Query::default();
    let sqlite = db
        .analytics_scoped(&query, 50, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    let detail = duckdb_olap::analytics_scoped_detail(
        &projection_path,
        &query,
        50,
        Some(AnalyticsScope::Companies),
        10,
    )
    .unwrap();
    let (automatic, source) = duckdb_olap::analytics_scoped_with_source(
        &projection_path,
        &query,
        50,
        Some(AnalyticsScope::Companies),
        10,
    )
    .unwrap();
    assert_eq!(source, DuckAnalyticsSource::Detail);
    assert_analytics_match(&detail, &sqlite);
    assert_analytics_match(&automatic, &sqlite);
}

#[test]
fn unknown_currency_makes_monetary_rollups_explicitly_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let records = vec![
        row(
            "2024-01-15",
            "INV-USD",
            "ACME 1001",
            "8517130000",
            "1000",
            "USD",
            "10",
        ),
        row(
            "2024-01-16",
            "INV-UNKNOWN",
            "BETA 1002",
            "8517130000",
            "900",
            "",
            "10",
        ),
    ];
    let (db, _, projection_path) = build_fixture(dir.path(), &records);
    let projection = Connection::open(&projection_path).unwrap();
    let (total, mode): (Option<f64>, String) = projection
        .query_row(
            "SELECT total_value_usd, monetary_mode FROM rollup_overview
             WHERE record_scope = 'canonical' AND year_key = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(total, None);
    assert_eq!(mode, "unavailable");
    let unknown_rows: i64 = projection
        .query_row(
            "SELECT valued_rows FROM rollup_currency_totals
             WHERE record_scope = 'canonical' AND year_key = 0
               AND starts_with(currency_key, '__unknown__')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown_rows, 1);
    drop(projection);

    let query = Query::default();
    let (automatic, source) = duckdb_olap::analytics_scoped_with_source(
        &projection_path,
        &query,
        50,
        Some(AnalyticsScope::Companies),
        10,
    )
    .unwrap();
    assert_eq!(source, DuckAnalyticsSource::Detail);
    let sqlite = db
        .analytics_scoped(&query, 50, Some(AnalyticsScope::Companies), 10)
        .unwrap();
    assert_analytics_match(&automatic, &sqlite);
}

#[test]
fn adapter_rejects_stale_or_tampered_rollup_generation() {
    let dir = tempfile::tempdir().unwrap();
    let records = vec![row(
        "2024-01-15",
        "INV-001",
        "ACME 1001",
        "8517130000",
        "1000",
        "USD",
        "10",
    )];
    let (db, sqlite_path, projection_path) = build_fixture(dir.path(), &records);
    let engine = DuckDbAnalyticsEngine::new(&sqlite_path, &projection_path);
    assert!(engine.is_current().unwrap());

    db.diagnostic_execute("UPDATE records SET value_num = value_num + 1 WHERE id = 1")
        .unwrap();
    assert!(!engine.is_current().unwrap());
    let error = engine
        .analytics(&Query::default(), 10, None, 10)
        .unwrap_err();
    assert!(error.contains("stale"), "{error}");

    duckdb_olap::build_projection_atomic(&sqlite_path, &projection_path).unwrap();
    assert!(engine.is_current().unwrap());
    let projection = Connection::open(&projection_path).unwrap();
    projection
        .execute(
            "UPDATE projection_meta SET value = 'tampered'
             WHERE key = 'rollup_rules_version'",
            [],
        )
        .unwrap();
    drop(projection);
    assert!(!engine.is_current().unwrap());
}

#[test]
fn concurrent_read_only_adapter_calls_share_a_projection_safely() {
    let dir = tempfile::tempdir().unwrap();
    let records = (0..100)
        .map(|index| {
            row(
                "2024-01-15",
                &format!("INV-{index:04}"),
                &format!("COMPANY {}", index % 10),
                "8517130000",
                "1000",
                "USD",
                "10",
            )
        })
        .collect::<Vec<_>>();
    let (_, sqlite_path, projection_path) = build_fixture(dir.path(), &records);
    let engine = Arc::new(DuckDbAnalyticsEngine::new(sqlite_path, projection_path));
    let barrier = Arc::new(Barrier::new(5));
    let workers = (0..4)
        .map(|_| {
            let engine = Arc::clone(&engine);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                engine
                    .analytics(&Query::default(), 20, Some(AnalyticsScope::Companies), 10)
                    .unwrap()
                    .overview
                    .row_count
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), 100);
    }
}

#[test]
#[ignore = "focused OLAP benchmark; run explicitly for release performance evidence"]
fn benchmark_persisted_rollup_against_detail_projection() {
    let dir = tempfile::tempdir().unwrap();
    let sqlite_path = dir.path().join("rollup-benchmark.db");
    let mut db = Db::open(&sqlite_path).unwrap();
    let shape = TableShape::from_headers(HEADERS.iter().map(|header| (*header).to_string()));
    db.remember_table_shape(&shape);
    db.remember_extra_headers(HEADERS);
    db.begin_import_file().unwrap();
    for chunk in 0..20 {
        let records = (0..5_000)
            .map(|offset| {
                let index = chunk * 5_000 + offset;
                row(
                    if index % 2 == 0 {
                        "2024-01-15"
                    } else {
                        "2025-02-20"
                    },
                    &format!("INV-{index:08}"),
                    &format!("COMPANY {:04}", index % 2_000),
                    &format!("{:010}", 8_500_000_000u64 + (index % 10_000) as u64),
                    &(100 + index % 5_000).to_string(),
                    "USD",
                    &(1 + index % 100).to_string(),
                )
            })
            .collect::<Vec<_>>();
        db.insert_batch("benchmark.csv", &records).unwrap();
    }
    db.commit_import_file().unwrap();
    let projection_path = duckdb_olap::default_projection_path(&sqlite_path);
    duckdb_olap::build_projection_atomic(&sqlite_path, &projection_path).unwrap();
    let query = Query::default();

    let detail_started = Instant::now();
    let detail = duckdb_olap::analytics_scoped_detail(
        &projection_path,
        &query,
        50,
        Some(AnalyticsScope::Companies),
        10,
    )
    .unwrap();
    let detail_elapsed = detail_started.elapsed();
    let rollup_started = Instant::now();
    let (rollup, source) = duckdb_olap::analytics_scoped_with_source(
        &projection_path,
        &query,
        50,
        Some(AnalyticsScope::Companies),
        10,
    )
    .unwrap();
    let rollup_elapsed = rollup_started.elapsed();
    assert_eq!(source, DuckAnalyticsSource::Rollup);
    assert_analytics_match(&rollup, &detail);
    eprintln!(
        "100k rows: detail={:.3}ms rollup={:.3}ms",
        detail_elapsed.as_secs_f64() * 1_000.0,
        rollup_elapsed.as_secs_f64() * 1_000.0
    );
}
