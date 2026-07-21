//! Deterministic search query-plan checks and an opt-in latency benchmark.

use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use base_search::db::{Db, Filters, Query, RecordScope};
use base_search::search::{ConditionOp, ConditionValue, FieldRef, QueryCondition, QueryExpr};

const PLAN_FIXTURE_ROWS: u64 = 2_000;
const BENCH_FIXTURE_ROWS: u64 = 200_000;

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
}

impl Fixture {
    fn new(rows: u64) -> Self {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("search-performance.db");
        let mut db = Db::open(&path).expect("open fixture database");
        seed_records(&db, rows);
        db.index_fts(&AtomicBool::new(false), |_, _| {})
            .expect("index fixture FTS");
        Self { _dir: dir, db }
    }
}

fn seed_records(db: &Db, rows: u64) {
    let sql = format!(
        "BEGIN IMMEDIATE;
         WITH RECURSIVE fixture(n) AS (
             VALUES(1)
             UNION ALL
             SELECT n + 1 FROM fixture WHERE n < {rows}
         )
         INSERT INTO records (
             id, row_hash, source_file, year, dup_first_file, extra,
             declaration_number, declaration_date, product_code, description,
             sender, recipient, edrpou, trademark,
             trade_country, dispatch_country, origin_country,
             value_num, net_kg_num, gross_kg_num, quantity_num,
             sender_label, recipient_label, edrpou_label, trademark_label,
             trade_key, dispatch_key, origin_key, month,
             currency_control_value, net_kg, gross_kg, quantity
         )
         SELECT
             n,
             CAST(printf('%016x', n) AS BLOB),
             'fixture.xlsx',
             2022 + (n % 4),
             CASE WHEN n % 11 = 0 THEN 'fixture-original.xlsx' ELSE NULL END,
             printf('[[\"Custom note\",\"batch %d\"],[\"Custom score\",\"%d\"]]', n % 10, n % 1000),
             printf('%02dUA%014d', 22 + (n % 4), n),
             printf('%04d-%02d-%02d', 2022 + (n % 4), 1 + (n % 12), 1 + (n % 28)),
             CASE n % 4
                 WHEN 0 THEN printf('8504%06d', n % 1000000)
                 WHEN 1 THEN printf('8517%06d', n % 1000000)
                 WHEN 2 THEN printf('2204%06d', n % 1000000)
                 ELSE printf('0805%06d', n % 1000000)
             END,
             CASE
                 WHEN n % 20 = 0 THEN 'precision turbine alpha'
                 WHEN n % 5 = 0 THEN 'industrial pump beta'
                 ELSE 'bulk commodity gamma'
             END,
             CASE n % 3 WHEN 0 THEN 'ACME GmbH' WHEN 1 THEN 'Northwind SA' ELSE 'Contoso SpA' END,
             CASE n % 4 WHEN 0 THEN 'Atlas LLC' WHEN 1 THEN 'Beacon Ltd' WHEN 2 THEN 'Cedar BV' ELSE 'Delta Oy' END,
             CASE WHEN n % 97 = 0 THEN ' 00000042 ' ELSE printf('%08d', n % 1000) END,
             CASE WHEN n % 7 = 0 THEN 'Acme Prime' ELSE 'Other Brand' END,
             CASE n % 3 WHEN 0 THEN 'CN' WHEN 1 THEN 'DE' ELSE 'PL' END,
             CASE n % 3 WHEN 0 THEN 'DE' WHEN 1 THEN 'PL' ELSE 'CN' END,
             CASE n % 3 WHEN 0 THEN 'PL' WHEN 1 THEN 'CN' ELSE 'DE' END,
             CAST(n % 100000 AS REAL),
             CAST(n % 5000 AS REAL) / 10.0,
             CAST(n % 7000 AS REAL) / 10.0,
             CAST(n % 250 AS REAL),
             CASE n % 3 WHEN 0 THEN 'ACME GmbH' WHEN 1 THEN 'Northwind SA' ELSE 'Contoso SpA' END,
             CASE n % 4 WHEN 0 THEN 'Atlas LLC' WHEN 1 THEN 'Beacon Ltd' WHEN 2 THEN 'Cedar BV' ELSE 'Delta Oy' END,
             CASE WHEN n % 97 = 0 THEN '00000042' ELSE printf('%08d', n % 1000) END,
             CASE WHEN n % 7 = 0 THEN 'Acme Prime' ELSE 'Other Brand' END,
             CASE n % 3 WHEN 0 THEN 'CN' WHEN 1 THEN 'DE' ELSE 'PL' END,
             CASE n % 3 WHEN 0 THEN 'DE' WHEN 1 THEN 'PL' ELSE 'CN' END,
             CASE n % 3 WHEN 0 THEN 'PL' WHEN 1 THEN 'CN' ELSE 'DE' END,
             printf('%04d-%02d', 2022 + (n % 4), 1 + (n % 12)),
             CAST(n % 100000 AS TEXT),
             printf('%.1f', CAST(n % 5000 AS REAL) / 10.0),
             printf('%.1f', CAST(n % 7000 AS REAL) / 10.0),
             CAST(n % 250 AS TEXT)
         FROM fixture;
         COMMIT;"
    );
    db.diagnostic_execute_batch(&sql)
        .expect("seed deterministic fixture");
}

