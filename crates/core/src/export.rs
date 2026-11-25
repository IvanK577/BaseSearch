use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::params_from_iter;
use rust_xlsxwriter::{Workbook, Worksheet, XlsxError};
use tempfile::NamedTempFile;

use crate::model::{Column, QuerySpec};
use crate::query::{Plan, RecordProjection, decode_values, snapshot};
use crate::workspace::{Workspace, columns};
use crate::{Error, Operation, Result};

const XLSX_ROWS: u64 = 1_048_575;
const XLSX_COLUMNS: usize = 16_384;
const PROGRESS_INTERVAL: u64 = 1_000;

pub fn export(
    workspace: &Workspace,
    query: &QuerySpec,
    path: &Path,
    columns: Option<&[String]>,
    op: &Operation<'_>,
) -> Result<u64> {
    export_inner(workspace, query, path, columns, op)
        .map_err(|error| op.check().err().unwrap_or(error))
}

fn export_inner(
    workspace: &Workspace,
    query: &QuerySpec,
    path: &Path,
    selected: Option<&[String]>,
    op: &Operation<'_>,
) -> Result<u64> {
    op.check()?;
    let extension = path
        .extension()
        .and_then(|text| text.to_str())
        .unwrap_or_default();
    let as_xlsx = extension.eq_ignore_ascii_case("xlsx");
    if !as_xlsx && !extension.eq_ignore_ascii_case("csv") {
        return Err(Error::Message("Выберите файл CSV или XLSX.".into()));
    }
    if let Ok(destination) = path.canonicalize()
        && workspace.path().canonicalize()? == destination
    {
        return Err(Error::Message("Выберите другой файл для экспорта.".into()));
    }

    let mut conn = workspace.connect()?;
    if let Some(cancel) = op.shared_cancel() {
        conn.progress_handler(20_000, Some(move || cancel.load(Ordering::Relaxed)))?;
    }
    let tx = conn.transaction()?;
    let (_, all_rows) = snapshot(&tx, query.revision)?;
    let plan = Plan::new(&tx, query)?;
    let columns = selection(columns(&tx)?, selected)?;
    if as_xlsx && columns.len() > XLSX_COLUMNS {
        return Err(Error::Message(format!(
            "В один лист XLSX помещается до {XLSX_COLUMNS} столбцов. Выберите меньше столбцов или CSV."
        )));
    }
    let total = plan.count(&tx, all_rows)?;
    op.check()?;
    op.report(0, Some(total), "Экспорт строк");

    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = NamedTempFile::new_in(directory)?;
    let mut output = if as_xlsx {
        let mut workbook = Workbook::new();
        workbook.set_tempdir(directory).map_err(xlsx_error)?;
        workbook.use_zip_large_file(true);
        start_sheet(&mut workbook, 0, &columns)?;
        Output::Xlsx(Box::new(workbook))
    } else {
        let mut file = temp.as_file().try_clone()?;
        file.write_all(b"\xef\xbb\xbf")?;
        let mut writer = csv::Writer::from_writer(file);
        writer.write_record(columns.iter().map(|column| excel_safe_csv(&column.label)))?;
        Output::Csv(Box::new(writer))
    };

    let (sql, args) = plan.ordered(RecordProjection::Raw);
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(&args))?;
    let mut processed = 0;
    while let Some(row) = rows.next()? {
        op.check()?;
        let values = decode_values(row, 0)?;
        let data = columns.iter().map(|column| {
            values
                .get(&column.id)
                .and_then(Option::as_deref)
                .unwrap_or_default()
        });
        match &mut output {
            Output::Csv(writer) => writer.write_record(data.map(excel_safe_csv))?,
            Output::Xlsx(workbook) => {
                let index = (processed / XLSX_ROWS) as usize;
                let position = (processed % XLSX_ROWS) as u32 + 1;
                if index > 0 && position == 1 {
                    start_sheet(workbook, index, &columns)?;
                }
                let sheet = workbook.worksheet_from_index(index).map_err(xlsx_error)?;
                write_xlsx_row(sheet, position, data)?;
            }
        }
        processed += 1;
        if processed.is_multiple_of(PROGRESS_INTERVAL) {
            op.report(processed, Some(total), "Экспорт строк");
        }
    }
    drop(rows);
    drop(statement);
    tx.commit()?;
    op.check()?;

    match output {
        Output::Csv(mut writer) => writer.flush()?,
        Output::Xlsx(mut workbook) => {
            op.report(processed, Some(total), "Сохранение книги XLSX");
            op.check()?;
            workbook
                .save_to_writer(CancellableFile {
                    file: temp.as_file_mut(),
                    cancel: op.cancel,
                })
                .map_err(xlsx_error)?;
        }
    }
    temp.as_file().sync_all()?;
    op.check()?;
    temp.persist(path).map_err(|error| error.error)?;
    op.report(processed, Some(total), "Файл сохранён");
    Ok(processed)
}

