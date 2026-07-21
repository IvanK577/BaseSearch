//! Export search results to CSV (UTF-8 BOM, ';') and streaming XLSX.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::{Db, Query, ResultSort};
use crate::search::FieldInfo;

/// Excel worksheet row limit minus the header row.
pub const XLSX_MAX_ROWS: u64 = 1_048_575;
const BATCH: u64 = 4096;
pub const MAX_EXPORT_FIELDS: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
    Csv,
    Xlsx,
}

impl ExportFormat {
    pub fn from_path(path: &Path) -> Result<ExportFormat, ExportError> {
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("csv") => Ok(ExportFormat::Csv),
            Some("xlsx") => Ok(ExportFormat::Xlsx),
            other => Err(ExportError::UnsupportedExtension(
                other.unwrap_or("").to_string(),
            )),
        }
    }
}

#[derive(Debug)]
pub enum ExportError {
    /// More rows than a single Excel worksheet can store; CSV is required.
    TooManyRowsForXlsx(u64),
    UnsupportedExtension(String),
    Cancelled,
    Other(String),
}

#[derive(Debug)]
pub enum ExportSelectionError {
    EmptyFieldSelection,
    TooManyFields { requested: usize, maximum: usize },
    DuplicateField(String),
    UnknownField(String),
    UnknownSortField(String),
}

/// Exports all rows matching the query and returns the row count.
pub fn export(
    db: &Db,
    q: &Query,
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<u64, ExportError> {
    let fields = db.result_fields_cached();
    export_selected(db, q, dest, &fields, None, cancel, progress)
}

pub fn resolve_fields(
    catalog: &[FieldInfo],
    field_ids: Option<&[String]>,
) -> Result<Vec<FieldInfo>, ExportSelectionError> {
    let ids = match field_ids {
        Some(ids) => ids,
        None => {
            if catalog.is_empty() {
                return Err(ExportSelectionError::EmptyFieldSelection);
            }
            return Ok(catalog.to_vec());
        }
    };
    if ids.is_empty() {
        return Err(ExportSelectionError::EmptyFieldSelection);
    }
    if ids.len() > MAX_EXPORT_FIELDS {
        return Err(ExportSelectionError::TooManyFields {
            requested: ids.len(),
            maximum: MAX_EXPORT_FIELDS,
        });
    }

    let by_id: HashMap<&str, &FieldInfo> = catalog
        .iter()
        .map(|field| (field.id.as_str(), field))
        .collect();
    let mut seen = HashSet::with_capacity(ids.len());
    let mut fields = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err(ExportSelectionError::DuplicateField(id.clone()));
        }
        let field = by_id
            .get(id.as_str())
            .ok_or_else(|| ExportSelectionError::UnknownField(id.clone()))?;
        fields.push((*field).clone());
    }
    Ok(fields)
}

pub fn validate_sort(
    catalog: &[FieldInfo],
    sort: Option<&ResultSort>,
) -> Result<(), ExportSelectionError> {
    if let Some(sort) = sort
        && !catalog.iter().any(|field| field.id == sort.field)
    {
        return Err(ExportSelectionError::UnknownSortField(sort.field.clone()));
    }
    Ok(())
}

pub fn export_selected(
    db: &Db,
    q: &Query,
    dest: &Path,
    fields: &[FieldInfo],
    sort: Option<&ResultSort>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<u64, ExportError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    if fields.is_empty() {
        return Err(ExportError::Other(
            "Select at least one export field.".to_string(),
        ));
    }
    // Query-aware catalog: schema-exact field ids are sortable when the query
    // addresses registered source fields directly.
    let catalog = db
        .result_fields_for_query(q)
        .map_err(|e| ExportError::Other(e.to_string()))?;
    validate_sort(&catalog, sort).map_err(|error| ExportError::Other(error.to_string()))?;
    let total = db.count(q).map_err(|e| ExportError::Other(e.to_string()))?;
    let format = ExportFormat::from_path(dest)?;
    if format == ExportFormat::Xlsx && total > XLSX_MAX_ROWS {
        return Err(ExportError::TooManyRowsForXlsx(total));
    }
    let temp_dest = temp_export_path(dest);
    let context = ExportContext {
        db,
        query: q,
        total,
        fields,
        sort,
        cancel,
    };
    let result = match format {
        ExportFormat::Csv => export_csv(&context, &temp_dest, &mut progress),
        ExportFormat::Xlsx => export_xlsx(&context, &temp_dest, &mut progress),
    };
    match result {
        Ok(written) => {
            if dest.exists() {
                std::fs::remove_file(dest).map_err(|e| ExportError::Other(e.to_string()))?;
            }
            std::fs::rename(&temp_dest, dest).map_err(|e| ExportError::Other(e.to_string()))?;
            Ok(written)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp_dest);
            Err(err)
        }
    }
}

