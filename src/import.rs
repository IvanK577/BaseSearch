//! Imports Excel files (.xlsx and .xlsb) into the database through calamine.
//!
//! Files are read as a cell stream, so import uses very little memory even for
//! files that are hundreds of megabytes. Before parsing, the file content hash
//! is calculated so repeat imports of the same file can be skipped quickly.
//!
//! Supported input:
//! - any spreadsheet-like table with a detectable header row;
//! - optional semantic profiles that recognize common business fields and
//!   improve analytics without deciding whether a file is importable;
//! - repeated/noisy headers and additional source columns, which are preserved
//!   as dynamic data instead of being dropped.

mod aliases;
mod detection;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use calamine::{Data, Reader, Sheets, open_workbook_auto};
use chrono::Timelike;
use xxhash_rust::xxh3::Xxh3;

use crate::db::{
    Db, ImportLogWrite, ImportQuality, ImportRecord, canonical_record_hash, extract_year,
};
use crate::domain::table::{ColumnRole, ColumnStorage, SemanticField, TableShape};
use crate::schema::{COLUMNS, DATE_COL};

use self::detection::{DETECTION_BUFFER_ROWS, DetectedTable, detect_table};

const BATCH_SIZE: usize = 8192;
/// Number of first sheet rows scanned while searching for the header row.
const HEADER_SCAN_ROWS: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportPhase {
    /// File reading and parsing.
    Reading,
    /// Writing rows to the database.
    Inserting,
    /// Full-text index construction.
    Indexing,
}

#[derive(Clone, Debug, Default)]
pub struct FileSummary {
    pub file_name: String,
    pub total_rows: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub seconds: f64,
    pub error: Option<String>,
    pub cancelled: bool,
    /// Whole-file skip because this content was already imported.
    /// Stores the previously imported filename.
    pub skipped_duplicate_of: Option<String>,
    pub quality: ImportQuality,
}

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Workbook sheets to import. `None` imports every readable sheet.
    pub selected_sheets: Option<BTreeSet<String>>,
    /// Per-sheet semantic overrides keyed by zero-based source column index.
    /// `Some(field)` maps the value into the canonical semantic column;
    /// `None` explicitly keeps that source column unmapped.
    pub sheet_semantics: BTreeMap<String, BTreeMap<usize, Option<SemanticField>>>,
    /// Per-sheet values supplied by a saved source profile when the source has
    /// no currency or weight-unit column. Only `Currency` and `WeightUnit` are
    /// accepted, and values are materialized into existing canonical storage.
    pub sheet_fixed_values: BTreeMap<String, BTreeMap<SemanticField, String>>,
}

impl ImportOptions {
    pub fn selected_sheets<I, S>(sheets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            selected_sheets: Some(sheets.into_iter().map(Into::into).collect()),
            ..Default::default()
        }
    }

    pub fn with_sheet_semantics<I>(mut self, sheet: impl Into<String>, semantics: I) -> Self
    where
        I: IntoIterator<Item = (usize, Option<SemanticField>)>,
    {
        self.sheet_semantics
            .insert(sheet.into(), semantics.into_iter().collect());
        self
    }

    pub fn with_sheet_fixed_values<I, S>(mut self, sheet: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = (SemanticField, S)>,
        S: Into<String>,
    {
        self.sheet_fixed_values.insert(
            sheet.into(),
            values
                .into_iter()
                .map(|(semantic, value)| (semantic, value.into()))
                .collect(),
        );
        self
    }
}

/// Source for a schema column value in a file row.
#[derive(Clone, Debug)]
enum ColSrc {
    /// The file does not contain this column.
    Missing,
    Cell(usize),
    Fixed(String),
    /// Several file columns joined with a separator, such as `UA100290/2024/102794`.
    Join(Vec<usize>, &'static str),
}

// ---------- header mapping ----------

// Generic table detection and semantic inference live in import/detection.rs.

fn semantic_for_schema_column(name: &str) -> Option<SemanticField> {
    // Single source of truth lives in the schema profile.
    crate::schema::semantic_for_column(name)
}

fn mapped_source_indices(src: &ColSrc) -> Vec<usize> {
    match src {
        ColSrc::Cell(index) => vec![*index],
        // Joined values are materialized for search and analytics, while each
        // original part remains JSON-backed and independently visible.
        ColSrc::Join(_, _) | ColSrc::Fixed(_) | ColSrc::Missing => Vec::new(),
    }
}

/// Source columns the mapping does not consume, paired with their header names,
/// in file order. These are preserved per row in the `extra` payload so the app
/// keeps every column, not only fields with inferred semantics.
fn unmapped_columns(headers: &[String], mapping: &[ColSrc]) -> Vec<(usize, String)> {
    let shape = TableShape::from_headers(headers.iter().cloned());
    let mut consumed = std::collections::HashSet::new();
    for src in mapping {
        match src {
            ColSrc::Cell(i) => {
                consumed.insert(*i);
            }
            ColSrc::Join(_, _) | ColSrc::Fixed(_) | ColSrc::Missing => {}
        }
    }
    headers
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .filter_map(|(i, _)| {
            shape
                .columns
                .iter()
                .find(|column| column.source_index == i)
                .map(|column| (i, column.header.clone()))
        })
        .collect()
}

fn recognized_columns(mapping: &[ColSrc]) -> usize {
    mapping
        .iter()
        .filter(|src| matches!(src, ColSrc::Cell(_) | ColSrc::Join(_, _)))
        .count()
}

fn cell_has_value(data: &Data) -> bool {
    match data {
        Data::Empty | Data::Error(_) => false,
        Data::String(s) => !s.trim().is_empty(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => !s.trim().is_empty(),
        Data::Float(_) | Data::Int(_) | Data::Bool(_) | Data::DateTime(_) => true,
    }
}

// ---------- import ----------

/// File content hash, streamed without loading the whole file into memory.
pub fn file_content_hash(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.digest128()))
}

