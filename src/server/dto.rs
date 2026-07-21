//! Serializable data-transfer objects for the browser API. Core types that
//! already derive `Serialize` (analytics, pivots, query) are sent as-is; the
//! ones here wrap core types that do not, so the wire format stays stable and
//! independent of internal field layout.

use serde::Serialize;

use crate::db::{DatabaseStorageInfo, ImportLogEntry, ImportQuality, RecordCard};
use crate::import::FileSummary;
use crate::search::{ConditionOp, FieldInfo, FieldKind, FieldRef};

#[derive(Serialize)]
pub struct FieldDto {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub source: FieldSourceDto,
    pub operators: Vec<ConditionOp>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldSourceDto {
    Column { name: String },
    Extra { header: String },
    SourceField { field_id: String },
}

impl From<&FieldInfo> for FieldDto {
    fn from(field: &FieldInfo) -> Self {
        FieldDto {
            id: field.id.clone(),
            label: field.label.clone(),
            kind: field_kind_str(field.kind),
            source: match &field.source {
                FieldRef::Column(name) => FieldSourceDto::Column { name: name.clone() },
                FieldRef::Extra(header) => FieldSourceDto::Extra {
                    header: header.clone(),
                },
                FieldRef::SourceField(field_id) => FieldSourceDto::SourceField {
                    field_id: field_id.clone(),
                },
            },
            operators: field.operators.clone(),
        }
    }
}

pub fn field_dtos(fields: &[FieldInfo]) -> Vec<FieldDto> {
    fields.iter().map(FieldDto::from).collect()
}

fn field_kind_str(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Text => "text",
        FieldKind::Code => "code",
        FieldKind::Country => "country",
        FieldKind::Number => "number",
        FieldKind::Date => "date",
        FieldKind::Year => "year",
    }
}

#[derive(Serialize)]
pub struct KeyValue {
    pub label: String,
    pub value: String,
}

#[derive(Serialize)]
pub struct RecordDto {
    pub id: i64,
    pub source_file: String,
    pub fields: Vec<KeyValue>,
    pub extra: Vec<KeyValue>,
}

impl RecordDto {
    pub fn from_card(id: i64, card: RecordCard) -> Self {
        RecordDto {
            id,
            source_file: card.source_file,
            fields: card
                .fields
                .into_iter()
                .map(|(label, value)| KeyValue { label, value })
                .collect(),
            extra: card
                .extra
                .into_iter()
                .map(|(label, value)| KeyValue { label, value })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct ImportQualityDto {
    pub layout: String,
    pub header_row: u64,
    pub source_columns: u64,
    pub recognized_columns: u64,
    pub extra_columns: u64,
    pub non_empty_cells: u64,
    pub empty_cells: u64,
    pub filled_percent: f64,
    pub warnings: Vec<String>,
}

impl From<&ImportQuality> for ImportQualityDto {
    fn from(quality: &ImportQuality) -> Self {
        ImportQualityDto {
            layout: quality.layout.clone(),
            header_row: quality.header_row,
            source_columns: quality.source_columns,
            recognized_columns: quality.recognized_columns,
            extra_columns: quality.extra_columns,
            non_empty_cells: quality.non_empty_cells,
            empty_cells: quality.empty_cells,
            filled_percent: quality.filled_percent(),
            warnings: quality.warnings.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct ImportLogDto {
    pub file_name: String,
    pub total_rows: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub seconds: f64,
    pub imported_at: String,
    pub quality: ImportQualityDto,
}

impl From<&ImportLogEntry> for ImportLogDto {
    fn from(entry: &ImportLogEntry) -> Self {
        ImportLogDto {
            file_name: entry.file_name.clone(),
            total_rows: entry.total_rows,
            imported: entry.imported,
            duplicates: entry.duplicates,
            seconds: entry.seconds,
            imported_at: entry.imported_at.clone(),
            quality: ImportQualityDto::from(&entry.quality),
        }
    }
}

/// Per-file outcome of an import job.
#[derive(Serialize)]
pub struct ImportFileResultDto {
    pub file_name: String,
    pub total_rows: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub seconds: f64,
    pub error: Option<String>,
    pub cancelled: bool,
    pub skipped_duplicate_of: Option<String>,
    pub quality: ImportQualityDto,
}

impl From<&FileSummary> for ImportFileResultDto {
    fn from(summary: &FileSummary) -> Self {
        ImportFileResultDto {
            file_name: summary.file_name.clone(),
            total_rows: summary.total_rows,
            imported: summary.imported,
            duplicates: summary.duplicates,
            seconds: summary.seconds,
            error: summary.error.clone(),
            cancelled: summary.cancelled,
            skipped_duplicate_of: summary.skipped_duplicate_of.clone(),
            quality: ImportQualityDto::from(&summary.quality),
        }
    }
}

#[derive(Serialize)]
pub struct StorageDto {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub freelist_pages: u64,
    pub freelist_bytes: u64,
    pub total_file_bytes: u64,
}

impl From<&DatabaseStorageInfo> for StorageDto {
    fn from(info: &DatabaseStorageInfo) -> Self {
        StorageDto {
            database_bytes: info.database_bytes,
            wal_bytes: info.wal_bytes,
            shm_bytes: info.shm_bytes,
            freelist_pages: info.freelist_pages,
            freelist_bytes: info.freelist_bytes,
            total_file_bytes: info.total_file_bytes(),
        }
    }
}
