//! Command-line utility for database checks without the GUI:
//! import, search, export, and statistics.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use base_search::app::{fmt_money_exact, fmt_money_per_kg};
use base_search::db::{
    AnalyticsGroupRow, AnalyticsPriceMetric, AnalyticsSection, AnalyticsSectionKind,
    DatabaseStorageInfo, Db, Filters, PriceMetricKind, Query,
};
#[cfg(feature = "duckdb-olap")]
use base_search::duckdb_olap;
use base_search::export;
use base_search::i18n::Lang;
use base_search::import::{self, ImportPhase};
use base_search::olap::{OlapBenchmarkOptions, OlapBenchmarkReport, run_sqlite_benchmark};
use base_search::search::FieldInfo;

const USAGE: &str = "base-search-cli - diagnostics and automation for Base Search 2

Usage:
  base-search-cli stats  <db>
  base-search-cli compact <db> [--vacuum]
  base-search-cli peek   <table-file>
  base-search-cli import <db> <table-file> [...]
  base-search-cli search <db> [query...] [--limit N] [--year Y] [--code C]
                     [--sender S] [--recipient R] [--edrpou E]
                     [--trademark T] [--description D]
                     [--origin C] [--dispatch C] [--trade C]
                     [--repeat N] [--warmups N] [--no-print-rows] [--json]
  base-search-cli analytics <db> [query...] [--year Y] [--code C]
                       [--sender S] [--recipient R] [--edrpou E]
                       [--trademark T] [--description D]
                       [--origin C] [--dispatch C] [--trade C]
  base-search-cli benchmark <db> [query...] [--year Y] [--code C]
                       [--sender S] [--recipient R] [--edrpou E]
                       [--trademark T] [--description D]
                       [--origin C] [--dispatch C] [--trade C]
                       [--repeat N] [--warmups N] [--limit N]
                       [--section-limit N] [--hs-level 2|4|6|10]
                       [--pivot-rows N] [--pivot-cols N]
                       [--allow-empty] [--json]
  base-search-cli browser <db> [--host 127.0.0.1] [--port 7833] [--no-open]
                          [--confirm-wildcard-bind]
  base-search-cli user-add <db> <username> [--role owner|admin|editor|viewer]
                          [--password-stdin]
                          (default role: admin; password hidden unless automation is explicit)
  base-search-cli user-list <db>
  base-search-cli user-remove <db> <username>
  base-search-cli olap-build <db> [projection.duckdb]
  base-search-cli olap-benchmark <projection.duckdb> [query...] [--year Y]
                       [--origin C] [--repeat N] [--warmups N] [--json]
  base-search-cli export <db> <out.csv|out.xlsx> [query...]
  base-search-cli version

Import formats: .xlsx, .xlsb, .xls, .xlsm, .ods, .csv, .tsv
Account roles: owner, admin, editor, viewer";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("stats") if args.len() == 2 => cmd_stats(Path::new(&args[1])),
        Some("compact") if args.len() >= 2 => cmd_compact(Path::new(&args[1]), &args[2..]),
        Some("peek") if args.len() == 2 => cmd_peek(Path::new(&args[1])),
        Some("import") if args.len() >= 3 => cmd_import(Path::new(&args[1]), &args[2..]),
        Some("search") if args.len() >= 2 => cmd_search(Path::new(&args[1]), &args[2..]),
        Some("analytics") if args.len() >= 2 => cmd_analytics(Path::new(&args[1]), &args[2..]),
        Some("benchmark") if args.len() >= 2 => cmd_benchmark(Path::new(&args[1]), &args[2..]),
        Some("olap-build") if args.len() >= 2 => cmd_olap_build(Path::new(&args[1]), args.get(2)),
        Some("olap-benchmark") if args.len() >= 2 => {
            cmd_olap_benchmark(Path::new(&args[1]), &args[2..])
        }
        Some("export") if args.len() >= 3 => cmd_export(Path::new(&args[1]), &args[2], &args[3..]),
        Some("browser") if args.len() >= 2 => cmd_browser(Path::new(&args[1]), &args[2..]),
        Some("user-add") if args.len() >= 3 => cmd_user_add(Path::new(&args[1]), &args[2..]),
        Some("user-list") if args.len() == 2 => cmd_user_list(Path::new(&args[1])),
        Some("user-remove") if args.len() == 3 => cmd_user_remove(Path::new(&args[1]), &args[2]),
        Some("sql") if args.len() == 3 => cmd_sql(Path::new(&args[1]), &args[2]),
        Some("version") if args.len() == 1 => cmd_version(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reports the version and the optional features this binary was compiled
/// with, so a packaged artifact can be checked instead of assumed.
fn cmd_version() -> Result<(), String> {
    println!("{}", base_search::build_info::summary());
    Ok(())
}

fn cmd_stats(db_path: &Path) -> Result<(), String> {
    let db = Db::open(db_path)?;
    println!("Database: {}", db_path.display());
    println!("Rows: {}", db.total_rows());
    println!("Unindexed rows: {}", db.unindexed_rows());
    print_storage_info(&db.storage_info(db_path).map_err(|e| e.to_string())?);
    let log = db.import_log(20);
    if !log.is_empty() {
        println!("Recent imports:");
        for e in log {
            println!(
                "  {}  rows {}  imported {}  duplicates {}  {:.1}s  {}",
                e.file_name, e.total_rows, e.imported, e.duplicates, e.seconds, e.imported_at
            );
            if !e.quality.layout.is_empty() {
                println!(
                    "    quality: {} | header row {} | columns {} recognized {} extra {} | filled {:.0}%",
                    e.quality.layout,
                    e.quality.header_row,
                    e.quality.source_columns,
                    e.quality.recognized_columns,
                    e.quality.extra_columns,
                    e.quality.filled_percent()
                );
                for warning in &e.quality.warnings {
                    println!("    warning: {warning}");
                }
            }
        }
    }
    Ok(())
}

fn cmd_compact(db_path: &Path, args: &[String]) -> Result<(), String> {
    let vacuum = parse_compact_options(args)?;
    let db = Db::open(db_path)?;
    println!("Database: {}", db_path.display());
    println!("Before:");
    let before = db.storage_info(db_path).map_err(|e| e.to_string())?;
    print_storage_info(&before);

    let checkpoint = db.checkpoint_wal_truncate().map_err(|e| e.to_string())?;
    println!(
        "WAL checkpoint: busy {}, log frames {}, checkpointed {}",
        checkpoint.busy, checkpoint.log_frames, checkpoint.checkpointed_frames
    );

    if vacuum {
        println!("Running VACUUM. This can take a long time on large databases.");
        db.vacuum_database().map_err(|e| e.to_string())?;
    } else if before.freelist_bytes > 0 {
        println!(
            "SQLite free pages remain inside the main database file. Run with --vacuum to rewrite the file and return about {} to the filesystem.",
            format_bytes(before.freelist_bytes)
        );
    }

    println!("After:");
    print_storage_info(&db.storage_info(db_path).map_err(|e| e.to_string())?);
    Ok(())
}

fn parse_compact_options(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--vacuum" => Ok(true),
        _ => Err("Usage: base-search-cli compact <db> [--vacuum]".to_string()),
    }
}