/// A lightweight preview of a spreadsheet's structure without importing it:
/// the first sheets with their size, header row, and one sample data row.
/// Powers the CLI `peek` command and the browser import preview.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkbookPeek {
    pub sheets: Vec<SheetPeek>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SheetPeek {
    pub name: String,
    pub rows: usize,
    pub cols: usize,
    pub header_row: usize,
    pub layout: String,
    pub columns: Vec<ColumnPeek>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ColumnPeek {
    pub index: usize,
    pub id: String,
    pub header: String,
    pub sample: String,
    pub role: ColumnRole,
    pub semantic: Option<SemanticField>,
}

fn shape_for_headers(
    headers: &[String],
    mapping: Option<&[ColSrc]>,
    inferred_semantics: Option<&BTreeMap<usize, SemanticField>>,
    semantic_overrides: Option<&BTreeMap<usize, Option<SemanticField>>>,
) -> TableShape {
    let mut shape = TableShape::from_headers(headers.iter().cloned());
    if let Some(inferred_semantics) = inferred_semantics {
        for column in &mut shape.columns {
            column.semantic = inferred_semantics.get(&column.source_index).copied();
        }
    }
    if let Some(mapping) = mapping {
        for (target_idx, src) in mapping.iter().enumerate() {
            let semantic = semantic_for_schema_column(COLUMNS[target_idx].name);
            if let ColSrc::Join(parts, _) = src {
                for source_index in parts {
                    if let Some(column) = shape
                        .columns
                        .iter_mut()
                        .find(|column| column.source_index == *source_index)
                    {
                        // The canonical joined value drives analytics, while
                        // every raw part stays independently visible.
                        column.semantic = None;
                        column.storage = ColumnStorage::SourceJson;
                    }
                }
            }
            for source_index in mapped_source_indices(src) {
                if let Some(column) = shape
                    .columns
                    .iter_mut()
                    .find(|column| column.source_index == source_index)
                {
                    if let Some(semantic) = semantic {
                        column.semantic = Some(semantic);
                    }
                    column.storage =
                        ColumnStorage::SchemaColumn(COLUMNS[target_idx].name.to_string());
                }
            }
        }
    }
    if let Some(overrides) = semantic_overrides {
        for (source_index, semantic) in overrides {
            if let Some(column) = shape
                .columns
                .iter_mut()
                .find(|column| column.source_index == *source_index)
            {
                column.semantic = *semantic;
            }
        }
    }
    shape
}

fn remove_source_from_mapping(source: &ColSrc, source_index: usize) -> ColSrc {
    match source {
        ColSrc::Missing => ColSrc::Missing,
        ColSrc::Fixed(value) => ColSrc::Fixed(value.clone()),
        ColSrc::Cell(index) if *index == source_index => ColSrc::Missing,
        ColSrc::Cell(index) => ColSrc::Cell(*index),
        ColSrc::Join(parts, separator) => {
            let remaining = parts
                .iter()
                .copied()
                .filter(|index| *index != source_index)
                .collect::<Vec<_>>();
            match remaining.as_slice() {
                [] => ColSrc::Missing,
                [index] => ColSrc::Cell(*index),
                _ => ColSrc::Join(remaining, separator),
            }
        }
    }
}

fn validate_fixed_values(
    values: &BTreeMap<SemanticField, String>,
) -> Result<BTreeMap<SemanticField, String>, String> {
    if values.len() > 2 {
        return Err("At most two fixed semantic values are supported.".to_string());
    }
    let mut validated = BTreeMap::new();
    for (semantic, value) in values {
        if !matches!(
            semantic,
            SemanticField::Currency | SemanticField::WeightUnit
        ) {
            return Err(format!(
                "Fixed values are not supported for {semantic:?}; use Currency or WeightUnit."
            ));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("Fixed {semantic:?} value must not be empty."));
        }
        if value.chars().count() > 32 {
            return Err(format!(
                "Fixed {semantic:?} value must be at most 32 characters."
            ));
        }
        validated.insert(*semantic, value.to_string());
    }
    Ok(validated)
}

fn validate_import_options(options: &ImportOptions) -> Result<(), String> {
    for values in options.sheet_fixed_values.values() {
        validate_fixed_values(values)?;
    }
    Ok(())
}

fn fixed_storage_target(semantic: SemanticField) -> Option<usize> {
    let name = match semantic {
        SemanticField::Currency => "contract",
        SemanticField::WeightUnit => "unit",
        _ => return None,
    };
    COLUMNS.iter().position(|column| column.name == name)
}

fn apply_fixed_values(
    mut mapping: Vec<ColSrc>,
    values: &BTreeMap<SemanticField, String>,
) -> Result<Vec<ColSrc>, String> {
    for (semantic, value) in validate_fixed_values(values)? {
        let target = fixed_storage_target(semantic)
            .ok_or_else(|| format!("No canonical storage exists for fixed {semantic:?}."))?;
        mapping[target] = ColSrc::Fixed(value);
    }
    Ok(mapping)
}

fn apply_semantic_overrides(
    headers: &[String],
    mut mapping: Vec<ColSrc>,
    overrides: &BTreeMap<usize, Option<SemanticField>>,
) -> Result<Vec<ColSrc>, String> {
    let mut targets = HashMap::<usize, usize>::new();
    for (source_index, semantic) in overrides {
        if *source_index >= headers.len() {
            return Err(format!(
                "Column {} is outside the detected table width of {}.",
                source_index + 1,
                headers.len()
            ));
        }
        for source in &mut mapping {
            *source = remove_source_from_mapping(source, *source_index);
        }
        let Some(semantic) = semantic else {
            continue;
        };
        let Some(target_name) = crate::schema::column_for_semantic(*semantic) else {
            continue;
        };
        let target_index = COLUMNS
            .iter()
            .position(|column| column.name == target_name)
            .ok_or_else(|| format!("No canonical storage column exists for {semantic:?}."))?;
        if let Some(previous_source) = targets.insert(target_index, *source_index) {
            return Err(format!(
                "Columns {} and {} are both mapped to {semantic:?}.",
                previous_source + 1,
                source_index + 1
            ));
        }
        mapping[target_index] = ColSrc::Cell(*source_index);
    }
    Ok(mapping)
}