fn plan_details(db: &Db, sql: &str) -> Vec<String> {
    db.diagnostic_query_rows(&format!("EXPLAIN QUERY PLAN {sql}"), 100)
        .expect("explain query plan")
        .into_iter()
        .filter_map(|row| row.get(3).cloned())
        .collect()
}

fn assert_plan_uses(db: &Db, label: &str, sql: &str, index: &str) {
    let details = plan_details(db, sql);
    assert!(
        details.iter().any(|line| line.contains(index)),
        "{label} did not use {index}: {details:#?}"
    );
}

#[test]
fn materialized_search_filters_have_indexed_query_plans() {
    let fixture = Fixture::new(PLAN_FIXTURE_ROWS);
    let db = &fixture.db;

    assert_plan_uses(
        db,
        "year",
        "SELECT COUNT(*) FROM records r
         WHERE (r.year = 2024 OR (r.year IS NULL AND CAST(SUBSTR(r.month, 1, 4) AS INTEGER) = 2024))
           AND r.dup_first_file IS NULL",
        "idx_records_year_scope",
    );
    assert_plan_uses(
        db,
        "origin country",
        "SELECT COUNT(*) FROM records r
         WHERE r.origin_key = 'CN' AND r.dup_first_file IS NULL",
        "idx_records_origin_key_scope",
    );
    assert_plan_uses(
        db,
        "company code",
        "SELECT COUNT(*) FROM records r
         WHERE text_key(r.edrpou) = text_key('00000042') AND r.dup_first_file IS NULL",
        "idx_records_edrpou_key_scope",
    );
    assert_plan_uses(
        db,
        "numeric range",
        "SELECT COUNT(*) FROM records r
         WHERE r.value_num BETWEEN 50000 AND 70000 AND r.dup_first_file IS NULL",
        "idx_records_value_num_scope",
    );
    assert_plan_uses(
        db,
        "canonical broad page",
        "SELECT r.id FROM records r WHERE r.dup_first_file IS NULL ORDER BY r.id DESC LIMIT 51",
        "idx_records_canonical_id",
    );

    let fts_details = plan_details(
        db,
        "SELECT r.id FROM records r
         WHERE r.id IN (
             SELECT rowid FROM records_fts WHERE records_fts MATCH '\"precision\"'
         ) OR (r.id > 2000 AND cyr_contains(r.description, 'precision'))
         ORDER BY r.declaration_date DESC, r.id DESC LIMIT 51",
    );
    assert!(
        fts_details
            .iter()
            .any(|line| line.contains("records_fts") && line.contains("VIRTUAL TABLE INDEX")),
        "FTS page did not use the FTS virtual index: {fts_details:#?}"
    );
    assert!(
        !fts_details.iter().any(|line| line == "SCAN r"),
        "FTS page fell back to a full records scan: {fts_details:#?}"
    );
}

#[test]
fn fixture_covers_fts_structured_and_arbitrary_column_semantics() {
    let fixture = Fixture::new(PLAN_FIXTURE_ROWS);
    let db = &fixture.db;

    let text = Query {
        text: "precision".to_string(),
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert_eq!(db.count(&text).unwrap(), PLAN_FIXTURE_ROWS / 20);

    let structured = Query {
        filters: Filters {
            year: "2024".to_string(),
            origin_country: "CN".to_string(),
            ..Default::default()
        },
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert!(db.count(&structured).unwrap() > 0);

    let arbitrary = Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::Extra("Custom note".to_string()),
            op: ConditionOp::Contains,
            value: ConditionValue::Single("batch 3".to_string()),
            negated: false,
        })),
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert_eq!(db.count(&arbitrary).unwrap(), PLAN_FIXTURE_ROWS / 10);
}