fn selection(columns: Vec<Column>, selected: Option<&[String]>) -> Result<Vec<Column>> {
    let columns = match selected {
        Some(selected) => {
            let by_id: HashMap<_, _> = columns.iter().map(|column| (&column.id, column)).collect();
            let mut seen = HashSet::with_capacity(selected.len());
            let mut projected = Vec::with_capacity(selected.len());
            for id in selected {
                if !seen.insert(id) {
                    return Err(Error::Message(format!("Поле выбрано дважды: {id}")));
                }
                let column = by_id
                    .get(id)
                    .ok_or_else(|| Error::Message(format!("Неизвестное поле экспорта: {id}")))?;
                projected.push((*column).clone());
            }
            projected
        }
        None => columns,
    };
    if columns.is_empty() {
        return Err(Error::Message(
            "Выберите хотя бы одно поле для экспорта.".into(),
        ));
    }
    Ok(columns)
}

enum Output {
    Csv(Box<csv::Writer<File>>),
    Xlsx(Box<Workbook>),
}

fn start_sheet(workbook: &mut Workbook, index: usize, columns: &[Column]) -> Result<()> {
    let sheet = workbook.add_worksheet_with_constant_memory();
    sheet
        .set_name(format!("Данные {}", index + 1))
        .map_err(xlsx_error)?;
    write_xlsx_row(sheet, 0, columns.iter().map(|column| column.label.as_str()))
}

fn write_xlsx_row<'a>(
    sheet: &mut Worksheet,
    row: u32,
    values: impl Iterator<Item = &'a str>,
) -> Result<()> {
    for (column, value) in values.enumerate() {
        sheet
            .write_string(row, column as u16, value)
            .map_err(xlsx_error)?;
    }
    Ok(())
}

fn xlsx_error(error: XlsxError) -> Error {
    Error::Message(format!("Ошибка XLSX: {error}"))
}

fn excel_safe_csv(value: &str) -> Cow<'_, [u8]> {
    let trimmed = value.trim_start();
    let first = trimmed.chars().next();
    let has_control_prefix = value.starts_with(['\t', '\r', '\n']);
    let formula = match first {
        Some('=' | '@' | '＝' | '＠' | '＋' | '－') => true,
        Some('+' | '-') => !trimmed[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b',')),
        _ => false,
    };
    if has_control_prefix || formula {
        Cow::Owned(format!("'{value}").into_bytes())
    } else {
        Cow::Borrowed(value.as_bytes())
    }
}

struct CancellableFile<'a> {
    file: &'a mut File,
    cancel: &'a AtomicBool,
}

impl CancellableFile<'_> {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Export cancelled"));
        }
        Ok(())
    }
}

impl Write for CancellableFile<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.file.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.file.flush()
    }
}

impl Seek for CancellableFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.file.seek(position)
    }
}