fn semantics_with_overrides(
    inferred: &BTreeMap<usize, SemanticField>,
    overrides: &BTreeMap<usize, Option<SemanticField>>,
) -> BTreeMap<usize, SemanticField> {
    let mut semantics = inferred.clone();
    for (source_index, semantic) in overrides {
        match semantic {
            Some(semantic) => {
                semantics.insert(*source_index, *semantic);
            }
            None => {
                semantics.remove(source_index);
            }
        }
    }
    semantics
}

fn preview_sheet(
    name: String,
    rows: usize,
    cols: usize,
    scanned: &[Vec<Data>],
) -> Option<SheetPeek> {
    let detected = detect_table(scanned)?;
    let sample: Vec<String> = scanned
        .get(detected.header_index + 1)
        .map(|row| {
            row.iter()
                .map(|cell| normalize_value(cell).chars().take(80).collect())
                .collect()
        })
        .unwrap_or_default();
    let shape = shape_for_headers(
        &detected.headers,
        Some(&detected.plan.columns),
        Some(&detected.plan.semantics),
        None,
    );
    let width = detected.headers.len().max(sample.len());
    let columns = (0..width)
        .map(|index| {
            let source = shape
                .columns
                .iter()
                .find(|column| column.source_index == index);
            ColumnPeek {
                index,
                id: source
                    .map(|column| column.id.clone())
                    .unwrap_or_else(|| format!("column_{}", index + 1)),
                header: source
                    .map(|column| column.header.clone())
                    .or_else(|| detected.headers.get(index).cloned())
                    .unwrap_or_else(|| format!("Column {}", index + 1)),
                sample: sample.get(index).cloned().unwrap_or_default(),
                role: source.map(|column| column.role).unwrap_or(ColumnRole::Text),
                semantic: source.and_then(|column| column.semantic),
            }
        })
        .collect();
    Some(SheetPeek {
        name,
        rows,
        cols,
        header_row: detected.header_index + 1,
        layout: detected.layout().to_string(),
        columns,
    })
}

/// Reads at most `max_sheets` sheets and previews each one's header row plus the
/// first data row. Read-only: it never touches the database.
pub fn peek_file(path: &Path, max_sheets: usize) -> Result<WorkbookPeek, String> {
    if is_delimited_path(path) {
        return peek_delimited_file(path);
    }
    let mut workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names: Vec<String> = workbook.sheet_names().to_vec();
    let mut sheets = Vec::new();
    for (index, name) in names.iter().enumerate().take(max_sheets) {
        let Some(Ok(range)) = workbook.worksheet_range_at(index) else {
            continue;
        };
        let scanned = range
            .rows()
            .take(DETECTION_BUFFER_ROWS)
            .map(|row| row.to_vec())
            .collect::<Vec<_>>();
        if let Some(sheet) = preview_sheet(name.clone(), range.height(), range.width(), &scanned) {
            sheets.push(sheet);
        }
    }
    if sheets.is_empty() {
        return Err("No readable sheets in this file.".to_string());
    }
    Ok(WorkbookPeek { sheets })
}

/// Imports one file. progress(phase, done, total); total == 0 means unknown.
pub fn import_file(
    db: &mut Db,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportPhase, u64, u64),
) -> FileSummary {
    import_file_with_options(db, path, &ImportOptions::default(), cancel, progress)
}

pub fn import_file_with_options(
    db: &mut Db,
    path: &Path,
    options: &ImportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportPhase, u64, u64),
) -> FileSummary {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut summary = FileSummary {
        file_name: file_name.clone(),
        ..Default::default()
    };
    let started = Instant::now();
    progress(ImportPhase::Reading, 0, 0);
    if let Err(error) = validate_import_options(options) {
        summary.error = Some(error);
        summary.seconds = started.elapsed().as_secs_f64();
        return summary;
    }

    // Whole-file deduplication: identical content is not parsed again.
    let file_hash = match file_content_hash(path) {
        Ok(content_hash) => {
            let hash = import_fingerprint(&content_hash, options);
            if let Some(previous) = db.find_import_by_hash(&hash) {
                summary.skipped_duplicate_of = Some(previous);
                summary.seconds = started.elapsed().as_secs_f64();
                return summary;
            }
            hash
        }
        Err(e) => {
            summary.error = Some(e);
            return summary;
        }
    };

    let mut committed = false;
    match db.begin_import_file() {
        Ok(()) => match import_file_inner(
            db,
            path,
            &file_name,
            options,
            &file_hash,
            cancel,
            progress,
            &mut summary,
        ) {
            Ok(()) if !summary.cancelled => match db.commit_import_file() {
                Ok(()) => {
                    committed = true;
                }
                Err(e) => {
                    summary.error = Some(e.to_string());
                    db.rollback_import_file();
                }
            },
            Ok(()) => {
                db.rollback_import_file();
                summary.imported = 0;
                summary.duplicates = 0;
            }
            Err(e) => {
                summary.error = Some(e);
                db.rollback_import_file();
                summary.imported = 0;
                summary.duplicates = 0;
            }
        },
        Err(e) => summary.error = Some(e.to_string()),
    }
    if committed {
        progress(ImportPhase::Indexing, 0, 0);
        match db.index_fts(cancel, |done, total| {
            progress(ImportPhase::Indexing, done, total)
        }) {
            Ok((_, fts_cancelled)) => {
                summary.cancelled |= fts_cancelled;
            }
            Err(e) => summary.error = Some(e.to_string()),
        }
    }
    summary.seconds = started.elapsed().as_secs_f64();
    finalize_quality(&mut summary);
    if committed {
        // Store the hash only for fully imported files, so interrupted imports
        // can be retried.
        db.add_import_log(ImportLogWrite {
            file_name: &file_name,
            total_rows: summary.total_rows,
            imported: summary.imported,
            duplicates: summary.duplicates,
            seconds: summary.seconds,
            file_hash: Some(file_hash.as_str()),
            quality: &summary.quality,
        });
        // Imports are the biggest WAL producers. Truncate the log once the
        // import is durable so the database does not keep a WAL the size of
        // the imported data on disk. Best effort: concurrent readers only
        // make it partial.
        let _ = db.checkpoint_wal_truncate();
    }
    summary
}

