//! The customs layout never names a currency, but it states the same amount
//! twice: `ФВ вал.контр` is the invoice value in the contract currency, and
//! `РФВ` is that same invoice value in dollars per kilogram. So `РФВ × вага`
//! reproduces `ФВ` exactly when the contract is in dollars, and misses by the
//! exchange rate when it is not. These tests pin that this is read from the
//! data rather than assumed, and that it stays quiet when the evidence is
//! absent or contradicts itself.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use base_search::db::{Db, Query};
use base_search::import;

/// Writes a customs-shaped sheet where every row's value is `rate` times the
/// dollar amount implied by `РФВ × Нетто`. `rate = 1.0` is a dollar contract.
fn write_customs(path: &Path, rows: usize, rate: f64, with_rfv: bool) {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    let mut headers = vec![
        "Номер МД",
        "Дата",
        "Відправник",
        "ЕДРПОУ",
        "Одержувач",
        "Код товару",
        "Опис товару",
        "Нетто, кг.",
        "ФВ вал.контр",
    ];
    if with_rfv {
        headers.push("РФВ Дол/кг.");
    }
    for (column, header) in headers.iter().enumerate() {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    for index in 0..rows {
        let net = 100.0 + index as f64;
        let rfv = 12.5 + (index % 7) as f64;
        let value = rfv * net * rate;
        let row = (index + 1) as u32;
        sheet
            .write_string(row, 0, format!("24UA1001100{index:05}U1"))
            .unwrap();
        sheet.write_string(row, 1, "15.03.2024").unwrap();
        sheet.write_string(row, 2, "SHENZHEN TECH CO").unwrap();
        sheet.write_string(row, 3, "33333333").unwrap();
        sheet.write_string(row, 4, "ТОВ «АЛЬФА»").unwrap();
        sheet.write_string(row, 5, "8517130000").unwrap();
        sheet.write_string(row, 6, "Телефони").unwrap();
        sheet.write_string(row, 7, format!("{net:.1}")).unwrap();
        sheet.write_string(row, 8, format!("{value:.2}")).unwrap();
        if with_rfv {
            sheet.write_string(row, 9, format!("{rfv:.4}")).unwrap();
        }
    }
    workbook.save(path).unwrap();
}

fn import_and_read_currency(
    dir: &Path,
    name: &str,
    rows: usize,
    rate: f64,
    rfv: bool,
) -> (bool, String) {
    let source = dir.join(name);
    write_customs(&source, rows, rate, rfv);
    let mut db = Db::open(&dir.join(format!("{name}.db"))).unwrap();
    let summary = import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {});
    assert_eq!(summary.error, None, "{name}");
    let analytics = db.analytics(&Query::default(), 10).unwrap();
    let total = &analytics.overview.measures.currency_totals[0];
    (total.known, total.currency.clone())
}

#[test]
fn a_dollar_contract_is_recognized_from_the_file_itself() {
    let dir = tempfile::tempdir().unwrap();
    let (known, currency) = import_and_read_currency(dir.path(), "usd.xlsx", 60, 1.0, true);
    assert!(
        known,
        "the evidence is unambiguous, so the currency is known"
    );
    assert_eq!(currency, "USD");
}

#[test]
fn a_contract_in_another_currency_is_not_called_dollars() {
    let dir = tempfile::tempdir().unwrap();
    // Values are ~0.92 of the dollar amount, as a euro contract would be.
    let (known, _) = import_and_read_currency(dir.path(), "eur.xlsx", 60, 0.92, true);
    assert!(
        !known,
        "the amounts disagree with the dollar column, so nothing may be claimed"
    );
}

#[test]
fn a_file_without_the_dollar_column_stays_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (known, _) = import_and_read_currency(dir.path(), "no-rfv.xlsx", 60, 1.0, false);
    assert!(!known, "there is nothing to compare against");
}

#[test]
fn a_handful_of_rows_is_not_enough_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let (known, _) = import_and_read_currency(dir.path(), "tiny.xlsx", 8, 1.0, true);
    assert!(
        !known,
        "a few agreeing rows can be coincidence; the threshold must hold"
    );
}

#[test]
fn a_stated_currency_outranks_the_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("stated.xlsx");
    write_customs(&source, 60, 1.0, true);
    let mut db = Db::open(&dir.path().join("stated.db")).unwrap();
    assert_eq!(
        import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {}).error,
        None
    );
    assert_eq!(
        db.analytics(&Query::default(), 10)
            .unwrap()
            .overview
            .measures
            .currency_totals[0]
            .currency,
        "USD"
    );

    // Someone whose files really are in another currency says so, and that
    // answer wins over what the file appears to show.
    assert!(db.set_workspace_fixed_values(Some("EUR"), None).is_ok());
    let totals = db.analytics(&Query::default(), 10).unwrap();
    assert_eq!(totals.overview.measures.currency_totals[0].currency, "EUR");

    // Clearing it falls back to the evidence rather than to nothing.
    assert!(db.set_workspace_fixed_values(None, None).is_ok());
    let totals = db.analytics(&Query::default(), 10).unwrap();
    assert_eq!(totals.overview.measures.currency_totals[0].currency, "USD");
}

/// The failure the previous attempt shipped: a generic table with a money
/// column got branded dollars because the importer stores any recognized value
/// column in the customs profile's physical column.
#[test]
fn a_generic_table_is_never_branded_with_a_currency() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("orders.csv");
    let mut csv = String::from("Order id,Customer,Amount\n");
    for index in 0..60 {
        csv.push_str(&format!("A-{index},Acme,{}\n", 1000 + index));
    }
    std::fs::write(&source, csv).unwrap();
    let mut db = Db::open(&dir.path().join("orders.db")).unwrap();
    assert_eq!(
        import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {}).error,
        None
    );
    let totals = db.analytics(&Query::default(), 10).unwrap();
    assert!(
        !totals.overview.measures.currency_totals[0].known,
        "an order book states no currency and must not be told it is in dollars"
    );
}

/// Two sources in one workspace keep their own answers instead of being
/// flattened into a single bucket.
#[test]
fn each_source_keeps_its_own_currency_in_a_mixed_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let customs = dir.path().join("customs.xlsx");
    write_customs(&customs, 60, 1.0, true);
    let orders = dir.path().join("orders.csv");
    let mut csv = String::from("Order id,Customer,Amount\n");
    for index in 0..60 {
        csv.push_str(&format!("B-{index},Globex,{}\n", 500 + index));
    }
    std::fs::write(&orders, csv).unwrap();

    let mut db = Db::open(&dir.path().join("mixed.db")).unwrap();
    for source in [&customs, &orders] {
        assert_eq!(
            import::import_file(&mut db, source, &AtomicBool::new(false), &mut |_, _, _| {}).error,
            None
        );
    }

    let totals = &db
        .analytics(&Query::default(), 10)
        .unwrap()
        .overview
        .measures
        .currency_totals;
    assert_eq!(
        totals.len(),
        2,
        "the two sources must not share one bucket: {totals:?}"
    );
    assert!(
        totals
            .iter()
            .any(|total| total.known && total.currency == "USD"),
        "the customs rows keep their recognized currency"
    );
    assert!(
        totals.iter().any(|total| !total.known),
        "the order rows stay unknown rather than borrowing it"
    );
}