fn print_storage_info(info: &DatabaseStorageInfo) {
    println!("Storage:");
    println!("  database file: {}", format_bytes(info.database_bytes));
    println!("  WAL file: {}", format_bytes(info.wal_bytes));
    println!("  SHM file: {}", format_bytes(info.shm_bytes));
    println!(
        "  SQLite free pages: {} ({})",
        info.freelist_pages,
        format_bytes(info.freelist_bytes)
    );
    println!("  total files: {}", format_bytes(info.total_file_bytes()));
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Diagnostic query: arbitrary SELECT, limited to 50 printed rows.
fn cmd_sql(db_path: &Path, sql: &str) -> Result<(), String> {
    let db = Db::open(db_path)?;
    for cells in db
        .diagnostic_query_rows(sql, 50)
        .map_err(|e| e.to_string())?
    {
        println!("{}", cells.join(" | "));
    }
    Ok(())
}

fn cmd_peek(path: &Path) -> Result<(), String> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names: Vec<String> = wb.sheet_names().to_vec();
    println!("Sheets: {names:?}");
    for (i, name) in names.iter().enumerate().take(3) {
        if let Some(Ok(range)) = wb.worksheet_range_at(i) {
            println!(
                "-- Sheet {i} \"{name}\": {} rows x {} columns",
                range.height(),
                range.width()
            );
            let mut rows = range.rows();
            let headers: Vec<String> = rows
                .next()
                .map(|r| r.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();
            let sample: Vec<String> = rows
                .next()
                .map(|r| {
                    r.iter()
                        .map(|d| d.to_string().chars().take(40).collect())
                        .collect()
                })
                .unwrap_or_default();
            for (c, h) in headers.iter().enumerate() {
                println!(
                    "   [{c:2}] {h} = {}",
                    sample.get(c).cloned().unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

fn cmd_import(db_path: &Path, files: &[String]) -> Result<(), String> {
    let mut db = Db::open(db_path)?;
    let cancel = AtomicBool::new(false);
    for file in files {
        let path = PathBuf::from(file);
        println!("== {}", path.display());
        let started = Instant::now();
        let mut last_phase: Option<ImportPhase> = None;
        let mut last_print = Instant::now();
        let summary = import::import_file(&mut db, &path, &cancel, &mut |phase, done, total| {
            let phase_changed = last_phase != Some(phase);
            if phase_changed || last_print.elapsed().as_secs() >= 2 {
                last_phase = Some(phase);
                last_print = Instant::now();
                let name = match phase {
                    ImportPhase::Reading => "reading",
                    ImportPhase::Inserting => "inserting",
                    ImportPhase::Indexing => "indexing",
                };
                if total > 0 {
                    println!("   {name}: {done} / {total}");
                } else {
                    println!("   {name}...");
                }
            }
        });
        match (&summary.error, &summary.skipped_duplicate_of) {
            (Some(e), _) => println!("   ERROR: {e}"),
            (None, Some(previous)) => {
                println!("   skipped: file was already imported (matches \"{previous}\")")
            }
            (None, None) => println!(
                "   done: rows {}, imported {}, duplicates {}, in {:.1}s ({:.0} rows/s)",
                summary.total_rows,
                summary.imported,
                summary.duplicates,
                started.elapsed().as_secs_f64(),
                summary.total_rows as f64 / started.elapsed().as_secs_f64().max(0.001)
            ),
        }
    }
    println!("Total rows in database: {}", db.total_rows());
    Ok(())
}

fn parse_query(args: &[String]) -> Result<(Query, u64), String> {
    parse_query_with_options(args, true)
}

fn parse_export_query(args: &[String]) -> Result<Query, String> {
    parse_query_with_options(args, false).map(|(query, _)| query)
}

fn parse_query_with_options(args: &[String], allow_limit: bool) -> Result<(Query, u64), String> {
    let mut q = Query::default();
    let mut limit = 10u64;
    let mut filters = Filters::default();
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize, flag: &str| -> Result<String, String> {
            *i += 1;
            match args.get(*i) {
                Some(value) if !value.starts_with("--") => Ok(value.clone()),
                _ => Err(format!("{flag} requires a value")),
            }
        };
        match args[i].as_str() {
            "--limit" if allow_limit => {
                let value = take(&mut i, "--limit")?;
                limit = value
                    .parse()
                    .map_err(|_| "--limit must be a positive integer".to_string())?;
                if limit == 0 {
                    return Err("--limit must be a positive integer".to_string());
                }
            }
            "--limit" => return Err("--limit is not supported by export".to_string()),
            "--year" => filters.year = take(&mut i, "--year")?,
            "--code" => filters.product_code = take(&mut i, "--code")?,
            "--sender" => filters.sender = take(&mut i, "--sender")?,
            "--recipient" => filters.recipient = take(&mut i, "--recipient")?,
            "--edrpou" => filters.edrpou = take(&mut i, "--edrpou")?,
            "--trademark" => filters.trademark = take(&mut i, "--trademark")?,
            "--description" => filters.description = take(&mut i, "--description")?,
            "--origin" => filters.origin_country = take(&mut i, "--origin")?,
            "--dispatch" => filters.dispatch_country = take(&mut i, "--dispatch")?,
            "--trade" => filters.trade_country = take(&mut i, "--trade")?,
            flag if flag.starts_with("--") => return Err(format!("Unknown query option: {flag}")),
            word => words.push(word.to_string()),
        }
        i += 1;
    }
    q.text = words.join(" ");
    q.filters = filters;
    Ok((q, limit))
}

#[derive(Debug, Clone)]
struct SearchOptions {
    repeat: usize,
    warmups: usize,
    print_rows: bool,
    json: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            repeat: 1,
            warmups: 0,
            print_rows: true,
            json: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BenchmarkCliOptions {
    olap: OlapBenchmarkOptions,
    json: bool,
    allow_empty: bool,
}

#[derive(Debug)]
struct SearchRun {
    count_ms: f64,
    page_ms: f64,
    total: u64,
    page_rows: usize,
    fields: Option<Vec<FieldInfo>>,
    rows: Option<Vec<Vec<String>>>,
}

fn parse_search_args(args: &[String]) -> Result<(Vec<String>, SearchOptions), String> {
    let mut query_args = Vec::new();
    let mut options = SearchOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repeat" => {
                i += 1;
                options.repeat = parse_positive_usize(args.get(i), "--repeat")?;
            }
            "--warmups" => {
                i += 1;
                options.warmups = parse_usize(args.get(i), "--warmups")?;
            }
            "--no-print-rows" => options.print_rows = false,
            "--json" => {
                options.json = true;
                options.print_rows = false;
            }
            arg => query_args.push(arg.to_string()),
        }
        i += 1;
    }
    Ok((query_args, options))
}

fn parse_usize(value: Option<&String>, flag: &str) -> Result<usize, String> {
    value
        .ok_or_else(|| format!("{flag} requires a value"))?
        .parse()
        .map_err(|_| format!("{flag} must be a non-negative integer"))
}

fn parse_positive_usize(value: Option<&String>, flag: &str) -> Result<usize, String> {
    let parsed = parse_usize(value, flag)?;
    if parsed == 0 {
        return Err(format!("{flag} must be at least 1"));
    }
    Ok(parsed)
}

/// Completes the search index after an interrupted import or migration.
fn ensure_indexed(db: &mut Db) -> Result<(), String> {
    if db.unindexed_rows() > 0 {
        eprintln!("Rebuilding search index...");
        let cancel = AtomicBool::new(false);
        db.index_fts(&cancel, |_, _| {})
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn cmd_search(db_path: &Path, args: &[String]) -> Result<(), String> {
    let mut db = Db::open(db_path)?;
    ensure_indexed(&mut db)?;
    let (query_args, options) = parse_search_args(args)?;
    let (q, limit) = parse_query(&query_args)?;
    for _ in 0..options.warmups {
        run_search_once(&db, &q, limit, false)?;
    }
    let mut runs = Vec::with_capacity(options.repeat);
    for idx in 0..options.repeat {
        let keep_rows = options.print_rows && options.repeat == 1 && idx == 0;
        runs.push(run_search_once(&db, &q, limit, keep_rows)?);
    }
    if options.json {
        print_search_json(&q, limit, &options, &runs);
        return Ok(());
    }
    if options.repeat == 1 {
        let run = &runs[0];
        println!(
            "Found: {} (count {:.3} ms, page {:.3} ms)",
            run.total, run.count_ms, run.page_ms
        );
    } else {
        for (idx, run) in runs.iter().enumerate() {
            println!(
                "Run {}: found {} (count {:.3} ms, page {:.3} ms, rows {})",
                idx + 1,
                run.total,
                run.count_ms,
                run.page_ms,
                run.page_rows
            );
        }
        print_search_summary(&runs);
    }
    if options.print_rows
        && options.repeat == 1
        && let Some(rows) = &runs[0].rows
    {
        print_search_rows(runs[0].fields.as_deref().unwrap_or(&[]), rows);
    }
    Ok(())
}

fn run_search_once(db: &Db, q: &Query, limit: u64, keep_rows: bool) -> Result<SearchRun, String> {
    let started = Instant::now();
    let total = db.count(q).map_err(|e| e.to_string())?;
    let count_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let (fields, _ids, rows, _dups) = db
        .search_page_dynamic(q, limit, 0)
        .map_err(|e| e.to_string())?;
    let page_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(SearchRun {
        count_ms,
        page_ms,
        total,
        page_rows: rows.len(),
        fields: keep_rows.then_some(fields),
        rows: keep_rows.then_some(rows),
    })
}

fn print_search_rows(fields: &[FieldInfo], rows: &[Vec<String>]) {
    for row in rows {
        let cells: Vec<String> = fields
            .iter()
            .zip(row)
            .filter(|(_, value)| !value.trim().is_empty())
            .take(8)
            .map(|(field, value)| format!("{}={}", field.label, trunc(value, 36)))
            .collect();
        println!("  {}", cells.join(" | "));
    }
}

fn print_search_summary(runs: &[SearchRun]) {
    println!(
        "Summary: count avg {:.3} ms min {:.3} max {:.3}; page avg {:.3} ms min {:.3} max {:.3}",
        avg_ms(runs, |run| run.count_ms),
        min_ms(runs, |run| run.count_ms),
        max_ms(runs, |run| run.count_ms),
        avg_ms(runs, |run| run.page_ms),
        min_ms(runs, |run| run.page_ms),
        max_ms(runs, |run| run.page_ms),
    );
}

fn print_search_json(q: &Query, limit: u64, options: &SearchOptions, runs: &[SearchRun]) {
    let total = runs.first().map(|run| run.total).unwrap_or(0);
    let page_rows = runs.first().map(|run| run.page_rows).unwrap_or(0);
    print!(
        "{{\"query\":\"{}\",\"limit\":{},\"repeat\":{},\"warmups\":{},\"total\":{},\"page_rows\":{},",
        json_escape(&q.text),
        limit,
        options.repeat,
        options.warmups,
        total,
        page_rows
    );
    print!(
        "\"count_ms\":{{\"avg\":{:.3},\"min\":{:.3},\"max\":{:.3}}},",
        avg_ms(runs, |run| run.count_ms),
        min_ms(runs, |run| run.count_ms),
        max_ms(runs, |run| run.count_ms),
    );
    print!(
        "\"page_ms\":{{\"avg\":{:.3},\"min\":{:.3},\"max\":{:.3}}},\"runs\":[",
        avg_ms(runs, |run| run.page_ms),
        min_ms(runs, |run| run.page_ms),
        max_ms(runs, |run| run.page_ms),
    );
    for (idx, run) in runs.iter().enumerate() {
        if idx > 0 {
            print!(",");
        }
        print!(
            "{{\"count_ms\":{:.3},\"page_ms\":{:.3},\"total\":{},\"page_rows\":{}}}",
            run.count_ms, run.page_ms, run.total, run.page_rows
        );
    }
    println!("]}}");
}

fn avg_ms(runs: &[SearchRun], value: impl Fn(&SearchRun) -> f64) -> f64 {
    if runs.is_empty() {
        return 0.0;
    }
    runs.iter().map(value).sum::<f64>() / runs.len() as f64
}

fn min_ms(runs: &[SearchRun], value: impl Fn(&SearchRun) -> f64) -> f64 {
    runs.iter().map(value).fold(f64::INFINITY, f64::min)
}

fn max_ms(runs: &[SearchRun], value: impl Fn(&SearchRun) -> f64) -> f64 {
    runs.iter().map(value).fold(0.0, f64::max)
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            ch => vec![ch],
        })
        .collect()
}

fn cmd_analytics(db_path: &Path, args: &[String]) -> Result<(), String> {
    let mut db = Db::open(db_path)?;
    ensure_indexed(&mut db)?;
    let (q, _) = parse_query(args)?;
    let started = Instant::now();
    let analytics = db.analytics(&q, 10).map_err(|e| e.to_string())?;
    println!(
        "Rows: {}  document IDs: {}  companies: {}  sources: {}  company IDs: {}",
        analytics.overview.row_count,
        analytics.overview.declaration_count,
        analytics.overview.distinct_recipients,
        analytics.overview.distinct_senders,
        analytics.overview.distinct_edrpou
    );
    println!(
        "Value: {}  net: {:.3} kg  gross: {:.3} kg  value/kg: {}  quantity: {:.3}",
        fmt_money_exact(&analytics.overview.measures, Lang::En),
        analytics.overview.total_net_kg,
        analytics.overview.total_gross_kg,
        fmt_money_per_kg(&analytics.overview.measures, Lang::En),
        analytics.overview.total_quantity
    );
    println!(
        "Product codes: {}  trademarks: {}  origin countries: {}  dispatch countries: {}  trade countries: {}",
        analytics.overview.distinct_product_codes,
        analytics.overview.distinct_trademarks,
        analytics.overview.distinct_origin_countries,
        analytics.overview.distinct_dispatch_countries,
        analytics.overview.distinct_trade_countries
    );
    print_sections("Companies", &analytics.company_sections);
    print_sections("Goods", &analytics.product_sections);
    print_sections("Countries", &analytics.country_sections);
    print_prices(&analytics.price_sections);
    println!("Done in {} ms", started.elapsed().as_millis());
    Ok(())
}

fn cmd_benchmark(db_path: &Path, args: &[String]) -> Result<(), String> {
    let mut db = Db::open(db_path)?;
    ensure_indexed(&mut db)?;
    let (query_args, mut options) = parse_benchmark_args(args)?;
    let limit_was_set = query_args.iter().any(|arg| arg == "--limit");
    let (query, limit) = parse_query(&query_args)?;
    if limit_was_set {
        options.olap.page_limit = limit;
    }
    if query.is_empty() && !options.allow_empty {
        return Err(
            "Benchmark requires a query or filter. Use --allow-empty for a full-database baseline."
                .to_string(),
        );
    }

    let report = run_sqlite_benchmark(&db, &query, &options.olap)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?
        );
    } else {
        print_benchmark_report(&report);
    }
    Ok(())
}

#[cfg(feature = "duckdb-olap")]
fn cmd_olap_build(db_path: &Path, projection_arg: Option<&String>) -> Result<(), String> {
    let projection_path = projection_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| duckdb_olap::default_projection_path(db_path));
    let build = duckdb_olap::build_projection_atomic(db_path, &projection_path)?;
    println!(
        "Built DuckDB projection: {}",
        build.projection_path.display()
    );
    println!(
        "Rows: {}  time: {:.1}s",
        build.rows,
        build.elapsed_ms / 1000.0
    );
    Ok(())
}