fn import_fingerprint(content_hash: &str, options: &ImportOptions) -> String {
    if options.selected_sheets.is_none()
        && options.sheet_semantics.is_empty()
        && options.sheet_fixed_values.is_empty()
    {
        return content_hash.to_string();
    }
    let mut hasher = Xxh3::new();
    hasher.update(content_hash.as_bytes());
    if let Some(selected_sheets) = &options.selected_sheets {
        hasher.update(b"\0selected");
        for sheet in selected_sheets {
            hasher.update(&[0]);
            hasher.update(sheet.as_bytes());
        }
    }
    for (sheet, semantics) in &options.sheet_semantics {
        hasher.update(b"\0mapping\0");
        hasher.update(sheet.as_bytes());
        for (source_index, semantic) in semantics {
            hasher.update(&(*source_index as u64).to_le_bytes());
            let encoded = semantic
                .map(|field| format!("{field:?}"))
                .unwrap_or_else(|| "None".to_string());
            hasher.update(encoded.as_bytes());
        }
    }
    for (sheet, values) in &options.sheet_fixed_values {
        hasher.update(b"\0fixed\0");
        hasher.update(sheet.as_bytes());
        for (semantic, value) in values {
            hasher.update(format!("{semantic:?}").as_bytes());
            hasher.update(&[0]);
            hasher.update(value.trim().as_bytes());
        }
    }
    format!("{content_hash}:s:{:016x}", hasher.digest())
}

fn finalize_quality(summary: &mut FileSummary) {
    let quality = &mut summary.quality;
    if quality.layout.is_empty() {
        return;
    }
    if quality.header_row > 1 {
        push_quality_warning(
            quality,
            &format!(
                "Header row was detected at row {}; earlier rows were treated as title or metadata.",
                quality.header_row
            ),
        );
    }
    if quality.recognized_columns > 0 && quality.recognized_columns < 6 {
        push_quality_warning(
            quality,
            &format!(
                "Only {} semantic fields were recognized; source columns are still preserved, but domain analytics may be limited.",
                quality.recognized_columns
            ),
        );
    }
    let total_cells = quality.non_empty_cells + quality.empty_cells;
    if total_cells > 0 {
        let empty_percent = quality.empty_cells as f64 * 100.0 / total_cells as f64;
        if empty_percent >= 90.0 {
            push_quality_warning(
                quality,
                &format!("{empty_percent:.0}% of imported table cells are empty."),
            );
        }
    }
}

fn push_quality_warning(quality: &mut ImportQuality, warning: &str) {
    if !quality.warnings.iter().any(|existing| existing == warning) {
        quality.warnings.push(warning.to_string());
    }
}

fn is_delimited_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "csv" | "tsv"))
}

fn detect_delimiter(path: &Path) -> Result<u8, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut sample = vec![0u8; 256 * 1024];
    let read = file.read(&mut sample).map_err(|error| error.to_string())?;
    sample.truncate(read);
    if sample.is_empty() {
        return Err("The delimited file is empty.".to_string());
    }

    let mut best: Option<(usize, usize, u8)> = None;
    for delimiter in [b'\t', b';', b',', b'|'] {
        let widths = delimited_record_widths(&sample, delimiter, 24);
        let mut frequencies: HashMap<usize, usize> = HashMap::new();
        for width in widths.into_iter().filter(|width| *width > 1) {
            *frequencies.entry(width).or_default() += 1;
        }
        let candidate = frequencies
            .into_iter()
            .max_by_key(|(width, frequency)| (*frequency, *width))
            .map(|(width, frequency)| (frequency, width, delimiter));
        if candidate > best {
            best = candidate;
        }
    }
    best.map(|(_, _, delimiter)| delimiter).ok_or_else(|| {
        "Could not detect a comma, semicolon, tab, or pipe-delimited table.".to_string()
    })
}

fn delimited_record_widths(sample: &[u8], delimiter: u8, limit: usize) -> Vec<usize> {
    let mut widths = Vec::new();
    let mut fields = 1usize;
    let mut quoted = false;
    let mut index = 0usize;
    while index < sample.len() && widths.len() < limit {
        match sample[index] {
            b'"' if quoted && sample.get(index + 1) == Some(&b'"') => index += 1,
            b'"' => quoted = !quoted,
            byte if byte == delimiter && !quoted => fields += 1,
            b'\n' if !quoted => {
                widths.push(fields);
                fields = 1;
            }
            _ => {}
        }
        index += 1;
    }
    if fields > 1 && widths.len() < limit {
        widths.push(fields);
    }
    widths
}

fn peek_delimited_file(path: &Path) -> Result<WorkbookPeek, String> {
    let delimiter = detect_delimiter(path)?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .map_err(|error| error.to_string())?;
    let mut scanned = Vec::new();
    for record in reader.records().take(DETECTION_BUFFER_ROWS) {
        let record = record.map_err(|error| error.to_string())?;
        scanned.push(
            record
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let value = if index == 0 {
                        value.trim_start_matches('\u{feff}')
                    } else {
                        value
                    };
                    Data::String(collapse_ws(value))
                })
                .collect::<Vec<_>>(),
        );
    }
    if scanned.is_empty() {
        return Err("The delimited file has no header row.".to_string());
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Delimited table".to_string());
    let cols = scanned.iter().map(Vec::len).max().unwrap_or(0);
    let sheet = preview_sheet(name, scanned.len(), cols, &scanned)
        .ok_or_else(|| "No non-empty table was found in the delimited file.".to_string())?;
    Ok(WorkbookPeek {
        sheets: vec![sheet],
    })
}

