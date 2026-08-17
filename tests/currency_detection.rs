//! The customs layout never names a currency, but it states the same amount
//! twice: `ФВ вал.контр` is the invoice value in the contract currency, and
//! `РФВ` is that same invoice value in dollars per kilogram. So `РФВ × вага`
//! reproduces `ФВ` exactly when the contract is in dollars, and misses by the
//! exchange rate when it is not. These tests pin that this is read from the
//! data rather than assumed, and that it stays quiet when the evidence is
//! absent or contradicts itself.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use base_search::db::{AnalyticsSectionKind, Db, Query};
use base_search::domain::table::SemanticField;
use base_search::import;

/// Writes a customs-shaped sheet where every row's value is `rate` times the
/// dollar amount implied by `РФВ × Нетто`. `rate = 1.0` is a dollar contract.
fn write_customs(path: &Path, rows: usize, rate: f64, with_rfv: bool) {
    write_customs_as(path, rows, rate, with_rfv, "SHENZHEN TECH CO");
}

/// The same sheet under a named sender, so one workspace can hold two
/// companies whose contracts are in different currencies.
fn write_customs_as(path: &Path, rows: usize, rate: f64, with_rfv: bool, sender: &str) {
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
        sheet.write_string(row, 2, sender).unwrap();
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

/// A workspace with two currencies in it is not a workspace without numbers.
///
/// The query as a whole spans two, so no single total is true of it — but a
/// company that has only ever traded in one still has an honest total, and that
/// is the number a person opens the program to read. Before this, every group
/// row in such a workspace looked for the query-level bucket, found none, and
/// reported nothing at all.
#[test]
fn a_company_keeps_its_own_currency_when_the_workspace_has_several() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("two-currencies.xlsx");
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    for (column, header) in [
        "Order Date",
        "Invoice No",
        "Supplier",
        "Buyer",
        "SKU",
        "Amount",
        "Net weight",
        "Currency",
        "Weight unit",
    ]
    .iter()
    .enumerate()
    {
        sheet.write_string(0, column as u16, *header).unwrap();
    }
    // Two suppliers, one invoicing in dollars and one in euros, in one month.
    for index in 0..40u32 {
        let euros = index % 2 == 0;
        let row = [
            "2024-03-15".to_string(),
            format!("INV-{index:03}"),
            if euros {
                "HAMBURG HANDEL"
            } else {
                "SHENZHEN TECH"
            }
            .to_string(),
            "Buyer A".to_string(),
            "SKU-1".to_string(),
            // Deliberately far apart, so a share computed on value cannot be
            // mistaken for one computed on weight.
            if euros { 1000 + index } else { 100 + index }.to_string(),
            "10".to_string(),
            if euros { "EUR" } else { "USD" }.to_string(),
            "kg".to_string(),
        ];
        for (column, value) in row.iter().enumerate() {
            sheet.write_string(index + 1, column as u16, value).unwrap();
        }
    }
    workbook.save(&source).unwrap();

    let mut db = Db::open(&dir.path().join("two-currencies.db")).unwrap();
    assert_eq!(
        import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {}).error,
        None
    );
    for (column, semantic) in [
        ("order_date", SemanticField::Date),
        ("invoice_no", SemanticField::DeclarationNumber),
        ("supplier", SemanticField::Sender),
        ("buyer", SemanticField::Recipient),
        ("sku", SemanticField::ProductCode),
        ("amount", SemanticField::Value),
        ("net_weight", SemanticField::NetWeight),
        ("currency", SemanticField::Currency),
        ("weight_unit", SemanticField::WeightUnit),
    ] {
        assert!(
            db.set_column_semantic(column, Some(semantic)),
            "missing shape column {column}"
        );
    }

    let analytics = db.analytics(&Query::default(), 10).unwrap();
    assert!(
        analytics
            .overview
            .measures
            .single_currency_total()
            .is_none(),
        "the workspace itself spans two currencies: {:?}",
        analytics.overview.measures.currency_totals
    );

    let senders = analytics
        .company_sections
        .iter()
        .find(|section| section.kind == AnalyticsSectionKind::Senders)
        .expect("the senders section is part of the company sections");
    let row_for = |needle: &str| {
        senders
            .rows
            .iter()
            .find(|row| row.label.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle} in {:?}", senders.rows))
    };

    for (supplier, code) in [("SHENZHEN", "USD"), ("HAMBURG", "EUR")] {
        let row = row_for(supplier);
        let (total, shown) = row
            .measures
            .single_currency_total()
            .unwrap_or_else(|| panic!("{supplier} trades in one currency and must report it"));
        assert_eq!(shown, code, "{supplier} is labelled with its own currency");
        assert!(total > 0.0, "{supplier} carries its own total");
        assert!(
            total < analytics.overview.total_value_usd,
            "and not the whole workspace's"
        );
    }

    // The row that covers both suppliers is the one the smoke test caught
    // reporting a plain zero: it holds 22 780 in two currencies, and an empty
    // bucket list is indistinguishable from having no money at all. It has to
    // carry both buckets, so the cell can say the currencies differ and the
    // hover can show what they are.
    let buyers = analytics
        .company_sections
        .iter()
        .find(|section| section.kind == AnalyticsSectionKind::Recipients)
        .expect("the recipients section is part of the company sections");
    let everyone = buyers
        .rows
        .iter()
        .find(|row| row.label.contains("Buyer A"))
        .expect("one buyer bought everything");
    assert!(
        everyone.measures.single_currency_total().is_none(),
        "this row spans two currencies and has no single total"
    );
    let mut split: Vec<(&str, f64)> = everyone
        .measures
        .currency_totals
        .iter()
        .map(|total| (total.currency.as_str(), total.total_value))
        .collect();
    split.sort_by(|left, right| left.0.cmp(right.0));
    assert_eq!(
        split,
        vec![("EUR", 20_380.0), ("USD", 2_400.0)],
        "and must report each currency it does hold, not a zero"
    );
    assert_eq!(
        everyone.measures.value_per_net_weight.len(),
        2,
        "value per kilogram splits the same way"
    );

    // A share of the total value needs that total to exist. It does not here,
    // so the shares fall back to weight — each supplier carries half the
    // kilograms — instead of dividing euros by euros-plus-dollars.
    for supplier in ["SHENZHEN", "HAMBURG"] {
        let share = row_for(supplier).share_percent;
        assert!(
            (share - 50.0).abs() < 0.01,
            "{supplier} moved half the weight, so its share is 50%, not {share}"
        );
    }

    // Both suppliers ship in the same month, so that month really does span two
    // currencies and must not be handed one.
    let month = analytics
        .months
        .first()
        .expect("every row lands in one month");
    assert!(
        month.measures.single_currency_total().is_none(),
        "a month holding both contracts has no single total: {:?}",
        month.measures.currency_totals
    );
}