impl std::fmt::Display for ExportSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportSelectionError::EmptyFieldSelection => {
                formatter.write_str("Select at least one export field.")
            }
            ExportSelectionError::TooManyFields { requested, maximum } => write!(
                formatter,
                "Selected {requested} export fields; the maximum is {maximum}."
            ),
            ExportSelectionError::DuplicateField(field) => {
                write!(
                    formatter,
                    "Export field '{field}' was selected more than once."
                )
            }
            ExportSelectionError::UnknownField(field) => {
                write!(formatter, "Unknown export field: '{field}'.")
            }
            ExportSelectionError::UnknownSortField(field) => {
                write!(formatter, "Unknown export sort field: '{field}'.")
            }
        }
    }
}

struct ExportContext<'a> {
    db: &'a Db,
    query: &'a Query,
    total: u64,
    fields: &'a [FieldInfo],
    sort: Option<&'a ResultSort>,
    cancel: &'a AtomicBool,
}

fn export_csv(
    context: &ExportContext<'_>,
    dest: &Path,
    progress: &mut impl FnMut(u64, u64),
) -> Result<u64, ExportError> {
    let mut file = std::fs::File::create(dest).map_err(|e| ExportError::Other(e.to_string()))?;
    // BOM makes Excel open Cyrillic text as UTF-8; ';' is friendlier for
    // locales that use a decimal comma.
    file.write_all(b"\xEF\xBB\xBF")
        .map_err(|e| ExportError::Other(e.to_string()))?;
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .terminator(csv::Terminator::CRLF)
        .from_writer(std::io::BufWriter::new(file));
    let headers: Vec<String> = context
        .fields
        .iter()
        .map(|field| csv_safe_cell(&field.label).into_owned())
        .collect();
    writer
        .write_record(headers)
        .map_err(|e| ExportError::Other(e.to_string()))?;

    let mut failure = None;
    let mut written = 0_u64;
    let sort_catalog = context
        .db
        .result_fields_for_query(context.query)
        .map_err(|e| ExportError::Other(e.to_string()))?;
    context
        .db
        .visit_export_rows_fields(
            context.query,
            context.fields,
            &sort_catalog,
            context.sort,
            |row| {
                if context.cancel.load(Ordering::Relaxed) {
                    failure = Some(ExportError::Cancelled);
                    return false;
                }
                let safe_row: Vec<String> = row
                    .iter()
                    .map(|value| csv_safe_cell(value).into_owned())
                    .collect();
                if let Err(err) = writer.write_record(&safe_row) {
                    failure = Some(ExportError::Other(err.to_string()));
                    return false;
                }
                written += 1;
                if written.is_multiple_of(BATCH) {
                    progress(written, context.total);
                }
                true
            },
        )
        .map_err(|e| ExportError::Other(e.to_string()))?;
    if let Some(error) = failure {
        return Err(error);
    }
    if context.cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    progress(written, context.total);
    writer
        .flush()
        .map_err(|e| ExportError::Other(e.to_string()))?;
    Ok(written)
}

pub fn csv_safe_cell(value: &str) -> Cow<'_, str> {
    let trimmed = value.trim_start_matches([' ', '\t', '\r', '\n']);
    if trimmed
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'=' | b'+' | b'-' | b'@'))
    {
        Cow::Owned(format!("'{value}"))
    } else {
        Cow::Borrowed(value)
    }
}

fn temp_export_path(dest: &Path) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "base-search-export".into());
    dest.with_file_name(format!("{file_name}.{pid}.{stamp}.tmp"))
}

fn export_xlsx(
    context: &ExportContext<'_>,
    dest: &Path,
    progress: &mut impl FnMut(u64, u64),
) -> Result<u64, ExportError> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let worksheet = workbook.add_worksheet_with_constant_memory();
    for (col, field) in context.fields.iter().enumerate() {
        worksheet
            .write_string(0, col as u16, &field.label)
            .map_err(|e| ExportError::Other(e.to_string()))?;
    }
    let mut failure = None;
    let mut written = 0_u64;
    let sort_catalog = context
        .db
        .result_fields_for_query(context.query)
        .map_err(|e| ExportError::Other(e.to_string()))?;
    context
        .db
        .visit_export_rows_fields(
            context.query,
            context.fields,
            &sort_catalog,
            context.sort,
            |row| {
                if context.cancel.load(Ordering::Relaxed) {
                    failure = Some(ExportError::Cancelled);
                    return false;
                }
                let output_row = written + 1;
                for (col, value) in row.iter().enumerate() {
                    if !value.is_empty()
                        && let Err(err) =
                            worksheet.write_string(output_row as u32, col as u16, value)
                    {
                        failure = Some(ExportError::Other(err.to_string()));
                        return false;
                    }
                }
                written += 1;
                if written.is_multiple_of(BATCH) {
                    progress(written, context.total);
                }
                true
            },
        )
        .map_err(|e| ExportError::Other(e.to_string()))?;
    if let Some(error) = failure {
        return Err(error);
    }
    if context.cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    progress(written, context.total);
    workbook
        .save(dest)
        .map_err(|e| ExportError::Other(e.to_string()))?;
    Ok(written)
}