struct DelimitedImportSpec<'a> {
    path: &'a Path,
    file_name: &'a str,
    import_fingerprint: &'a str,
    semantic_overrides: BTreeMap<usize, Option<SemanticField>>,
    fixed_values: BTreeMap<SemanticField, String>,
}

fn import_delimited_file_inner(
    db: &mut Db,
    spec: DelimitedImportSpec<'_>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportPhase, u64, u64),
    summary: &mut FileSummary,
) -> Result<(), String> {
    let DelimitedImportSpec {
        path,
        file_name,
        import_fingerprint,
        semantic_overrides,
        fixed_values,
    } = spec;
    let delimiter = detect_delimiter(path)?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .map_err(|error| error.to_string())?;
    let mut sink = RowSink {
        db,
        file_name,
        table_name: file_name,
        import_fingerprint,
        cancel,
        progress,
        summary,
        total_rows_hint: 0,
        mapping: None,
        extra_cols: Vec::new(),
        year_extra_index: None,
        date_extra_indices: HashSet::new(),
        scanned: Vec::new(),
        batch: Vec::with_capacity(BATCH_SIZE),
        rows_seen: 0,
        semantic_overrides,
        fixed_values,
        schema_id: None,
        source_id: None,
    };
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let row = record
            .iter()
            .map(|value| Data::String(value.to_string()))
            .collect();
        if !sink.row(row)? {
            break;
        }
    }
    sink.finish()
}

fn import_file_inner(
    db: &mut Db,
    path: &Path,
    file_name: &str,
    options: &ImportOptions,
    import_fingerprint: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportPhase, u64, u64),
    summary: &mut FileSummary,
) -> Result<(), String> {
    if is_delimited_path(path) {
        if options
            .selected_sheets
            .as_ref()
            .is_some_and(BTreeSet::is_empty)
        {
            return Err("No table was selected for import.".to_string());
        }
        let semantics = options
            .sheet_semantics
            .get(file_name)
            .cloned()
            .or_else(|| {
                (options.sheet_semantics.len() == 1)
                    .then(|| options.sheet_semantics.values().next().cloned())
                    .flatten()
            })
            .unwrap_or_default();
        let fixed_values = options
            .sheet_fixed_values
            .get(file_name)
            .cloned()
            .or_else(|| {
                (options.sheet_fixed_values.len() == 1)
                    .then(|| options.sheet_fixed_values.values().next().cloned())
                    .flatten()
            })
            .unwrap_or_default();
        return import_delimited_file_inner(
            db,
            DelimitedImportSpec {
                path,
                file_name,
                import_fingerprint,
                semantic_overrides: semantics,
                fixed_values,
            },
            cancel,
            progress,
            summary,
        );
    }
    let workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let sheet_names = workbook.sheet_names().to_vec();
    drop(workbook);
    if sheet_names.is_empty() {
        return Err("The workbook contains no sheets.".to_string());
    }

    let selected_sheet_names = if let Some(selected) = &options.selected_sheets {
        if selected.is_empty() {
            return Err("No workbook sheet was selected for import.".to_string());
        }
        let available: BTreeSet<&str> = sheet_names.iter().map(String::as_str).collect();
        let missing: Vec<&str> = selected
            .iter()
            .map(String::as_str)
            .filter(|name| !available.contains(name))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Selected workbook sheets were not found: {}.",
                missing.join(", ")
            ));
        }
        sheet_names
            .iter()
            .filter(|name| selected.contains(name.as_str()))
            .cloned()
            .collect()
    } else {
        sheet_names.clone()
    };

    let qualify_source = sheet_names.len() > 1;
    let mut imported_sheets = Vec::new();
    for sheet_name in selected_sheet_names {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let source_name = if qualify_source {
            format!("{file_name} [{sheet_name}]")
        } else {
            file_name.to_string()
        };
        let mut sheet_summary = FileSummary {
            file_name: source_name.clone(),
            ..Default::default()
        };
        let sheet_import = SheetImportSpec {
            path,
            sheet_name: &sheet_name,
            source_name: &source_name,
            import_fingerprint,
            semantic_overrides: options
                .sheet_semantics
                .get(&sheet_name)
                .cloned()
                .unwrap_or_default(),
            fixed_values: options
                .sheet_fixed_values
                .get(&sheet_name)
                .cloned()
                .unwrap_or_default(),
        };
        import_single_sheet_inner(db, sheet_import, cancel, progress, &mut sheet_summary)?;
        if !sheet_summary.quality.layout.is_empty() {
            merge_sheet_summary(summary, &sheet_summary);
            imported_sheets.push(sheet_name);
        }
        if sheet_summary.cancelled {
            summary.cancelled = true;
            break;
        }
    }

    if imported_sheets.is_empty() && !summary.cancelled {
        return Err("No non-empty table was found in the workbook.".to_string());
    }
    if imported_sheets.len() > 1 {
        summary.quality.layout = "multi-sheet workbook".to_string();
        summary.quality.header_row = 0;
        push_quality_warning(
            &mut summary.quality,
            &format!("Imported sheets: {}.", imported_sheets.join(", ")),
        );
    }
    Ok(())
}

fn merge_sheet_summary(target: &mut FileSummary, sheet: &FileSummary) {
    target.total_rows += sheet.total_rows;
    target.imported += sheet.imported;
    target.duplicates += sheet.duplicates;
    target.cancelled |= sheet.cancelled;
    if target.quality.layout.is_empty() {
        target.quality = sheet.quality.clone();
        return;
    }
    target.quality.source_columns = target
        .quality
        .source_columns
        .max(sheet.quality.source_columns);
    target.quality.recognized_columns = target
        .quality
        .recognized_columns
        .max(sheet.quality.recognized_columns);
    target.quality.extra_columns = target
        .quality
        .extra_columns
        .max(sheet.quality.extra_columns);
    target.quality.non_empty_cells += sheet.quality.non_empty_cells;
    target.quality.empty_cells += sheet.quality.empty_cells;
    for warning in &sheet.quality.warnings {
        push_quality_warning(&mut target.quality, warning);
    }
}

