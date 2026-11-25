use base_search_core::{
    Error, Operation, Result, export,
    import::{self, ImportFileRequest, ImportSheetRequest},
    model::{QuerySpec, Role},
    query,
    workspace::Workspace,
};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let rows: u64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1000000".into())
        .parse()
        .map_err(|_| Error::Message("Pass the positive fixture row count".into()))?;
    if rows == 0 {
        return Err(Error::Message("Pass the positive fixture row count".into()));
    }
    let folder = tempfile::Builder::new()
        .prefix("base-search-scale-")
        .tempdir()?;
    let input = folder.path().join("fixture.csv");
    let database = folder.path().join("workspace.sqlite");
    let output = folder.path().join("export.csv");
    let expected_matches = rows.div_ceil(100);

    let generated = Instant::now();
    make_fixture(&input, rows)?;
    println!(
        "Fixture: {rows} rows, {:.1} MiB, generated in {:.2}s",
        std::fs::metadata(&input)?.len() as f64 / 1_048_576.0,
        generated.elapsed().as_secs_f64()
    );

    let workspace = Workspace::open(&database)?;
    let cancel = AtomicBool::new(false);
    let print_progress = |progress: base_search_core::Progress| {
        if progress.processed > 0 && progress.processed.is_multiple_of(1_000_000) {
            println!("  {}: {}", progress.message, progress.processed);
        }
    };
    let operation = Operation::new(&cancel, &print_progress);
    let preview = import::preview_file(&input)?;
    let read = Instant::now();
    let imported = import::import_files(
        &workspace,
        &[ImportFileRequest {
            path: input.to_string_lossy().into_owned(),
            sheets: vec![ImportSheetRequest {
                key: preview.sheets[0].key.clone(),
                headers: preview.sheets[0].headers.clone(),
                roles: vec![
                    Some(Role::Date),
                    Some(Role::Company),
                    Some(Role::Product),
                    None,
                    Some(Role::Value),
                    Some(Role::Currency),
                    Some(Role::Weight),
                    Some(Role::WeightUnit),
                ],
                fixed_currency: None,
                fixed_weight_unit: None,
            }],
        }],
        &operation,
    )?;
    if imported.inserted_rows != rows {
        return Err(Error::Message(format!(
            "Fixture import failed: {imported:?}"
        )));
    }
    println!(
        "Import with FTS index: {:.2}s",
        read.elapsed().as_secs_f64()
    );

    let open = Instant::now();
    let reopened = Workspace::open(&database)?;
    let workspace_info = reopened.info()?;
    let open_time = open.elapsed();
    if workspace_info.row_count != rows {
        return Err(Error::Message("Row count changed on reopening".into()));
    }
    println!("Reopen and read metadata: {:.2}ms", millis(open_time));
    let search = QuerySpec {
        text: "coffee".into(),
        revision: Some(workspace_info.revision),
        ..QuerySpec::default()
    };
    let mut times = Vec::new();
    for index in 0..23 {
        let began = Instant::now();
        let page = query::search(&reopened, &search, 0, 50)?;
        if page.total_rows != expected_matches || page.rows.len() as u64 != expected_matches.min(50)
        {
            return Err(Error::Message("Search mismatch on scale fixture".into()));
        }
        if index >= 3 {
            times.push(began.elapsed());
        }
    }
    times.sort_unstable();
    println!(
        "Indexed search ({} matches, 20 runs): median {:.2}ms, p95 {:.2}ms",
        expected_matches,
        millis(times[9]),
        millis(times[18])
    );

    let export = Instant::now();
    let exported = export::export(
        &reopened,
        &QuerySpec {
            revision: Some(workspace_info.revision),
            ..QuerySpec::default()
        },
        &output,
        None,
        &operation,
    )?;
    if exported != rows {
        return Err(Error::Message(
            "Export did not write the full fixture".into(),
        ));
    }
    println!(
        "Full CSV export: {:.2}s, {:.1} MiB",
        export.elapsed().as_secs_f64(),
        std::fs::metadata(&output)?.len() as f64 / 1_048_576.0
    );
    println!(
        "SQLite database: {:.1} MiB",
        std::fs::metadata(&database)?.len() as f64 / 1_048_576.0
    );
    Ok(())
}

fn make_fixture(path: &Path, rows: u64) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "Date",
        "Company",
        "Product code",
        "Description",
        "Value",
        "Currency",
        "Weight",
        "Weight unit",
    ])?;
    for index in 0..rows {
        let description = if index.is_multiple_of(100) {
            "Coffee shipment"
        } else {
            "Tea shipment"
        };
        writer.write_record([
            format!("2024-{:02}-{:02}", index % 12 + 1, index / 12 % 28 + 1),
            format!("Company {}", index % 256),
            format!("{:08}", index % 2048),
            description.to_owned(),
            (index % 1000 + 1).to_string(),
            if index.is_multiple_of(7) {
                "EUR".into()
            } else {
                "USD".into()
            },
            (index % 500 + 1).to_string(),
            "kg".into(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
