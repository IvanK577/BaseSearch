use std::{
    collections::HashSet,
    fs::File,
    io::{BufReader, ErrorKind, Read, Seek},
    path::Path,
};

use calamine::{Cell, Data, DataRef, Dimensions, Reader, SheetType, Sheets, open_workbook_from_rs};
use rusqlite::{Connection, ErrorCode, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Error, Operation, Result,
    model::{IncomingColumn, NewSource, Role},
    workspace::{RecordWriter, Workspace, add_source, publish},
};

const PREVIEW_ROWS: usize = 5;
const MAX_SHEET_COLUMNS: usize = 16_384;
const PROGRESS_ROWS: u64 = 1_000;
const HASH_PROGRESS_BYTES: u64 = 32 * 1024 * 1024;
const DELIMITED_KEY: &str = "table";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub path: String,
    pub file_name: String,
    pub sheets: Vec<PreviewSheet>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSheet {
    pub key: String,
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub suggested_roles: Vec<Option<Role>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileRequest {
    pub path: String,
    pub sheets: Vec<ImportSheetRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSheetRequest {
    pub key: String,
    pub headers: Vec<String>,
    pub roles: Vec<Option<Role>>,
    pub fixed_currency: Option<String>,
    pub fixed_weight_unit: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub inserted_rows: u64,
    pub skipped_sheets: u64,
    pub files: Vec<ImportFileResult>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileResult {
    pub file_name: String,
    pub inserted_rows: u64,
    pub skipped_sheets: u64,
    pub error: Option<String>,
}

pub fn preview_file(path: &Path) -> Result<Preview> {
    let file_name = file_name(path);
    let mut file = File::open(path)?;
    let mut input = Input::open(path, &mut file)?;
    let mut sheets = Vec::new();
    for (key, name) in input.sheets(&file_name) {
        let mut table = PreviewSheet {
            key,
            name,
            headers: Vec::new(),
            rows: Vec::new(),
            suggested_roles: Vec::new(),
        };
        input.walk_rows(&table.key, |width, _, mut row| {
            if table.headers.is_empty() {
                table.headers = headers(row, width);
            } else {
                fit_row(&mut row, table.headers.len())?;
                table.rows.push(row);
            }
            Ok(table.rows.len() < PREVIEW_ROWS)
        })?;
        table.suggested_roles = suggest_roles(&table.headers);
        sheets.push(table);
    }
    Ok(Preview {
        path: path.to_string_lossy().into_owned(),
        file_name,
        sheets,
    })
}

pub fn import_files(
    workspace: &Workspace,
    files: &[ImportFileRequest],
    op: &Operation<'_>,
) -> Result<ImportReport> {
    op.check()?;
    let mut conn = workspace.connect()?;
    import_batch(&mut conn, files, op)
}

fn import_batch(
    conn: &mut Connection,
    files: &[ImportFileRequest],
    op: &Operation<'_>,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    for request in files {
        op.check()?;
        let path = Path::new(&request.path);
        let file_name = file_name(path);
        let result = match import_one(
            conn,
            path,
            &file_name,
            &request.sheets,
            report.inserted_rows,
            op,
        ) {
            Ok(result) => result,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ImportFileResult {
                file_name,
                error: Some(import_error(&error)),
                ..Default::default()
            },
        };
        report.inserted_rows += result.inserted_rows;
        report.skipped_sheets += result.skipped_sheets;
        report.files.push(result);
    }
    Ok(report)
}

fn import_error(error: &Error) -> String {
    let storage_full = match error {
        Error::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => {
            failure.code == ErrorCode::DiskFull
        }
        Error::Io(error) => error.kind() == ErrorKind::StorageFull,
        Error::Csv(error) => {
            matches!(error.kind(), csv::ErrorKind::Io(error) if error.kind() == ErrorKind::StorageFull)
        }
        _ => false,
    };
    if storage_full {
        "Не хватает места на диске для импорта файла.".to_owned()
    } else {
        error.to_string()
    }
}

fn import_one(
    conn: &mut Connection,
    path: &Path,
    file_name: &str,
    selected: &[ImportSheetRequest],
    previously_inserted: u64,
    op: &Operation<'_>,
) -> Result<ImportFileResult> {
    let mut result = ImportFileResult {
        file_name: file_name.to_owned(),
        ..Default::default()
    };
    if selected.is_empty() {
        return Ok(result);
    }

    op.report(
        previously_inserted,
        None,
        format!("Чтение файла «{file_name}»"),
    );
    let mut file = File::open(path)?;
    let initial = file.metadata()?;
    let file_hash = hash_file(
        &mut file,
        initial.len(),
        |bytes, total| {
            op.report(
                previously_inserted,
                None,
                format!("Проверка SHA-256 файла «{file_name}» ({bytes} из {total} байт)"),
            );
        },
        op,
    )?;
    let mut input = Input::open(path, &mut file)?;
    let sheets = input.sheets(file_name);
    let mut visited = HashSet::new();
    for sheet in selected {
        if !visited.insert(&sheet.key) {
            return Err(message("Один и тот же лист выбран несколько раз"));
        }
        if !sheets.iter().any(|(key, _)| *key == sheet.key) {
            return Err(message(format!("Лист «{}» отсутствует в файле", sheet.key)));
        }
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut new_sources = 0;
    {
        let mut writer = RecordWriter::new(&tx)?;
        for (key, name) in sheets {
            let Some(selection) = selected.iter().find(|sheet| sheet.key == key) else {
                continue;
            };
            op.check()?;
            let fingerprint = sheet_fingerprint(&file_hash, &key);
            let mut source: Option<(i64, NewSource)> = None;
            let mut found_header = false;
            let progress_message = format!("Импорт «{file_name}», лист «{name}»");
            input.walk_rows(&key, |width, number, mut row| {
                op.check()?;
                if let Some((id, source)) = &source {
                    fit_row(&mut row, source.columns.len())?;
                    writer.insert(*id, source, Some(number), &row)?;
                    result.inserted_rows += 1;
                    if result.inserted_rows.is_multiple_of(PROGRESS_ROWS) {
                        op.report(
                            previously_inserted + result.inserted_rows,
                            None,
                            &progress_message,
                        );
                    }
                    return Ok(true);
                }
                let header = headers(row, width);
                found_header = true;
                let mapped = import_source(file_name, &name, &fingerprint, header, selection)?;
                match add_source(&tx, &mapped)? {
                    Some(id) => {
                        source = Some((id, mapped));
                        new_sources += 1;
                        Ok(true)
                    }
                    None => {
                        result.skipped_sheets += 1;
                        op.report(
                            previously_inserted + result.inserted_rows,
                            None,
                            format!(
                                "Лист «{name}» из файла «{file_name}» уже импортирован (совпадает SHA-256 файла и ключ листа)"
                            ),
                        );
                        Ok(false)
                    }
                }
            })?;
            if !found_header && !selection.headers.is_empty() {
                return Err(changed_headers(&name));
            }
        }
        op.check()?;
        writer.finish()?;
    }
    drop(input);
    let final_metadata = file.metadata()?;
    if initial.len() != final_metadata.len()
        || initial.modified().ok() != final_metadata.modified().ok()
    {
        return Err(message(
            "Файл изменился во время импорта. Повторите импорт.",
        ));
    }
    op.check()?;
    if new_sources > 0 {
        publish(&tx)?;
        op.check()?;
        tx.commit()?;
    }
    op.report(
        previously_inserted + result.inserted_rows,
        None,
        format!("Обработан файл «{file_name}»"),
    );
    Ok(result)
}

fn import_source(
    file_name: &str,
    sheet: &str,
    fingerprint: &str,
    headers: Vec<String>,
    selection: &ImportSheetRequest,
) -> Result<NewSource> {
    if headers != selection.headers || headers.len() != selection.roles.len() {
        return Err(changed_headers(sheet));
    }
    let mut assigned = HashSet::new();
    if selection
        .roles
        .iter()
        .flatten()
        .any(|role| !assigned.insert(*role))
    {
        return Err(message(format!(
            "Назначьте каждую роль одному столбцу листа «{sheet}»"
        )));
    }
    let columns = headers
        .into_iter()
        .zip(selection.roles.iter().copied())
        .map(|(name, role)| IncomingColumn { name, role })
        .collect::<Vec<_>>();
    let fixed_currency = meaningful(&selection.fixed_currency).or_else(|| {
        columns
            .iter()
            .find(|c| c.role == Some(Role::Value))
            .and_then(|c| currency_in(&c.name))
    });
    let fixed_weight_unit = meaningful(&selection.fixed_weight_unit).or_else(|| {
        columns
            .iter()
            .find(|c| c.role == Some(Role::Weight))
            .and_then(|c| weight_unit_in(&c.name))
    });
    Ok(NewSource {
        file_name: file_name.to_owned(),
        sheet: sheet.to_owned(),
        fingerprint: fingerprint.to_owned(),
        columns,
        fixed_currency,
        fixed_weight_unit,
    })
}

fn changed_headers(sheet: &str) -> Error {
    message(format!(
        "Изменились столбцы листа «{sheet}». Откройте предварительный просмотр ещё раз."
    ))
}

fn meaningful(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn hash_file(
    file: &mut File,
    total: u64,
    mut progress: impl FnMut(u64, u64),
    op: &Operation<'_>,
) -> Result<sha2::digest::Output<Sha256>> {
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 256 * 1024];
    let mut processed = 0;
    let mut reported = 0;
    loop {
        op.check()?;
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
        processed += read as u64;
        if processed - reported >= HASH_PROGRESS_BYTES {
            progress(processed, total);
            reported = processed;
        }
    }
    file.rewind()?;
    Ok(hasher.finalize())
}

fn sheet_fingerprint(file_hash: &[u8], key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"base-search-sheet-v1\0");
    hasher.update(file_hash);
    hasher.update(key.as_bytes());
    format!("{:x}", hasher.finalize())
}

enum Input<'a> {
    Delimited { file: &'a mut File, separator: u8 },
    Workbook(Box<Sheets<BufReader<&'a mut File>>>),
}

type RawRow = Vec<Option<String>>;

impl<'a> Input<'a> {
    fn open(path: &Path, file: &'a mut File) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let workbook = match extension.as_str() {
            "csv" | "tsv" => {
                let separator = if extension == "tsv" {
                    b'\t'
                } else {
                    csv_separator(file)?
                };
                return Ok(Self::Delimited { file, separator });
            }
            "xlsx" | "xlsm" => {
                Sheets::Xlsx(open_workbook_from_rs(BufReader::new(file)).map_err(message)?)
            }
            "xlsb" => Sheets::Xlsb(open_workbook_from_rs(BufReader::new(file)).map_err(message)?),
            "xls" => Sheets::Xls(open_workbook_from_rs(BufReader::new(file)).map_err(message)?),
            "ods" => Sheets::Ods(open_workbook_from_rs(BufReader::new(file)).map_err(message)?),
            _ => {
                return Err(message(
                    "Выберите файл XLSX, XLSM, XLSB, XLS, ODS, CSV или TSV",
                ));
            }
        };
        Ok(Self::Workbook(Box::new(workbook)))
    }

    fn sheets(&self, file_name: &str) -> Vec<(String, String)> {
        match self {
            Self::Delimited { .. } => vec![(DELIMITED_KEY.to_owned(), file_name.to_owned())],
            Self::Workbook(workbook) => workbook
                .sheets_metadata()
                .iter()
                .filter(|sheet| sheet.typ == SheetType::WorkSheet)
                .map(|sheet| (sheet.name.clone(), sheet.name.clone()))
                .collect(),
        }
    }

    fn walk_rows(
        &mut self,
        sheet: &str,
        mut visit: impl FnMut(usize, u64, RawRow) -> Result<bool>,
    ) -> Result<()> {
        match self {
            Self::Delimited { file, separator } => {
                file.rewind()?;
                let mut reader = delimited(*separator).from_reader(&mut **file);
                let mut row = csv::StringRecord::new();
                while reader.read_record(&mut row)? {
                    let number = row.position().map_or(1, csv::Position::line);
                    let values = row.iter().map(|value| Some(value.to_owned())).collect();
                    if !visit(0, number, values)? {
                        break;
                    }
                }
                Ok(())
            }
            Self::Workbook(workbook) => match workbook.as_mut() {
                Sheets::Xlsx(workbook) => {
                    let mut cells = workbook.worksheet_cells_reader(sheet).map_err(message)?;
                    walk_cells(
                        cells.dimensions(),
                        || cells.next_cell().map_err(message),
                        visit,
                    )
                }
                Sheets::Xlsb(workbook) => {
                    let mut cells = workbook.worksheet_cells_reader(sheet).map_err(message)?;
                    walk_cells(
                        cells.dimensions(),
                        || cells.next_cell().map_err(message),
                        visit,
                    )
                }
                workbook => {
                    // Calamine loads XLS and ODS worksheets as in-memory ranges.
                    let range = workbook.worksheet_range(sheet).map_err(message)?;
                    let Some((first_row, _)) = range.start() else {
                        return Ok(());
                    };
                    let width = checked_width(range.width())?;
                    for (offset, row) in range.rows().enumerate() {
                        let row: RawRow = row.iter().map(cell_value).collect();
                        if row.iter().any(Option::is_some)
                            && !visit(width, u64::from(first_row) + offset as u64 + 1, row)?
                        {
                            break;
                        }
                    }
                    Ok(())
                }
            },
        }
    }
}

fn delimited(separator: u8) -> csv::ReaderBuilder {
    let mut builder = csv::ReaderBuilder::new();
    builder
        .delimiter(separator)
        .has_headers(false)
        .flexible(true);
    builder
}

fn csv_separator(file: &mut File) -> Result<u8> {
    let mut strongest = (0, b',');
    for separator in [b',', b';', b'\t'] {
        file.rewind()?;
        let mut reader = delimited(separator).from_reader(&mut *file);
        let mut row = csv::ByteRecord::new();
        if reader.read_byte_record(&mut row)? && row.len() > strongest.0 {
            strongest = (row.len(), separator);
        }
    }
    file.rewind()?;
    Ok(strongest.1)
}

fn walk_cells<'a>(
    dimensions: Dimensions,
    mut next: impl FnMut() -> Result<Option<Cell<DataRef<'a>>>>,
    mut visit: impl FnMut(usize, u64, RawRow) -> Result<bool>,
) -> Result<()> {
    let mut first_column = dimensions.start.1;
    let mut width =
        checked_width(dimensions.end.1.saturating_sub(dimensions.start.1) as usize + 1)?;
    let mut current: Option<u32> = None;
    let mut values = Vec::new();
    while let Some(cell) = next()? {
        let (row, column) = cell.get_position();
        let data = Data::from(cell.get_value().clone());
        let Some(value) = cell_value(&data) else {
            continue;
        };
        if let Some(previous) = current
            && previous != row
            && !visit(width, u64::from(previous) + 1, std::mem::take(&mut values))?
        {
            return Ok(());
        }
        // Some writers omit or understate worksheet dimensions.
        if column < first_column {
            if current.is_some_and(|previous| previous != row) {
                return Err(message(
                    "В листе есть данные левее первого столбца заголовка",
                ));
            }
            let shift = (first_column - column) as usize;
            width = checked_width(width + shift)?;
            values.splice(0..0, std::iter::repeat_n(None, shift));
            first_column = column;
        }
        let index = (column - first_column) as usize;
        checked_width(index + 1)?;
        if values.len() <= index {
            values.resize(index + 1, None);
        }
        values[index] = Some(value);
        current = Some(row);
    }
    if let Some(row) = current {
        visit(width, u64::from(row) + 1, values)?;
    }
    Ok(())
}

fn cell_value(value: &Data) -> Option<String> {
    match value {
        Data::Empty => None,
        Data::DateTime(value) if value.is_datetime() => Some(value.as_datetime().map_or_else(
            || value.to_string(),
            |date_time| {
                if date_time.time() == chrono::NaiveTime::MIN {
                    date_time.date().to_string()
                } else {
                    date_time.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
                }
            },
        )),
        _ => Some(value.to_string()),
    }
}

fn checked_width(width: usize) -> Result<usize> {
    if width > MAX_SHEET_COLUMNS {
        return Err(message(
            "Превышено максимально допустимое число столбцов в листе",
        ));
    }
    Ok(width)
}

fn headers(mut row: RawRow, minimum: usize) -> Vec<String> {
    if row.len() < minimum {
        row.resize(minimum, None);
    }
    row.into_iter().map(Option::unwrap_or_default).collect()
}

fn fit_row(row: &mut RawRow, width: usize) -> Result<()> {
    if row.len() > width {
        return Err(message("В строке больше столбцов, чем в заголовке листа"));
    }
    row.resize(width, None);
    Ok(())
}

fn suggest_roles(headers: &[String]) -> Vec<Option<Role>> {
    let candidates: Vec<_> = headers.iter().map(|name| header_role(name)).collect();
    let net_weight = headers.iter().zip(&candidates).position(|(name, role)| {
        *role == Some(Role::Weight) && normalized_words(name).any(|word| is_net_weight_token(&word))
    });
    let mut assigned = HashSet::new();
    candidates
        .into_iter()
        .enumerate()
        .map(|(index, role)| {
            let role = role?;
            if role == Role::Weight && net_weight.is_some_and(|preferred| preferred != index) {
                return None;
            }
            assigned.insert(role).then_some(role)
        })
        .collect()
}

fn header_role(name: &str) -> Option<Role> {
    let name = normalized_words(name).collect::<Vec<_>>().join(" ");
    match name.as_str() {
        "date"
        | "declaration date"
        | "order date"
        | "invoice date"
        | "дата"
        | "дата мд"
        | "дата оформлення" => Some(Role::Date),
        "company"
        | "company name"
        | "customer"
        | "vendor"
        | "supplier"
        | "recipient"
        | "sender"
        | "компания"
        | "компанія"
        | "одержувач"
        | "получатель"
        | "постачальник"
        | "поставщик"
        | "контрагент"
        | "відправник"
        | "отправитель" => Some(Role::Company),
        "product"
        | "product name"
        | "product code"
        | "description"
        | "item"
        | "commodity"
        | "sku"
        | "товар"
        | "код товару"
        | "код товара"
        | "опис товару"
        | "описание товара"
        | "наименование товара"
        | "назва товару"
        | "артикул"
        | "уктзед" => Some(Role::Product),
        "value"
        | "amount"
        | "total amount"
        | "invoice amount"
        | "invoice value"
        | "сума"
        | "сумма"
        | "стоимость"
        | "вартість"
        | "фв вал контр" => Some(Role::Value),
        "currency" | "currency code" | "валюта" | "код валюти" | "код валюты" => {
            Some(Role::Currency)
        }
        "weight" | "netto" | "вага" | "вес" | "нетто" | "вага по мд" => {
            Some(Role::Weight)
        }
        "weight unit" | "mass unit" | "одиниця ваги" | "единица веса" => {
            Some(Role::WeightUnit)
        }
        _ if currency_in(&name).is_some()
            && name.split_whitespace().any(|word| {
                matches!(
                    word,
                    "value" | "amount" | "total" | "сума" | "сумма" | "вартість" | "стоимость"
                )
            }) =>
        {
            Some(Role::Value)
        }
        _ if weight_header(&name) => Some(Role::Weight),
        _ => None,
    }
}

fn is_net_weight_token(word: &str) -> bool {
    matches!(word, "net" | "netto" | "нетто")
}

fn weight_header(name: &str) -> bool {
    let words = name.split_whitespace();
    if words
        .clone()
        .any(|word| matches!(word, "gross" | "brutto" | "брутто"))
    {
        return false;
    }
    let mentions_weight = words
        .clone()
        .any(|word| matches!(word, "weight" | "вага" | "вес"));
    let mentions_net = words.clone().any(is_net_weight_token);
    (mentions_weight && mentions_net)
        || ((mentions_weight || mentions_net) && weight_unit_in(name).is_some())
}

fn normalized_words(header: &str) -> impl Iterator<Item = String> + '_ {
    header
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

pub(crate) fn currency_in(header: &str) -> Option<String> {
    normalized_words(header).find_map(|word| match word.as_str() {
        "usd" | "eur" | "uah" | "gbp" | "cny" | "jpy" | "chf" | "pln" | "cad" | "aud" | "sek"
        | "nok" => Some(word.to_ascii_uppercase()),
        _ => None,
    })
}

pub(crate) fn weight_unit_in(header: &str) -> Option<String> {
    normalized_words(header).find_map(|word| {
        match word.as_str() {
            "kg" | "kgs" | "кг" | "kilogram" | "kilograms" => Some("kg"),
            "gram" | "grams" | "gm" | "gr" | "гр" => Some("g"),
            "ton" | "tons" | "tonne" | "tonnes" | "metricton" | "mt" => Some("tonne"),
            "lb" | "lbs" | "pound" | "pounds" => Some("lb"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

fn message(error: impl ToString) -> Error {
    Error::Message(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use rusqlite::hooks::Action;

    use super::*;

    fn select_csv(path: &Path) -> ImportFileRequest {
        ImportFileRequest {
            path: path.to_str().unwrap().to_owned(),
            sheets: vec![ImportSheetRequest {
                key: DELIMITED_KEY.to_owned(),
                headers: vec!["Product".to_owned()],
                roles: vec![Some(Role::Product)],
                fixed_currency: None,
                fixed_weight_unit: None,
            }],
        }
    }

    #[test]
    fn disk_full_during_file_rolls_back_attempt_and_preserves_committed_rows_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.sqlite");
        let prior = dir.path().join("committed.csv");
        let oversized = dir.path().join("oversized.csv");
        fs::write(&prior, "Product\n0007\n").unwrap();
        fs::write(
            &oversized,
            format!("Product\npartial\n{}\n", "x".repeat(128 * 1024)),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let operation = Operation::new(&cancel, &|_| {});
        let workspace = Workspace::open(&path).unwrap();
        let first = import_files(&workspace, &[select_csv(&prior)], &operation).unwrap();
        assert_eq!(first.inserted_rows, 1);

        {
            let mut conn = workspace.connect().unwrap();
            let pages = conn
                .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))
                .unwrap();
            conn.pragma_update(None, "max_page_count", pages + 8)
                .unwrap();
            let attempted = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&attempted);
            conn.update_hook(Some(move |action, _: &str, table: &str, _: i64| {
                if action == Action::SQLITE_INSERT && table == "records" {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }))
            .unwrap();
            let suspended = import_batch(&mut conn, &[select_csv(&oversized)], &operation).unwrap();
            assert!(attempted.load(Ordering::Relaxed) >= 1);
            assert_eq!(suspended.inserted_rows, 0);
            assert_eq!(suspended.files[0].inserted_rows, 0);
            let error = suspended.files[0].error.as_deref().unwrap();
            assert!(error.contains("места"), "{error}");
        }

        let reopened = Workspace::open(path).unwrap();
        let metadata = reopened.info().unwrap();
        assert_eq!(metadata.revision, 1);
        assert_eq!(metadata.row_count, 1);
        assert_eq!(metadata.sources.len(), 1);
        assert_eq!(metadata.sources[0].file_name, "committed.csv");
        let conn = reopened.connect().unwrap();
        let product: String = conn
            .query_row("SELECT product FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(product, "0007");
    }
}