struct SheetImportSpec<'a> {
    path: &'a Path,
    sheet_name: &'a str,
    source_name: &'a str,
    import_fingerprint: &'a str,
    semantic_overrides: BTreeMap<usize, Option<SemanticField>>,
    fixed_values: BTreeMap<SemanticField, String>,
}

fn import_single_sheet_inner(
    db: &mut Db,
    spec: SheetImportSpec<'_>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportPhase, u64, u64),
    summary: &mut FileSummary,
) -> Result<(), String> {
    let SheetImportSpec {
        path,
        sheet_name,
        source_name,
        import_fingerprint,
        semantic_overrides,
        fixed_values,
    } = spec;
    let mut workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let sheet = workbook
        .sheet_names()
        .iter()
        .find(|name| name.as_str() == sheet_name)
        .cloned()
        .ok_or_else(|| format!("The file has no sheet named \"{sheet_name}\"."))?;

    let mut sink = RowSink {
        db,
        file_name: source_name,
        table_name: sheet_name,
        import_fingerprint,
        cancel,
        progress,
        summary,
        total_rows_hint: 0,
        mapping: None,
        extra_cols: Vec::new(),
        year_extra_index: None,
        date_extra_indices: HashSet::new(),
        scanned: Vec::new(),
        batch: Vec::with_capacity(BATCH_SIZE),
        rows_seen: 0,
        semantic_overrides,
        fixed_values,
        schema_id: None,
        source_id: None,
    };

    match &mut workbook {
        Sheets::Xlsx(xlsx) => {
            let mut reader = xlsx
                .worksheet_cells_reader(&sheet)
                .map_err(|e| e.to_string())?;
            let dims = reader.dimensions();
            sink.total_rows_hint = (dims.end.0.saturating_sub(dims.start.0)) as u64;
            let mut assembler = RowAssembler::default();
            while let Some(cell) = reader.next_cell().map_err(|e| e.to_string())? {
                let (row, col) = cell.get_position();
                let data: Data = cell.get_value().clone().into();
                if let Some(done_row) = assembler.push(row, col, data)
                    && !sink.row(done_row)?
                {
                    return sink.finish();
                }
            }
            if let Some(done_row) = assembler.take() {
                sink.row(done_row)?;
            }
            sink.finish()
        }
        Sheets::Xlsb(xlsb) => {
            let mut reader = xlsb
                .worksheet_cells_reader(&sheet)
                .map_err(|e| e.to_string())?;
            let dims = reader.dimensions();
            sink.total_rows_hint = (dims.end.0.saturating_sub(dims.start.0)) as u64;
            let mut assembler = RowAssembler::default();
            while let Some(cell) = reader.next_cell().map_err(|e| e.to_string())? {
                let (row, col) = cell.get_position();
                let data: Data = cell.get_value().clone().into();
                if let Some(done_row) = assembler.push(row, col, data)
                    && !sink.row(done_row)?
                {
                    return sink.finish();
                }
            }
            if let Some(done_row) = assembler.take() {
                sink.row(done_row)?;
            }
            sink.finish()
        }
        // Old .xls and .ods files are uncommon, so read them as full ranges.
        other => {
            let range = other.worksheet_range(&sheet).map_err(|e| e.to_string())?;
            sink.total_rows_hint = (range.height().saturating_sub(1)) as u64;
            for row in range.rows() {
                if !sink.row(row.to_vec())? {
                    break;
                }
            }
            sink.finish()
        }
    }
}

/// Assembles a cell stream into rows. Gaps between cells are filled with
/// `Data::Empty`; fully empty sheet rows are not emitted by the reader.
#[derive(Default)]
struct RowAssembler {
    current_row: Option<u32>,
    cells: Vec<Data>,
}

impl RowAssembler {
    fn push(&mut self, row: u32, col: u32, value: Data) -> Option<Vec<Data>> {
        let mut finished = None;
        match self.current_row {
            Some(current) if current == row => {}
            Some(_) => finished = Some(std::mem::take(&mut self.cells)),
            None => {}
        }
        self.current_row = Some(row);
        let col = col as usize;
        if self.cells.len() < col {
            self.cells.resize(col, Data::Empty);
        }
        if self.cells.len() == col {
            self.cells.push(value);
        } else {
            self.cells[col] = value;
        }
        finished
    }

    fn take(&mut self) -> Option<Vec<Data>> {
        self.current_row.take()?;
        Some(std::mem::take(&mut self.cells))
    }
}

/// Row sink: finds the header row, normalizes data, and writes batches.
struct RowSink<'a> {
    db: &'a mut Db,
    file_name: &'a str,
    table_name: &'a str,
    import_fingerprint: &'a str,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(ImportPhase, u64, u64),
    summary: &'a mut FileSummary,
    total_rows_hint: u64,
    mapping: Option<Vec<ColSrc>>,
    /// Source columns not consumed by the mapping: (column index, header name).
    /// Captured verbatim per row so no source data is lost on import.
    extra_cols: Vec<(usize, String)>,
    year_extra_index: Option<usize>,
    date_extra_indices: HashSet<usize>,
    scanned: Vec<Vec<Data>>,
    batch: Vec<ImportRecord>,
    rows_seen: u64,
    semantic_overrides: BTreeMap<usize, Option<SemanticField>>,
    fixed_values: BTreeMap<SemanticField, String>,
    schema_id: Option<i64>,
    source_id: Option<i64>,
}