#[cfg(not(feature = "duckdb-olap"))]
fn cmd_olap_build(_db_path: &Path, _projection_arg: Option<&String>) -> Result<(), String> {
    Err(
        "DuckDB OLAP support is not enabled in this binary. Rebuild with --features duckdb-olap."
            .to_string(),
    )
}

#[cfg(feature = "duckdb-olap")]
fn cmd_olap_benchmark(projection_path: &Path, args: &[String]) -> Result<(), String> {
    let (query_args, options) = parse_benchmark_args(args)?;
    let (query, _) = parse_query(&query_args)?;
    if query.is_empty() && !options.allow_empty {
        return Err(
            "OLAP benchmark requires a query or filter. Use --allow-empty for a full-projection baseline."
                .to_string(),
        );
    }
    let report = duckdb_olap::run_duckdb_benchmark(projection_path, &query, &options.olap)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?
        );
    } else {
        print_benchmark_report(&report);
    }
    Ok(())
}

#[cfg(not(feature = "duckdb-olap"))]
fn cmd_olap_benchmark(_projection_path: &Path, _args: &[String]) -> Result<(), String> {
    Err(
        "DuckDB OLAP support is not enabled in this binary. Rebuild with --features duckdb-olap."
            .to_string(),
    )
}

fn parse_benchmark_args(args: &[String]) -> Result<(Vec<String>, BenchmarkCliOptions), String> {
    let mut query_args = Vec::new();
    let mut options = BenchmarkCliOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repeat" => {
                i += 1;
                options.olap.repeat = parse_positive_usize(args.get(i), "--repeat")?;
            }
            "--warmups" => {
                i += 1;
                options.olap.warmups = parse_usize(args.get(i), "--warmups")?;
            }
            "--section-limit" => {
                i += 1;
                options.olap.section_limit = parse_positive_u64(args.get(i), "--section-limit")?;
            }
            "--hs-level" => {
                i += 1;
                options.olap.hs_level = parse_hs_level(args.get(i))?;
            }
            "--pivot-rows" => {
                i += 1;
                options.olap.pivot_rows = parse_positive_usize(args.get(i), "--pivot-rows")?;
            }
            "--pivot-cols" => {
                i += 1;
                options.olap.pivot_cols = parse_positive_usize(args.get(i), "--pivot-cols")?;
            }
            "--allow-empty" => options.allow_empty = true,
            "--json" => options.json = true,
            arg => query_args.push(arg.to_string()),
        }
        i += 1;
    }
    options.olap.page_limit = options.olap.page_limit.clamp(1, 500);
    options.olap.section_limit = options.olap.section_limit.clamp(1, 200);
    options.olap.pivot_rows = options.olap.pivot_rows.clamp(1, 100);
    options.olap.pivot_cols = options.olap.pivot_cols.clamp(1, 100);
    Ok((query_args, options))
}

