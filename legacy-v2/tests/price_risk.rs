use std::path::Path;
use std::sync::atomic::AtomicBool;

use base_search::db::{Db, Query, RecordScope, RiskConfidence};
use base_search::domain::table::SemanticField;
use base_search::import;

#[derive(Clone)]
struct RiskRow {
    document: String,
    date: String,
    code: String,
    value: String,
    weight: String,
    currency: String,
    weight_unit: String,
    brand: String,
    country: String,
}

impl RiskRow {
    fn priced(index: usize, code: &str, price: f64) -> Self {
        Self {
            document: format!("DOC-{index:05}"),
            date: "2024-02-15".to_string(),
            code: code.to_string(),
            value: price.to_string(),
            weight: "1".to_string(),
            currency: "USD".to_string(),
            weight_unit: "kg".to_string(),
            brand: "ACME".to_string(),
            country: "CN".to_string(),
        }
    }
}

fn write_fixture(path: &Path, rows: &[RiskRow]) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let headers = [
        "Document number",
        "Date",
        "Product code",
        "Amount",
        "Net weight",
        "Currency",
        "Weight unit",
        "Brand",
        "Origin country",
    ];
    for (column, header) in headers.iter().enumerate() {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    for (index, row) in rows.iter().enumerate() {
        for (column, value) in [
            row.document.as_str(),
            row.date.as_str(),
            row.code.as_str(),
            row.value.as_str(),
            row.weight.as_str(),
            row.currency.as_str(),
            row.weight_unit.as_str(),
            row.brand.as_str(),
            row.country.as_str(),
        ]
        .iter()
        .enumerate()
        {
            sheet
                .write_string((index + 1) as u32, column as u16, *value)
                .unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn open_fixture(rows: &[RiskRow]) -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("risk.xlsx");
    let database = dir.path().join("risk.db");
    write_fixture(&source, rows);

    let mut db = Db::open(&database).unwrap();
    let cancel = AtomicBool::new(false);
    let summary = import::import_file(&mut db, &source, &cancel, &mut |_, _, _| {});
    assert_eq!(summary.error, None);
    assert_eq!(summary.imported, rows.len() as u64);
    for (id, semantic) in [
        ("document_number", SemanticField::DeclarationNumber),
        ("date", SemanticField::Date),
        ("product_code", SemanticField::ProductCode),
        ("amount", SemanticField::Value),
        ("net_weight", SemanticField::NetWeight),
        ("currency", SemanticField::Currency),
        ("weight_unit", SemanticField::WeightUnit),
        ("brand", SemanticField::Trademark),
        ("origin_country", SemanticField::OriginCountry),
    ] {
        assert!(db.set_column_semantic(id, Some(semantic)), "missing {id}");
    }
    (dir, db)
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-8,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn risk_never_mixes_currency_or_weight_unit() {
    let mut rows = Vec::new();
    for index in 0..20 {
        rows.push(RiskRow::priced(index, "SKU-COMP", 100.0 + index as f64));
    }
    let target_document = "TARGET-COMPARABLE".to_string();
    let mut target = RiskRow::priced(20, "SKU-COMP", 10.0);
    target.document.clone_from(&target_document);
    rows.push(target);

    for index in 0..20 {
        let mut row = RiskRow::priced(100 + index, "SKU-COMP", 10.0 + index as f64 / 100.0);
        row.currency = "EUR".to_string();
        rows.push(row);
    }
    for index in 0..20 {
        let mut row = RiskRow::priced(200 + index, "SKU-COMP", 10.0 + index as f64 / 100.0);
        row.weight_unit = "lb".to_string();
        rows.push(row);
    }

    let (_dir, db) = open_fixture(&rows);
    let result = db.undervaluation(&Query::default(), 0.7, 20, 100).unwrap();

    assert!(result.available);
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.declaration_number, target_document);
    assert_eq!(row.cohort.currency, "USD");
    assert_eq!(row.cohort.weight_unit, "KG");
    assert_eq!(row.cohort.period, "2024-Q1");
    assert_eq!(row.cohort.sample_count, 21);
    assert_close(row.cohort.median, 109.0);
    assert_close(row.cohort.p25, 104.0);
    assert_close(row.cohort.p75, 114.0);
    assert!(row.reason.contains("below"));
    assert!(row.deviation_percent > 90.0);
}

#[test]
fn risk_uses_brand_and_country_only_when_the_refined_cohort_is_large_enough() {
    let mut rows = Vec::new();
    for index in 0..20 {
        rows.push(RiskRow::priced(index, "SKU-SPECIFIC", 100.0 + index as f64));
    }
    let mut target = RiskRow::priced(20, "SKU-SPECIFIC", 10.0);
    target.document = "TARGET-SPECIFIC".to_string();
    rows.push(target);
    for index in 0..20 {
        let mut row = RiskRow::priced(100 + index, "SKU-SPECIFIC", 10.0 + index as f64 / 100.0);
        row.brand = "BETA".to_string();
        rows.push(row);
    }

    let countries = [
        "AR", "AT", "AU", "BE", "BR", "CA", "CH", "CL", "CZ", "DE", "DK", "EE", "ES", "FI", "FR",
        "GB", "GR", "HU", "IE", "IN",
    ];
    for (index, country) in countries.iter().enumerate() {
        let mut row = RiskRow::priced(200 + index, "SKU-FALLBACK", 100.0 + index as f64);
        row.brand = format!("BRAND-{index}");
        row.country = (*country).to_string();
        rows.push(row);
    }
    let mut fallback = RiskRow::priced(220, "SKU-FALLBACK", 10.0);
    fallback.document = "TARGET-FALLBACK".to_string();
    fallback.brand = "SPARSE-BRAND".to_string();
    fallback.country = "ZZ".to_string();
    rows.push(fallback);

    let (_dir, db) = open_fixture(&rows);
    let result = db.undervaluation(&Query::default(), 0.7, 20, 100).unwrap();

    let specific = result
        .rows
        .iter()
        .find(|row| row.declaration_number == "TARGET-SPECIFIC")
        .unwrap();
    assert_eq!(specific.cohort.brand.as_deref(), Some("ACME"));
    assert_eq!(specific.cohort.country.as_deref(), Some("CN"));
    assert_eq!(specific.cohort.sample_count, 21);
    assert!(
        !specific
            .limitations
            .iter()
            .any(|item| item.code == "brand_cohort_too_small")
    );

    let fallback = result
        .rows
        .iter()
        .find(|row| row.declaration_number == "TARGET-FALLBACK")
        .unwrap();
    assert_eq!(fallback.cohort.brand, None);
    assert_eq!(fallback.cohort.country, None);
    assert!(
        fallback
            .limitations
            .iter()
            .any(|item| item.code == "brand_cohort_too_small")
    );
    assert!(
        fallback
            .limitations
            .iter()
            .any(|item| item.code == "country_cohort_too_small")
    );
}

#[test]
fn risk_enforces_twenty_samples_and_requires_the_robust_iqr_cutoff() {
    let mut rows = Vec::new();
    for index in 0..18 {
        rows.push(RiskRow::priced(index, "SKU-SPARSE", 100.0 + index as f64));
    }
    rows.push(RiskRow::priced(18, "SKU-SPARSE", 10.0));

    let broad_prices = [
        40.0, 80.0, 80.0, 80.0, 80.0, 80.0, 90.0, 90.0, 90.0, 90.0, 90.0, 110.0, 110.0, 110.0,
        110.0, 110.0, 120.0, 120.0, 120.0, 120.0, 120.0,
    ];
    for (index, price) in broad_prices.iter().enumerate() {
        let mut row = RiskRow::priced(100 + index, "SKU-BROAD", *price);
        row.brand = String::new();
        row.country = String::new();
        rows.push(row);
    }

    let (_dir, db) = open_fixture(&rows);
    let result = db.undervaluation(&Query::default(), 0.5, 5, 100).unwrap();

    assert_eq!(result.contract.min_samples, 20);
    assert_eq!(result.checked_codes, 1);
    assert_eq!(result.flagged_rows, 0);
    assert!(result.rows.is_empty());
    assert_eq!(result.exclusions.insufficient_cohort, 19);
}

#[test]
fn risk_reports_safe_exclusions_for_missing_non_numeric_and_zero_inputs() {
    let mut rows = Vec::new();
    for index in 0..19 {
        rows.push(RiskRow::priced(index, "SKU-SAFE", 100.0 + index as f64));
    }
    let mut target = RiskRow::priced(19, "SKU-SAFE", 10.0);
    target.document = "TARGET-SAFE".to_string();
    rows.push(target);

    let mut invalid_rows = Vec::new();
    let mut missing_code = RiskRow::priced(100, "", 100.0);
    missing_code.document = "MISSING-CODE".to_string();
    invalid_rows.push(missing_code);
    let mut missing_period = RiskRow::priced(101, "SKU-SAFE", 100.0);
    missing_period.date = "not a date".to_string();
    invalid_rows.push(missing_period);
    let mut missing_currency = RiskRow::priced(102, "SKU-SAFE", 100.0);
    missing_currency.currency = String::new();
    invalid_rows.push(missing_currency);
    let mut missing_unit = RiskRow::priced(103, "SKU-SAFE", 100.0);
    missing_unit.weight_unit = String::new();
    invalid_rows.push(missing_unit);
    for (index, value) in ["", "not-a-number", "0"].iter().enumerate() {
        let mut row = RiskRow::priced(110 + index, "SKU-SAFE", 100.0);
        row.value = (*value).to_string();
        invalid_rows.push(row);
    }
    for (index, weight) in ["", "not-a-number", "0", "-1"].iter().enumerate() {
        let mut row = RiskRow::priced(120 + index, "SKU-SAFE", 100.0);
        row.weight = (*weight).to_string();
        invalid_rows.push(row);
    }
    rows.extend(invalid_rows);

    let (_dir, db) = open_fixture(&rows);
    let result = db.undervaluation(&Query::default(), 0.7, 20, 100).unwrap();

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].declaration_number, "TARGET-SAFE");
    assert_eq!(result.exclusions.missing_product_code, 1);
    assert_eq!(result.exclusions.missing_period, 1);
    assert_eq!(result.exclusions.missing_currency, 1);
    assert_eq!(result.exclusions.missing_weight_unit, 1);
    assert_eq!(result.exclusions.invalid_value, 3);
    assert_eq!(result.exclusions.invalid_weight, 4);
    assert_eq!(result.eligible_rows, 20);
    assert_eq!(result.evaluated_rows, 20);
}