#[test]
fn pure_fts_count_does_not_double_count_a_newly_indexed_tail_row() {
    let fixture = Fixture::new(PLAN_FIXTURE_ROWS);
    let db = &fixture.db;
    let tail_id = PLAN_FIXTURE_ROWS + 1;
    db.diagnostic_execute_batch(&format!(
        "INSERT INTO records (
             id, row_hash, source_file, year, declaration_date, description
         ) VALUES (
             {tail_id}, CAST('tail-row' AS BLOB), 'tail.xlsx', 2024,
             '2024-12-31', 'precision tail row'
         );
         INSERT INTO records_fts(rowid, search_text)
         VALUES ({tail_id}, 'precision tail row');"
    ))
    .unwrap();

    // Simulate a count plan that captured the old watermark just before the
    // index chunk became visible. The row is now in both FTS and the old
    // watermark's tail, but it must still contribute exactly once.
    let query = Query {
        text: "precision".to_string(),
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    assert_eq!(db.count(&query).unwrap(), PLAN_FIXTURE_ROWS / 20 + 1);
}

fn median_duration(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn measure<T>(name: &str, warmups: usize, repeats: usize, mut operation: impl FnMut() -> T) {
    for _ in 0..warmups {
        black_box(operation());
    }
    let mut samples = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        let started = Instant::now();
        black_box(operation());
        samples.push(started.elapsed());
    }
    let median = median_duration(samples);
    println!(
        "SEARCH_BENCH {name} median_ms={:.3}",
        median.as_secs_f64() * 1000.0
    );
}

#[test]
#[ignore = "deterministic latency benchmark; run explicitly with --ignored --nocapture --test-threads=1"]
fn benchmark_search_fixture() {
    let fixture = Fixture::new(BENCH_FIXTURE_ROWS);
    let db = &fixture.db;
    let empty = Query::default();
    let fts = Query {
        text: "precision".to_string(),
        record_scope: RecordScope::Occurrences,
        ..Default::default()
    };
    let year = Query {
        filters: Filters {
            year: "2024".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let country = Query {
        filters: Filters {
            origin_country: "CN".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let company = Query {
        filters: Filters {
            edrpou: "00000042".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let numeric = Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::Column("currency_control_value".to_string()),
            op: ConditionOp::Range,
            value: ConditionValue::Range {
                from: Some("50000".to_string()),
                to: Some("70000".to_string()),
            },
            negated: false,
        })),
        ..Default::default()
    };
    let arbitrary = Query {
        advanced: Some(QueryExpr::Condition(QueryCondition {
            field: FieldRef::Extra("Custom note".to_string()),
            op: ConditionOp::Contains,
            value: ConditionValue::Single("batch 3".to_string()),
            negated: false,
        })),
        ..Default::default()
    };

    println!("SEARCH_BENCH fixture_rows={BENCH_FIXTURE_ROWS}");
    measure("empty_count", 2, 7, || db.count(&empty).unwrap());
    measure("empty_page", 2, 7, || {
        db.search_page_dynamic(&empty, 51, 0).unwrap()
    });
    measure("fts_count", 2, 7, || db.count(&fts).unwrap());
    measure("fts_page", 2, 7, || {
        db.search_page_dynamic(&fts, 51, 0).unwrap()
    });
    measure("year_count", 2, 7, || db.count(&year).unwrap());
    measure("country_count", 2, 7, || db.count(&country).unwrap());
    measure("company_count", 2, 7, || db.count(&company).unwrap());
    measure("numeric_count", 2, 7, || db.count(&numeric).unwrap());
    measure("arbitrary_extra_count", 2, 7, || {
        db.count(&arbitrary).unwrap()
    });
    measure("fts_count_plus_page", 2, 7, || {
        let total = db.count(&fts).unwrap();
        let page = db.search_page_dynamic(&fts, 51, 0).unwrap();
        (total, page)
    });
}