fn parse_positive_u64(value: Option<&String>, flag: &str) -> Result<u64, String> {
    let parsed = value
        .ok_or_else(|| format!("{flag} requires a value"))?
        .parse()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{flag} must be at least 1"));
    }
    Ok(parsed)
}

fn parse_hs_level(value: Option<&String>) -> Result<u8, String> {
    let parsed: u8 = value
        .ok_or_else(|| "--hs-level requires a value".to_string())?
        .parse()
        .map_err(|_| "--hs-level must be one of 2, 4, 6, 10".to_string())?;
    match parsed {
        2 | 4 | 6 | 10 => Ok(parsed),
        _ => Err("--hs-level must be one of 2, 4, 6, 10".to_string()),
    }
}

fn print_benchmark_report(report: &OlapBenchmarkReport) {
    println!("Backend: {}", report.backend);
    println!("Database rows: {}", report.total_database_rows);
    println!("Unindexed rows: {}", report.unindexed_rows);
    println!("Query: {}", query_label(&report.query));
    if report.query_is_empty {
        println!("Scope: full database");
    }
    println!();
    println!(
        "{:<28} {:<8} {:>10} {:>10} {:>10} {:>10}",
        "Scenario", "Category", "Avg ms", "Min ms", "Max ms", "Output"
    );
    for scenario in &report.scenarios {
        println!(
            "{:<28} {:<8} {:>10.3} {:>10.3} {:>10.3} {:>10}",
            scenario.name,
            scenario.category,
            scenario.average_ms,
            scenario.minimum_ms,
            scenario.maximum_ms,
            scenario.output_rows
        );
    }
}