#[test]
fn risk_preserves_canonical_and_occurrence_record_scopes() {
    let mut rows = Vec::new();
    for index in 0..20 {
        rows.push(RiskRow::priced(index, "SKU-DUP", 100.0 + index as f64));
    }
    let mut target = RiskRow::priced(20, "SKU-DUP", 10.0);
    target.document = "TARGET-DUP".to_string();
    rows.push(target.clone());
    rows.push(target);

    let (_dir, db) = open_fixture(&rows);
    let canonical = db.undervaluation(&Query::default(), 0.7, 20, 100).unwrap();
    let occurrences = db
        .undervaluation(
            &Query {
                record_scope: RecordScope::Occurrences,
                ..Query::default()
            },
            0.7,
            20,
            100,
        )
        .unwrap();

    assert_eq!(canonical.rows.len(), 1);
    assert_eq!(canonical.rows[0].cohort.sample_count, 21);
    assert_eq!(occurrences.rows.len(), 2);
    assert!(
        occurrences
            .rows
            .iter()
            .all(|row| row.cohort.sample_count == 22)
    );
    assert!(canonical.rows[0].confidence != RiskConfidence::High);
}

#[test]
fn risk_never_adds_value_or_gap_across_currencies() {
    let mut rows = Vec::new();
    for index in 0..20 {
        rows.push(RiskRow::priced(index, "SKU-MONEY", 100.0 + index as f64));
    }
    let mut usd_target = RiskRow::priced(20, "SKU-MONEY", 10.0);
    usd_target.document = "TARGET-USD".to_string();
    rows.push(usd_target);

    for index in 0..20 {
        let mut row = RiskRow::priced(100 + index, "SKU-MONEY", 200.0 + index as f64);
        row.currency = "EUR".to_string();
        rows.push(row);
    }
    let mut eur_target = RiskRow::priced(120, "SKU-MONEY", 20.0);
    eur_target.document = "TARGET-EUR".to_string();
    eur_target.currency = "EUR".to_string();
    rows.push(eur_target);

    let (_dir, db) = open_fixture(&rows);
    let result = db.undervaluation(&Query::default(), 0.7, 20, 100).unwrap();

    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.currency_totals.len(), 2);
    let usd = result
        .currency_totals
        .iter()
        .find(|total| total.currency == "USD")
        .unwrap();
    let eur = result
        .currency_totals
        .iter()
        .find(|total| total.currency == "EUR")
        .unwrap();
    assert_eq!(usd.flagged_rows, 1);
    assert_eq!(eur.flagged_rows, 1);
    assert_close(usd.flagged_value, 10.0);
    assert_close(eur.flagged_value, 20.0);
    assert_close(result.flagged_value, 0.0);
    assert_close(result.estimated_gap, 0.0);
    assert!(
        result
            .limitations
            .iter()
            .any(|item| item.code == "multiple_currencies_not_summed")
    );
}