impl RowSink<'_> {
    /// Ok(false) means import was cancelled and no more rows are needed.
    fn row(&mut self, row: Vec<Data>) -> Result<bool, String> {
        self.rows_seen += 1;
        if self.mapping.is_none() {
            self.scanned.push(row);
            if self.scanned.len() >= DETECTION_BUFFER_ROWS {
                return self.start_detected_import();
            }
            return Ok(true);
        }
        self.data_row(row)
    }

    fn start_detected_import(&mut self) -> Result<bool, String> {
        let scanned = std::mem::take(&mut self.scanned);
        let Some(DetectedTable {
            header_index,
            headers,
            plan,
        }) = detect_table(&scanned)
        else {
            return Err(self.missing_error());
        };
        let mapping = apply_semantic_overrides(&headers, plan.columns, &self.semantic_overrides)?;
        let mapping = apply_fixed_values(mapping, &self.fixed_values)?;
        let semantics = semantics_with_overrides(&plan.semantics, &self.semantic_overrides);
        let shape = shape_for_headers(
            &headers,
            Some(&mapping),
            Some(&semantics),
            Some(&self.semantic_overrides),
        );
        let (source_schema, source) = self
            .db
            .register_import_source_schema(
                &headers,
                &shape,
                &self.fixed_values,
                self.file_name,
                self.table_name,
                self.import_fingerprint,
            )
            .map_err(|error| error.to_string())?;
        self.schema_id = Some(source_schema.id);
        self.source_id = Some(source.id);
        self.extra_cols = unmapped_columns(&headers, &mapping);
        self.mapping = Some(mapping);
        let extra_indices = self
            .extra_cols
            .iter()
            .map(|(source_index, _)| *source_index)
            .collect::<HashSet<_>>();
        self.date_extra_indices = semantics
            .iter()
            .filter_map(|(source_index, semantic)| {
                (*semantic == SemanticField::Date && extra_indices.contains(source_index))
                    .then_some(*source_index)
            })
            .collect();
        self.year_extra_index = self.date_extra_indices.iter().copied().next();
        self.remember_extra_columns();
        self.summary.quality = ImportQuality {
            layout: if self.semantic_overrides.is_empty() && self.fixed_values.is_empty() {
                "generic table".to_string()
            } else {
                "custom mapping".to_string()
            },
            header_row: (header_index + 1) as u64,
            source_columns: headers.len() as u64,
            recognized_columns: self
                .mapping
                .as_ref()
                .map(|mapping| recognized_columns(mapping) as u64)
                .unwrap_or(0),
            extra_columns: self.extra_cols.len() as u64,
            ..Default::default()
        };
        for row in scanned.into_iter().skip(header_index + 1) {
            if !self.data_row(row)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn data_row(&mut self, row: Vec<Data>) -> Result<bool, String> {
        let mapping = self.mapping.as_ref().expect("mapping initialized");
        let mut values: Vec<String> = Vec::with_capacity(COLUMNS.len());
        for (i, src) in mapping.iter().enumerate() {
            let value = match src {
                ColSrc::Missing => String::new(),
                ColSrc::Fixed(value) => value.clone(),
                ColSrc::Cell(pos) => row
                    .get(*pos)
                    .map(|d| normalize_cell(d, i == DATE_COL))
                    .unwrap_or_default(),
                ColSrc::Join(parts, sep) => parts
                    .iter()
                    .filter_map(|pos| row.get(*pos))
                    .map(normalize_value)
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
                    .join(sep),
            };
            values.push(value);
        }
        let extra = self.collect_extra(&row);
        if values.iter().all(|v| v.is_empty()) && extra.is_none() {
            return Ok(true);
        }
        self.count_quality_cells(&row);
        values[DATE_COL] = normalize_date(&values[DATE_COL]);
        let hash = canonical_record_hash(&values, extra.as_deref());
        self.summary.total_rows += 1;
        let year = extract_year(&values[DATE_COL]).or_else(|| self.year_from_extra(&row));
        self.batch.push(ImportRecord {
            hash,
            year,
            values,
            extra,
        });
        if self.batch.len() >= BATCH_SIZE {
            self.flush_batch()?;
            if self.cancel.load(Ordering::Relaxed) {
                self.summary.cancelled = true;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn count_quality_cells(&mut self, row: &[Data]) {
        let width = self.summary.quality.source_columns as usize;
        if width == 0 {
            return;
        }
        for col in 0..width {
            if row.get(col).is_some_and(cell_has_value) {
                self.summary.quality.non_empty_cells += 1;
            } else {
                self.summary.quality.empty_cells += 1;
            }
        }
    }

    /// Builds the `extra` JSON payload (unmapped columns) for one source row.
    fn collect_extra(&self, row: &[Data]) -> Option<String> {
        if self.extra_cols.is_empty() {
            return None;
        }
        let pairs: Vec<(&str, String)> = self
            .extra_cols
            .iter()
            .filter_map(|(idx, name)| {
                let value = row
                    .get(*idx)
                    .map(|data| normalize_cell(data, self.date_extra_indices.contains(idx)))
                    .unwrap_or_default();
                (!value.is_empty()).then_some((name.as_str(), value))
            })
            .collect();
        if pairs.is_empty() {
            None
        } else {
            serde_json::to_string(&pairs).ok()
        }
    }

    fn year_from_extra(&self, row: &[Data]) -> Option<i64> {
        let source_index = self.year_extra_index?;
        let raw = row
            .get(source_index)
            .map(|data| normalize_cell(data, true))
            .unwrap_or_default();
        let normalized = normalize_date(&raw);
        extract_year(&normalized).or_else(|| extract_year(&raw))
    }

    fn remember_extra_columns(&self) {
        self.db
            .remember_extra_headers(self.extra_cols.iter().map(|(_, header)| header.as_str()));
    }

    fn flush_batch(&mut self) -> Result<(), String> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let (inserted, duplicates) = self
            .db
            .insert_batch_for_source(
                self.file_name,
                self.schema_id
                    .ok_or_else(|| "source schema was not registered".to_string())?,
                self.source_id
                    .ok_or_else(|| "import source was not registered".to_string())?,
                &self.batch,
            )
            .map_err(|e| e.to_string())?;
        self.summary.imported += inserted;
        self.summary.duplicates += duplicates;
        self.batch.clear();
        (self.progress)(ImportPhase::Inserting, self.rows_seen, self.total_rows_hint);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.mapping.is_none() {
            let sheet_is_empty = self
                .scanned
                .iter()
                .all(|row| row.iter().all(|cell| !cell_has_value(cell)));
            if sheet_is_empty {
                return Ok(());
            }
            self.start_detected_import()?;
        }
        self.flush_batch()?;
        Ok(())
    }

    fn missing_error(&self) -> String {
        "No non-empty header row was found in the first rows of the sheet.".to_string()
    }
}

// ---------- value normalization ----------

fn header_text(data: &Data) -> String {
    match data {
        Data::String(s) => collapse_ws(s.trim_start_matches('\u{feff}')),
        Data::Empty => String::new(),
        other => collapse_ws(&other.to_string()),
    }
}

/// Converts a cell value to a clean string: integer-like numbers without ".0",
/// ISO dates, and collapsed whitespace.
pub fn normalize_value(data: &Data) -> String {
    normalize_cell(data, false)
}

/// `expect_date` marks a date column; Excel serial numbers become dates.
pub fn normalize_cell(data: &Data, expect_date: bool) -> String {
    match data {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(s) => collapse_ws(s),
        Data::Float(f) => {
            if expect_date && let Some(date) = excel_serial_to_iso(*f) {
                return date;
            }
            float_to_string(*f)
        }
        Data::Int(i) => {
            if expect_date && let Some(date) = excel_serial_to_iso(*i as f64) {
                return date;
            }
            i.to_string()
        }
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(ndt) => {
                if dt.as_f64() < 1.0 {
                    // Time only: a fractional day without a date.
                    ndt.format("%H:%M:%S").to_string()
                } else if ndt.hour() == 0 && ndt.minute() == 0 && ndt.second() == 0 {
                    ndt.format("%Y-%m-%d").to_string()
                } else {
                    ndt.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            }
            None => float_to_string(dt.as_f64()),
        },
        Data::DateTimeIso(s) => collapse_ws(s),
        Data::DurationIso(s) => collapse_ws(s),
    }
}

/// Excel serial date (days since 1899-12-30) -> ISO date.
/// The range is limited to plausible years (1968-2064).
pub fn excel_serial_to_iso(serial: f64) -> Option<String> {
    if !serial.is_finite() || !(25000.0..=60000.0).contains(&serial) {
        return None;
    }
    let days = serial.trunc() as i64;
    let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?;
    let date = base.checked_add_signed(chrono::Duration::days(days))?;
    let secs = ((serial - days as f64) * 86400.0).round() as u32;
    if secs > 0 && secs < 86400 {
        let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, 0)?;
        Some(format!(
            "{} {}",
            date.format("%Y-%m-%d"),
            time.format("%H:%M:%S")
        ))
    } else {
        Some(date.format("%Y-%m-%d").to_string())
    }
}

fn float_to_string(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 {
        (f as i64).to_string()
    } else {
        f.to_string()
    }
}

pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // consumes leading whitespace
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// "31.12.2024" / "31/12/2024" / "31-12-2024" -> "2024-12-31".
/// "2024.12.31" / "2024-1-5" -> "2024-12-31" / "2024-01-05".
/// Existing ISO dates and unrecognized text are returned unchanged.
pub fn normalize_date(value: &str) -> String {
    let parts: Vec<&str> = value.split(['.', '/', '-']).collect();
    if parts.len() == 3 {
        // DD.MM.YYYY (also with '/' or '-' separators).
        if parts[0].len() <= 2
            && parts[1].len() <= 2
            && parts[2].len() == 4
            && let (Ok(d), Ok(m), Ok(y)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
            && (1..=31).contains(&d)
            && (1..=12).contains(&m)
        {
            return format!("{y:04}-{m:02}-{d:02}");
        }
        // YYYY.MM.DD / YYYY-M-D: canonicalize the separator to '-' and zero-pad
        // the month/day so the monthly-analytics filter (which matches the
        // "YYYY-MM" prefix) still sees these rows instead of silently dropping
        // them. ISO "2024-12-31" passes through here unchanged.
        if parts[0].len() == 4
            && parts[1].len() <= 2
            && parts[2].len() <= 2
            && let (Ok(y), Ok(m), Ok(d)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            )
            && (1..=12).contains(&m)
            && (1..=31).contains(&d)
        {
            return format!("{y:04}-{m:02}-{d:02}");
        }
    }
    value.to_string()
}

/// File row hash. Trailing empty cells are trimmed so the hash does not depend
/// on the reading mode: streaming cells or full range.
pub fn row_hash_cells(row: &[Data]) -> [u8; 16] {
    let mut end = row.len();
    while end > 0 && matches!(row[end - 1], Data::Empty) {
        end -= 1;
    }
    let mut hasher = Xxh3::new();
    for (i, cell) in row[..end].iter().enumerate() {
        if i > 0 {
            hasher.update(&[0x1f]);
        }
        hasher.update(normalize_value(cell).as_bytes());
    }
    hasher.digest128().to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::{ColSrc, unmapped_columns};

    #[test]
    fn unmapped_columns_preserve_blank_and_duplicate_headers() {
        let headers = vec![
            "SKU".to_string(),
            "".to_string(),
            "SKU".to_string(),
            "Value".to_string(),
        ];
        let mapping = vec![
            ColSrc::Cell(3),
            ColSrc::Missing,
            ColSrc::Missing,
            ColSrc::Missing,
        ];

        let extra = unmapped_columns(&headers, &mapping);

        assert_eq!(
            extra,
            vec![
                (0, "SKU".to_string()),
                (1, "Column 2".to_string()),
                (2, "SKU (2)".to_string())
            ]
        );
    }
}