fn query_label(query: &Query) -> String {
    let mut parts = Vec::new();
    if !query.text.trim().is_empty() {
        parts.push(format!("text={}", query.text.trim()));
    }
    let filters = &query.filters;
    for (name, value) in [
        ("year", filters.year.as_str()),
        ("code", filters.product_code.as_str()),
        ("sender", filters.sender.as_str()),
        ("recipient", filters.recipient.as_str()),
        ("edrpou", filters.edrpou.as_str()),
        ("trademark", filters.trademark.as_str()),
        ("description", filters.description.as_str()),
        ("origin", filters.origin_country.as_str()),
        ("dispatch", filters.dispatch_country.as_str()),
        ("trade", filters.trade_country.as_str()),
    ] {
        if !value.trim().is_empty() {
            parts.push(format!("{name}={}", value.trim()));
        }
    }
    if parts.is_empty() {
        "<empty>".to_string()
    } else {
        parts.join(", ")
    }
}

fn print_sections(group: &str, sections: &[AnalyticsSection]) {
    for section in sections {
        if section.rows.is_empty() {
            continue;
        }
        println!("\n{group} / {}:", analytics_section_title(section.kind));
        print_group(&section.rows);
    }
}

fn print_group(rows: &[AnalyticsGroupRow]) {
    if rows.is_empty() {
        return;
    }
    for row in rows {
        println!(
            "  {} | rows {} | decl {} | companies {} | share {:.1}% | value {} | net {:.3} kg | value/kg {}",
            row.label,
            row.rows,
            row.declarations,
            row.companies,
            row.share_percent,
            fmt_money_exact(&row.measures, Lang::En),
            row.total_net_kg,
            fmt_money_per_kg(&row.measures, Lang::En)
        );
    }
}

