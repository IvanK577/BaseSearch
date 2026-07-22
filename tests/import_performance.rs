use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use base_search::db::{Db, ImportRecord, canonical_record_hash};
use base_search::import::{self, ImportPhase};
use base_search::schema::{COLUMNS, col_index};

const SAMPLE_ROWS: u64 = 65_536;

#[test]
#[ignore = "requires BASE_SEARCH_IMPORT_BENCH_XLSX and measures a real workbook"]
fn real_xlsx_import_reports_streaming_throughput() {
    let source = std::env::var_os("BASE_SEARCH_IMPORT_BENCH_XLSX")
        .map(PathBuf::from)
        .expect("set BASE_SEARCH_IMPORT_BENCH_XLSX to a workbook path");
    assert!(
        source.is_file(),
        "workbook does not exist: {}",
        source.display()
    );

    let temp = tempfile::tempdir().unwrap();
    let mut db = Db::open(&temp.path().join("import-benchmark.db")).unwrap();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut first_batch_at = None;
    let mut last_done = 0_u64;

    let summary = import::import_file(&mut db, &source, &cancel, &mut |phase, done, total| {
        if phase != ImportPhase::Inserting || done == last_done {
            return;
        }
        last_done = done;
        let elapsed = started.elapsed();
        first_batch_at.get_or_insert(elapsed);
        eprintln!(
            "IMPORT_BENCH rows={done} total={total} elapsed_ms={} rows_per_second={:.0}",
            elapsed.as_millis(),
            done as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
        );
        if done >= SAMPLE_ROWS {
            cancel.store(true, Ordering::Relaxed);
        }
    });

    assert_eq!(summary.error, None);
    assert!(summary.cancelled);
    assert_eq!(summary.imported, 0, "cancelled imports must roll back");
    assert_eq!(db.total_rows(), 0, "benchmark must leave no imported rows");
    eprintln!(
        "IMPORT_BENCH_RESULT first_batch_ms={} sampled_rows={} total_ms={}",
        first_batch_at
            .expect("at least one batch must be observed")
            .as_millis(),
        last_done,
        started.elapsed().as_millis(),
    );
}

#[test]
#[ignore = "focused SQLite insert benchmark"]
fn sqlite_batch_insert_throughput_does_not_collapse() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Db::open(&temp.path().join("insert-benchmark.db")).unwrap();
    db.begin_import_file().unwrap();
    let started = Instant::now();
    let mut inserted = 0_u64;

    for batch_index in 0..8_u64 {
        let batch_started = Instant::now();
        let records = (0..8_192_u64)
            .map(|row_index| {
                let id = batch_index * 8_192 + row_index;
                let mut values = vec![String::new(); COLUMNS.len()];
                values[col_index("declaration_number").unwrap()] = format!("DECL-{id:010}");
                values[col_index("declaration_date").unwrap()] = "2024-01-15".to_string();
                values[col_index("sender").unwrap()] = format!("SENDER {}", id % 1_000);
                values[col_index("edrpou").unwrap()] = format!("{:08}", id % 100_000_000);
                values[col_index("recipient").unwrap()] = format!("RECIPIENT {}", id % 2_000);
                values[col_index("product_code").unwrap()] = format!("{:010}", id);
                values[col_index("description").unwrap()] =
                    format!("Representative benchmark product row {id}");
                values[col_index("origin_country").unwrap()] = "CN".to_string();
                values[col_index("net_kg").unwrap()] = "12.5".to_string();
                values[col_index("currency_control_value").unwrap()] = "1250.75".to_string();
                let hash = canonical_record_hash(&values, None);
                ImportRecord {
                    hash,
                    year: Some(2024),
                    values,
                    extra: None,
                }
            })
            .collect::<Vec<_>>();
        let (written, duplicates) = db.insert_batch("synthetic.xlsx", &records).unwrap();
        assert_eq!(duplicates, 0);
        assert_eq!(written, records.len() as u64);
        inserted += written;
        eprintln!(
            "SQLITE_INSERT_BENCH batch={} rows={} batch_ms={} cumulative_ms={} cumulative_rows_per_second={:.0}",
            batch_index + 1,
            written,
            batch_started.elapsed().as_millis(),
            started.elapsed().as_millis(),
            inserted as f64 / started.elapsed().as_secs_f64().max(f64::EPSILON),
        );
    }

    db.rollback_import_file();
    assert_eq!(db.total_rows(), 0);
}