/// Currency keys are built from whatever the cell says, and `normalize_text_key`
/// keeps punctuation. An apostrophe therefore reaches the statement that splits
/// a row by currency, where an unescaped one ends the string literal and takes
/// the whole query down with it.
#[test]
fn a_currency_written_with_an_apostrophe_does_not_break_the_query() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("odd-currency.csv");
    let mut csv = String::from("Order Date,Invoice No,Supplier,Amount,Net weight,Currency\n");
    for index in 0..20 {
        let odd = index % 2 == 0;
        csv.push_str(&format!(
            "2024-03-15,INV-{index:03},{},{},10,{}\n",
            if odd { "ODD TRADER" } else { "PLAIN TRADER" },
            100 + index,
            if odd { "O'DOLLAR" } else { "USD" },
        ));
    }
    std::fs::write(&source, csv).unwrap();

    let mut db = Db::open(&dir.path().join("odd.db")).unwrap();
    assert_eq!(
        import::import_file(&mut db, &source, &AtomicBool::new(false), &mut |_, _, _| {}).error,
        None
    );
    for (column, semantic) in [
        ("order_date", SemanticField::Date),
        ("invoice_no", SemanticField::DeclarationNumber),
        ("supplier", SemanticField::Sender),
        ("amount", SemanticField::Value),
        ("net_weight", SemanticField::NetWeight),
        ("currency", SemanticField::Currency),
    ] {
        assert!(db.set_column_semantic(column, Some(semantic)), "{column}");
    }

    let analytics = db
        .analytics(&Query::default(), 10)
        .expect("the apostrophe must be escaped, not executed");
    let senders = analytics
        .company_sections
        .iter()
        .find(|section| section.kind == AnalyticsSectionKind::Senders)
        .expect("the senders section is part of the company sections");
    let odd = senders
        .rows
        .iter()
        .find(|row| row.label.contains("ODD"))
        .expect("the odd trader has rows");
    assert_eq!(
        odd.measures.currency_totals.len(),
        1,
        "and its rows still resolve to their one bucket: {:?}",
        odd.measures.currency_totals
    );
}