fn print_prices(metrics: &[AnalyticsPriceMetric]) {
    if metrics.is_empty() {
        return;
    }
    println!("\nPrices:");
    for metric in metrics {
        if metric.count == 0 {
            continue;
        }
        println!(
            "  {} | values {} | avg {:.4} | weighted {:.4} | min {:.4} | max {:.4}",
            price_metric_title(metric.kind),
            metric.count,
            metric.average,
            metric.weighted_average,
            metric.minimum,
            metric.maximum
        );
    }
}

fn analytics_section_title(kind: AnalyticsSectionKind) -> &'static str {
    match kind {
        AnalyticsSectionKind::Recipients => "Recipients / who received",
        AnalyticsSectionKind::Senders => "Senders",
        AnalyticsSectionKind::Edrpou => "EDRPOU",
        AnalyticsSectionKind::ProductCodes => "Product codes",
        AnalyticsSectionKind::Trademarks => "Trademarks",
        AnalyticsSectionKind::ProductGroups => "Description groups",
        AnalyticsSectionKind::OriginCountries => "Origin countries",
        AnalyticsSectionKind::DispatchCountries => "Dispatch countries",
        AnalyticsSectionKind::TradeCountries => "Trade countries",
    }
}

fn price_metric_title(kind: PriceMetricKind) -> &'static str {
    match kind {
        PriceMetricKind::ValuePerNetKg => "Value / net kg",
        PriceMetricKind::RfvUsdKg => "RFV $/kg",
        PriceMetricKind::RmvNetUsdKg => "RMV net $/kg",
        PriceMetricKind::RmvUsdExtraUnit => "RMV extra unit",
        PriceMetricKind::RmvGrossUsdKg => "RMV gross $/kg",
        PriceMetricKind::MinBaseUsdKg => "Minimum base $/kg",
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn cmd_export(db_path: &Path, out: &str, args: &[String]) -> Result<(), String> {
    let mut db = Db::open(db_path)?;
    ensure_indexed(&mut db)?;
    let q = parse_export_query(args)?;
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut last_print = Instant::now();
    let written = export::export(&db, &q, Path::new(out), &cancel, |done, total| {
        if last_print.elapsed().as_secs() >= 2 {
            last_print = Instant::now();
            println!("  {done} / {total}");
        }
    })
    .map_err(export_error_message)?;
    println!(
        "Exported {written} rows to {out} in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn export_error_message(err: export::ExportError) -> String {
    match err {
        export::ExportError::TooManyRowsForXlsx(rows) => {
            format!("{rows} rows exceed the XLSX row limit; export CSV instead")
        }
        export::ExportError::UnsupportedExtension(ext) if ext.is_empty() => {
            "Unsupported export extension. Use .csv or .xlsx.".to_string()
        }
        export::ExportError::UnsupportedExtension(ext) => {
            format!("Unsupported export extension: .{ext}. Use .csv or .xlsx.")
        }
        export::ExportError::Cancelled => "Export cancelled".to_string(),
        export::ExportError::Other(message) => message,
    }
}

/// Starts the local browser workspace on 127.0.0.1 (or an explicit host).
#[cfg(feature = "browser")]
fn cmd_browser(db_path: &Path, args: &[String]) -> Result<(), String> {
    use base_search::server::{self, ServerConfig};

    let mut config = ServerConfig::local(db_path.to_path_buf());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                let value = args.get(i).ok_or("--host requires a value")?;
                config.host = value
                    .parse::<std::net::IpAddr>()
                    .map_err(|_| format!("invalid host address: {value}"))?;
            }
            "--port" => {
                i += 1;
                let value = args.get(i).ok_or("--port requires a value")?;
                config.port = value
                    .parse::<u16>()
                    .map_err(|_| "--port must be a number between 1 and 65535".to_string())?;
                if config.port == 0 {
                    return Err("--port must be a number between 1 and 65535".to_string());
                }
            }
            "--no-open" => config.open_browser = false,
            "--confirm-wildcard-bind" => config.confirm_wildcard_bind(),
            other => return Err(format!("Unknown browser option: {other}")),
        }
        i += 1;
    }
    config.validate_bind_policy()?;
    server::run(config)
}

#[cfg(not(feature = "browser"))]
fn cmd_browser(_db_path: &Path, _args: &[String]) -> Result<(), String> {
    Err(
        "This build was compiled without the browser feature. Rebuild with --features browser."
            .to_string(),
    )
}

#[cfg(feature = "browser")]
struct UserAddOptions {
    username: String,
    role: String,
    password_stdin: bool,
}

#[cfg(feature = "browser")]
fn parse_user_add_options(args: &[String]) -> Result<UserAddOptions, String> {
    let username = args.first().ok_or("user-add requires a username")?.clone();
    let mut role = "admin".to_string();
    let mut password_stdin = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--role" => {
                i += 1;
                role = args.get(i).ok_or("--role requires a value")?.clone();
            }
            "--password-stdin" => password_stdin = true,
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 1;
    }
    role = role.trim().to_ascii_lowercase();
    if !matches!(role.as_str(), "owner" | "admin" | "editor" | "viewer") {
        return Err("--role must be owner, admin, editor, or viewer".to_string());
    }
    Ok(UserAddOptions {
        username,
        role,
        password_stdin,
    })
}

#[cfg(feature = "browser")]
fn read_password_from_stdin() -> Result<String, String> {
    let mut password = String::new();
    std::io::stdin()
        .read_line(&mut password)
        .map_err(|err| err.to_string())?;
    Ok(password.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(feature = "browser")]
fn read_hidden_password(username: &str) -> Result<String, String> {
    let password = rpassword::prompt_password(format!("Password for '{username}': "))
        .map_err(|err| format!("could not read password from the terminal: {err}"))?;
    let confirmation = rpassword::prompt_password("Confirm password: ")
        .map_err(|err| format!("could not read password confirmation: {err}"))?;
    if password != confirmation {
        return Err("Passwords do not match.".to_string());
    }
    Ok(password)
}

/// Creates or updates a local account. Interactive input is hidden; pipelines
/// must opt in explicitly with `--password-stdin`.
#[cfg(feature = "browser")]
fn cmd_user_add(db_path: &Path, args: &[String]) -> Result<(), String> {
    let options = parse_user_add_options(args)?;
    let accounts_before = base_search::server::list_accounts(db_path)?;
    if accounts_before.is_empty() && options.role != "owner" {
        return Err("The first account must explicitly use --role owner.".to_string());
    }
    let existed = accounts_before
        .iter()
        .any(|(username, _, _)| username.eq_ignore_ascii_case(&options.username));
    let password = if options.password_stdin {
        read_password_from_stdin()?
    } else {
        read_hidden_password(&options.username)?
    };
    base_search::server::add_account(db_path, &options.username, &password, &options.role)?;
    let effective_role = base_search::server::list_accounts(db_path)?
        .into_iter()
        .find(|(username, _, _)| username.eq_ignore_ascii_case(&options.username))
        .map(|(_, role, _)| role)
        .ok_or("Account was saved but could not be read back.")?;
    let action = if existed { "updated" } else { "created" };
    println!(
        "Account '{}' {action} with effective role '{effective_role}'.",
        options.username
    );
    Ok(())
}

#[cfg(feature = "browser")]
fn cmd_user_list(db_path: &Path) -> Result<(), String> {
    let accounts = base_search::server::list_accounts(db_path)?;
    if accounts.is_empty() {
        println!("No accounts.");
    }
    for (username, role, created_at) in accounts {
        println!("{username}\t{role}\t{created_at}");
    }
    Ok(())
}

#[cfg(feature = "browser")]
fn cmd_user_remove(db_path: &Path, username: &str) -> Result<(), String> {
    if base_search::server::remove_account(db_path, username)? {
        println!("Removed account '{username}'.");
    } else {
        println!("No account named '{username}'.");
    }
    Ok(())
}

#[cfg(not(feature = "browser"))]
fn cmd_user_add(_db_path: &Path, _args: &[String]) -> Result<(), String> {
    Err("Account management requires the browser feature.".to_string())
}

#[cfg(not(feature = "browser"))]
fn cmd_user_list(_db_path: &Path) -> Result<(), String> {
    Err("Account management requires the browser feature.".to_string())
}

#[cfg(not(feature = "browser"))]
fn cmd_user_remove(_db_path: &Path, _username: &str) -> Result<(), String> {
    Err("Account management requires the browser feature.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parse_query_rejects_missing_filter_values() {
        assert!(parse_query(&args(&["--year"])).is_err());
        assert!(parse_query(&args(&["--code"])).is_err());
        assert!(parse_query(&args(&["--origin"])).is_err());
        assert!(parse_query(&args(&["--dispatch"])).is_err());
        assert!(parse_query(&args(&["--trade"])).is_err());
    }

    #[test]
    fn parse_query_rejects_invalid_limit() {
        assert!(parse_query(&args(&["--limit", "nope"])).is_err());
    }

    #[test]
    fn parse_query_rejects_unknown_flags() {
        assert!(parse_query(&args(&["--unknown"])).is_err());
    }

    #[test]
    fn parse_query_accepts_country_filters() {
        let (query, _) = parse_query(&args(&[
            "Widget",
            "--origin",
            "CN",
            "--dispatch",
            "PL",
            "--trade",
            "DE",
        ]))
        .unwrap();
        assert_eq!(query.text, "Widget");
        assert_eq!(query.filters.origin_country, "CN");
        assert_eq!(query.filters.dispatch_country, "PL");
        assert_eq!(query.filters.trade_country, "DE");
    }

    #[test]
    fn parse_export_query_rejects_limit() {
        assert!(parse_export_query(&args(&["--limit", "10"])).is_err());
    }

    #[test]
    fn parse_compact_options_accepts_only_vacuum_flag() {
        assert!(!parse_compact_options(&[]).unwrap());
        assert!(parse_compact_options(&args(&["--vacuum"])).unwrap());
        assert!(parse_compact_options(&args(&["--unknown"])).is_err());
        assert!(parse_compact_options(&args(&["--vacuum", "--again"])).is_err());
    }

    #[test]
    fn parse_benchmark_options_separates_runtime_and_query_args() {
        let (query_args, options) = parse_benchmark_args(&args(&[
            "Widget",
            "--year",
            "2024",
            "--repeat",
            "5",
            "--warmups",
            "2",
            "--section-limit",
            "25",
            "--hs-level",
            "4",
            "--pivot-rows",
            "30",
            "--pivot-cols",
            "8",
            "--allow-empty",
            "--json",
        ]))
        .unwrap();
        assert_eq!(query_args, args(&["Widget", "--year", "2024"]));
        assert_eq!(options.olap.repeat, 5);
        assert_eq!(options.olap.warmups, 2);
        assert_eq!(options.olap.section_limit, 25);
        assert_eq!(options.olap.hs_level, 4);
        assert_eq!(options.olap.pivot_rows, 30);
        assert_eq!(options.olap.pivot_cols, 8);
        assert!(options.allow_empty);
        assert!(options.json);
    }

    #[test]
    fn parse_benchmark_options_rejects_invalid_hs_level() {
        assert!(parse_benchmark_args(&args(&["--hs-level", "3"])).is_err());
        assert!(parse_benchmark_args(&args(&["--hs-level"])).is_err());
    }

    #[cfg(feature = "browser")]
    #[test]
    fn user_add_password_stdin_is_an_explicit_automation_option() {
        let options =
            parse_user_add_options(&args(&["alice", "--role", "owner", "--password-stdin"]))
                .unwrap();
        assert_eq!(options.username, "alice");
        assert_eq!(options.role, "owner");
        assert!(options.password_stdin);

        assert!(parse_user_add_options(&args(&["alice", "--unknown"])).is_err());
    }
}
