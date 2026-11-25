use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::functions::FunctionFlags;
use rusqlite::{
    CachedStatement, Connection, OptionalExtension, Transaction, TransactionBehavior, params,
};
use rust_decimal::Decimal;
use serde::ser::{SerializeMap, Serializer};
use sha2::{Digest, Sha256};

use crate::model::{Column, IncomingColumn, NewSource, Role, Source, WorkspaceInfo};
use crate::{Error, Result};

const APPLICATION_ID: i64 = 0x4253_3130;
const SCHEMA_VERSION: i64 = 2;
const WRITER_CACHE_KIB: i64 = 128 * 1024;
const SCHEMA: &str = r#"
    CREATE TABLE workspace_meta (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        revision INTEGER NOT NULL,
        row_count INTEGER NOT NULL
    );
    INSERT INTO workspace_meta VALUES (1, 0, 0);

    CREATE TABLE sources (
        id INTEGER PRIMARY KEY,
        file_name TEXT NOT NULL,
        sheet TEXT NOT NULL,
        fingerprint TEXT NOT NULL UNIQUE,
        rows INTEGER NOT NULL DEFAULT 0,
        fixed_currency TEXT,
        fixed_weight_unit TEXT
    );
    CREATE TABLE columns (
        id TEXT NOT NULL UNIQUE,
        label TEXT NOT NULL,
        role TEXT
    );
    CREATE TABLE source_columns (
        source_id INTEGER NOT NULL REFERENCES sources(id),
        ordinal INTEGER NOT NULL,
        column_id TEXT NOT NULL REFERENCES columns(id),
        label TEXT NOT NULL,
        role TEXT,
        PRIMARY KEY (source_id, ordinal),
        UNIQUE (source_id, column_id)
    ) WITHOUT ROWID;
    CREATE TABLE records (
        id INTEGER PRIMARY KEY,
        source_id INTEGER NOT NULL REFERENCES sources(id),
        row_number INTEGER,
        raw TEXT NOT NULL,
        date TEXT,
        month TEXT,
        company TEXT COLLATE base_text,
        product TEXT COLLATE base_text,
        amount TEXT COLLATE base_decimal,
        currency TEXT COLLATE base_text,
        weight_kg TEXT COLLATE base_decimal,
        weight_unit TEXT COLLATE base_text,
        invalid_amount INTEGER NOT NULL,
        unknown_weight_unit INTEGER NOT NULL
    );
    CREATE INDEX records_source ON records(source_id);
    CREATE INDEX records_date ON records(date);
    CREATE INDEX records_month ON records(month);
    CREATE INDEX records_company ON records(company);
    CREATE INDEX records_product ON records(product);
    CREATE INDEX records_amount ON records(amount);
    CREATE INDEX records_currency ON records(currency);
    CREATE INDEX records_weight ON records(weight_kg);
    CREATE VIRTUAL TABLE record_text USING fts5(
        cells, content='', tokenize='unicode61 remove_diacritics 2',
        prefix='2 3 4'
    );
"#;

#[derive(Clone, Debug)]
pub struct Workspace {
    path: PathBuf,
}

impl Workspace {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let workspace = Self {
            path: if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            },
        };
        if let Some(parent) = workspace.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = workspace.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if Self::is_workspace(&tx)? {
            let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            match version {
                1 => {
                    tx.execute_batch("DROP TRIGGER IF EXISTS count_source_rows")?;
                    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                }
                SCHEMA_VERSION => {}
                _ => {
                    return Err(Error::Message(format!(
                        "Версия хранилища {version} несовместима с этой версией приложения."
                    )));
                }
            }
        } else {
            let application_id: i64 =
                tx.pragma_query_value(None, "application_id", |row| row.get(0))?;
            let populated: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
                    row.get(0)
                })?;
            if application_id != 0 || populated {
                return Err(Error::Message(format!(
                    "Файл {} не является хранилищем Base Search 1.0.",
                    workspace.path.display()
                )));
            }
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.commit()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(workspace)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_workspace(conn: &Connection) -> Result<bool> {
        let id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
        Ok(id == APPLICATION_ID)
    }

    pub fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.create_collation("base_text", |left, right| {
            if left == right {
                return Ordering::Equal;
            }
            if left.is_ascii() && right.is_ascii() {
                return left
                    .bytes()
                    .map(|byte| byte.to_ascii_lowercase())
                    .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()));
            }
            left.chars()
                .flat_map(char::to_lowercase)
                .cmp(right.chars().flat_map(char::to_lowercase))
        })?;
        conn.create_collation("base_decimal", compare_decimal)?;
        let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
        conn.create_scalar_function("base_fold", 1, flags, |args| {
            Ok(args.get::<Option<String>>(0)?.map(|text| fold_text(&text)))
        })?;
        conn.create_scalar_function("base_blank", 1, flags, |args| {
            Ok(args
                .get::<Option<String>>(0)?
                .is_none_or(|text| text.trim().is_empty()))
        })?;
        Ok(conn)
    }

    pub fn info(&self) -> Result<WorkspaceInfo> {
        let mut con = self.connect()?;
        let tx = con.transaction()?;
        let (revision, row_count) = metadata(&tx)?;
        let columns = columns(&tx)?;
        let mut statement =
            tx.prepare("SELECT id, file_name, sheet, rows FROM sources ORDER BY id")?;
        let sources = statement
            .query_map([], |row| {
                Ok(Source {
                    id: row.get(0)?,
                    file_name: row.get(1)?,
                    sheet: row.get(2)?,
                    rows: read_count(row, 3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(WorkspaceInfo {
            revision,
            row_count,
            columns,
            sources,
        })
    }
}

fn compare_decimal(left: &str, right: &str) -> Ordering {
    if left == right {
        return Ordering::Equal;
    }
    // A canonical positive integer of at most 28 digits always fits Decimal.
    // Its digit count and lexical order give the same result as numeric order.
    let canonical_integer = |text: &str| {
        !text.is_empty()
            && text.len() <= 28
            && (text.len() == 1 || !text.starts_with('0'))
            && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    if canonical_integer(left) && canonical_integer(right) {
        return left.len().cmp(&right.len()).then_with(|| left.cmp(right));
    }
    match (
        Decimal::from_str_exact(left),
        Decimal::from_str_exact(right),
    ) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

pub(crate) fn metadata(conn: &Connection) -> Result<(i64, u64)> {
    Ok(conn.query_row(
        "SELECT revision, row_count FROM workspace_meta WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, read_count(row, 1)?)),
    )?)
}

pub(crate) fn read_count(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

pub(crate) fn columns(conn: &Connection) -> Result<Vec<Column>> {
    let mut statement = conn.prepare("SELECT id, label, role FROM columns ORDER BY rowid")?;
    let columns = statement.query_map([], |row| {
        Ok(Column {
            id: row.get(0)?,
            label: row.get(1)?,
            role: row
                .get::<_, Option<String>>(2)?
                .as_deref()
                .and_then(Role::from_name),
        })
    })?;
    Ok(columns.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn fold_text(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

fn column_ids(columns: &[IncomingColumn]) -> Vec<String> {
    let mut occurrences = HashMap::new();
    columns
        .iter()
        .map(|column| {
            let name = column.name.trim().to_lowercase();
            let occurrences = occurrences.entry(name.clone()).or_insert(0_u64);
            let mut hash = Sha256::new();
            hash.update(occurrences.to_le_bytes());
            hash.update(name.as_bytes());
            *occurrences += 1;
            let hash = hash.finalize();
            hash[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        })
        .collect()
}

pub fn add_source(tx: &Transaction<'_>, source: &NewSource) -> Result<Option<i64>> {
    let source_id = tx
        .query_row(
            "INSERT INTO sources(file_name, sheet, fingerprint, fixed_currency, fixed_weight_unit)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(fingerprint) DO NOTHING RETURNING id",
            params![
                source.file_name,
                source.sheet,
                source.fingerprint,
                source.fixed_currency,
                source.fixed_weight_unit
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(source_id) = source_id else {
        return Ok(None);
    };
    let mut add_column = tx.prepare_cached(
        "INSERT INTO columns(id, label, role) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET role = COALESCE(columns.role, excluded.role)",
    )?;
    let mut assign = tx.prepare_cached(
        "INSERT INTO source_columns(source_id, ordinal, column_id, label, role)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (ordinal, (column, id)) in source
        .columns
        .iter()
        .zip(column_ids(&source.columns))
        .enumerate()
    {
        let role = column.role.map(Role::name);
        let display = if column.name.trim().is_empty() {
            format!("Столбец {}", ordinal + 1)
        } else {
            column.name.clone()
        };
        add_column.execute(params![id, display, role])?;
        assign.execute(params![source_id, ordinal as i64, id, column.name, role])?;
    }
    Ok(Some(source_id))
}

pub fn publish(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "UPDATE workspace_meta SET revision = revision + 1,
         row_count = (SELECT COALESCE(SUM(rows), 0) FROM sources) WHERE singleton = 1",
        [],
    )?;
    Ok(())
}

pub struct RecordWriter<'conn> {
    insert_record: CachedStatement<'conn>,
    insert_fts: CachedStatement<'conn>,
    update_count: CachedStatement<'conn>,
    layouts: HashMap<i64, SourceLayout>,
    search_text: String,
    raw: Vec<u8>,
}

struct SourceLayout {
    column_ids: Vec<String>,
    roles: HashMap<Role, usize>,
    fixed_currency: Option<String>,
    fixed_unit: Option<&'static str>,
    pending_rows: i64,
}

struct RawRow<'a> {
    ids: &'a [String],
    values: &'a [Option<String>],
}

impl serde::Serialize for RawRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.ids.len()))?;
        for (id, value) in self.ids.iter().zip(self.values) {
            map.serialize_entry(id, value)?;
        }
        map.end()
    }
}

impl SourceLayout {
    fn new(source: &NewSource) -> Self {
        let mut roles = HashMap::new();
        for (index, column) in source.columns.iter().enumerate() {
            if let Some(role) = column.role {
                roles.entry(role).or_insert(index);
            }
        }
        Self {
            column_ids: column_ids(&source.columns),
            roles,
            fixed_currency: source
                .fixed_currency
                .as_deref()
                .and_then(normalize_currency),
            fixed_unit: source
                .fixed_weight_unit
                .as_deref()
                .and_then(normalize_weight_unit),
            pending_rows: 0,
        }
    }
}

impl<'conn> RecordWriter<'conn> {
    pub fn new(tx: &'conn Transaction<'_>) -> Result<Self> {
        // Index writes revisit many pages. This limit is local to the writer
        // connection, while search and summary retain SQLite's smaller cache.
        tx.pragma_update(None, "cache_size", -WRITER_CACHE_KIB)?;
        Ok(Self {
            insert_record: tx.prepare_cached(
                "INSERT INTO records(
                    source_id, row_number, raw, date, month, company, product, amount,
                    currency, weight_kg, weight_unit, invalid_amount, unknown_weight_unit
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?,
            insert_fts: tx
                .prepare_cached("INSERT INTO record_text(rowid, cells) VALUES (?1, ?2)")?,
            update_count: tx.prepare_cached("UPDATE sources SET rows = rows + ?2 WHERE id = ?1")?,
            layouts: HashMap::new(),
            search_text: String::new(),
            raw: Vec::new(),
        })
    }

    pub fn insert(
        &mut self,
        source_id: i64,
        source: &NewSource,
        row_number: Option<u64>,
        values: &[Option<String>],
    ) -> Result<i64> {
        if values.len() != source.columns.len() {
            return Err(Error::Message(format!(
                "Ожидалось {} полей, получено {}.",
                source.columns.len(),
                values.len()
            )));
        }
        let row_number = row_number
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::Message("Номер строки выходит за допустимый диапазон.".into()))?;
        let layout = self
            .layouts
            .entry(source_id)
            .or_insert_with(|| SourceLayout::new(source));
        let get = |role| {
            let value = values[*layout.roles.get(&role)?].as_deref()?.trim();
            (!value.is_empty()).then_some(value)
        };

        let date = get(Role::Date).and_then(normalize_date);
        let month = date.as_deref().map(|date| &date[..7]);
        let amount_text = get(Role::Value);
        let amount = amount_text.and_then(parse_decimal);
        let currency = match get(Role::Currency) {
            Some(unit) => normalize_currency(unit),
            None => layout.fixed_currency.clone(),
        };
        let weight = get(Role::Weight).and_then(parse_decimal);
        let weight_unit = match get(Role::WeightUnit) {
            Some(unit) => normalize_weight_unit(unit),
            None => layout.fixed_unit,
        };
        let weight_kg = weight
            .zip(weight_unit)
            .and_then(|(weight, unit)| kilograms(weight, unit));

        // Serialize directly from references so ingest does not clone the row.
        self.raw.clear();
        serde_json::to_writer(
            &mut self.raw,
            &RawRow {
                ids: &layout.column_ids,
                values,
            },
        )?;
        let encoded = std::str::from_utf8(&self.raw).expect("JSON always encodes UTF-8");
        let record = self.insert_record.insert(params![
            source_id,
            row_number,
            encoded,
            date,
            month,
            get(Role::Company),
            get(Role::Product),
            amount.map(display_decimal),
            currency,
            weight_kg.map(display_decimal),
            weight_unit,
            amount_text.is_some() && amount.is_none(),
            weight.is_some() && weight_unit.is_none(),
        ])?;

        self.search_text.clear();
        for value in values.iter().flatten() {
            self.search_text.push_str(value);
            self.search_text.push('\n');
        }
        self.insert_fts.execute(params![record, self.search_text])?;
        layout.pending_rows += 1;
        Ok(record)
    }

    /// Writes each source's accumulated count before the transaction is published.
    pub fn finish(mut self) -> Result<()> {
        for (source_id, layout) in self.layouts {
            if layout.pending_rows > 0 {
                self.update_count
                    .execute(params![source_id, layout.pending_rows])?;
            }
        }
        Ok(())
    }
}

pub(crate) fn normalize_date(text: &str) -> Option<String> {
    let text = text.trim().split([' ', 'T']).next()?;
    for format in [
        "%Y-%m-%d", "%Y/%m/%d", "%d.%m.%Y", "%d/%m/%Y", "%d-%m-%Y", "%m/%d/%Y",
    ] {
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            let formatted = date.format("%Y-%m-%d").to_string();
            return (formatted.len() == 10 && formatted.as_bytes()[0].is_ascii_digit())
                .then_some(formatted);
        }
    }
    None
}

pub(crate) fn parse_decimal(text: &str) -> Option<Decimal> {
    let text = text.trim();
    if let Some((mantissa, exponent)) = text.split_once(['e', 'E']) {
        let mantissa = Decimal::from_str_exact(mantissa).ok()?.normalize();
        let exponent: i32 = exponent.parse().ok()?;
        if mantissa.is_zero() {
            return Some(Decimal::ZERO);
        }
        let scale = (mantissa.scale() as i32).checked_sub(exponent)?;
        return if scale < 0 {
            let place = 10_i128.checked_pow(scale.unsigned_abs())?;
            exact_decimal(mantissa.mantissa().checked_mul(place)?, 0)
        } else {
            exact_decimal(mantissa.mantissa(), scale as u32)
        };
    }
    let (sign, number) = if let Some(number) = text.strip_prefix(['-', '+']) {
        (&text[..1], number)
    } else {
        ("", text)
    };
    let all_digits =
        |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if all_digits(number)
        || number
            .split_once('.')
            .is_some_and(|(integer, fraction)| all_digits(integer) && all_digits(fraction))
    {
        return Decimal::from_str_exact(text).ok();
    }
    let last_comma = number.rfind(',');
    let last_dot = number.rfind('.');
    let decimal = match (last_comma, last_dot) {
        (Some(comma), Some(dot)) => Some(comma.max(dot)),
        (Some(index), None) if number.matches(',').count() == 1 => Some(index),
        (None, Some(index)) if number.matches('.').count() == 1 => Some(index),
        _ => None,
    };
    let (integer, fraction) = match decimal {
        Some(index) => (&number[..index], Some(&number[index + 1..])),
        None => (number, None),
    };
    let separator = |ch: char| {
        matches!(
            ch,
            ' ' | '\u{00a0}' | '\u{202f}' | '\u{2009}' | '\'' | '’' | '.' | ','
        )
    };
    let mut normalized = String::with_capacity(text.len());
    normalized.push_str(sign);
    if integer.is_empty() {
        fraction?;
        normalized.push('0');
    } else {
        let mut groups = integer.split(separator);
        let first = groups.next()?;
        if first.is_empty() || !first.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        normalized.push_str(first);
        for group in groups {
            if first.len() > 3
                || group.len() != 3
                || !group.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            normalized.push_str(group);
        }
    }
    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        normalized.push('.');
        normalized.push_str(fraction);
    }
    Decimal::from_str_exact(&normalized).ok()
}

pub(crate) fn display_decimal(value: Decimal) -> String {
    value.normalize().to_string()
}

pub(crate) fn exact_decimal(mut mantissa: i128, mut scale: u32) -> Option<Decimal> {
    if mantissa == 0 {
        return Some(Decimal::ZERO);
    }
    while scale > 0 && mantissa % 10 == 0 {
        mantissa /= 10;
        scale -= 1;
    }
    Decimal::try_from_i128_with_scale(mantissa, scale).ok()
}

pub(crate) fn normalize_currency(text: &str) -> Option<String> {
    let code = match text.trim().to_uppercase().as_str() {
        "$" | "US$" | "USD" | "ДОЛАР США" | "ДОЛЛАР США" => "USD".into(),
        "€" | "EUR" | "ЄВРО" | "ЕВРО" => "EUR".into(),
        "₴" | "ГРН" | "ГРН." | "UAH" | "ГРИВНЯ" | "ГРИВНА" => "UAH".into(),
        "£" | "GBP" => "GBP".into(),
        "ZŁ" | "PLN" => "PLN".into(),
        "₽" | "РУБ" | "РУБ." | "RUB" => "RUB".into(),
        "XXX" | "NAN" => return None,
        code if code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_uppercase()) => {
            code.into()
        }
        _ => return None,
    };
    Some(code)
}

pub(crate) fn normalize_weight_unit(text: &str) -> Option<&'static str> {
    match text.trim().trim_end_matches('.').to_lowercase().as_str() {
        "kg"
        | "kgs"
        | "кг"
        | "кілограм"
        | "кілограми"
        | "кілограмів"
        | "килограмм"
        | "килограммы"
        | "килограммов" => Some("kg"),
        "g" | "gr" | "г" | "гр" | "грам" | "грами" | "грамів" | "грамм" => {
            Some("g")
        }
        "mg" | "мг" => Some("mg"),
        "t" | "т" | "tonne" | "tonnes" | "метрична тонна" | "тонна" | "тонни" | "тонн" => {
            Some("t")
        }
        "lb" | "lbs" | "pound" | "pounds" | "фунт" => Some("lb"),
        "oz" | "ounce" | "ounces" | "унція" => Some("oz"),
        _ => None,
    }
}

fn kilograms(weight: Decimal, unit: &str) -> Option<Decimal> {
    let (mantissa, scale) = match unit {
        "kg" => return Some(weight),
        "g" => (1, 3),
        "mg" => (1, 6),
        "t" => (1000, 0),
        "lb" => (45_359_237, 8),
        "oz" => (28_349_523_125, 12),
        _ => return None,
    };
    let weight = weight.normalize();
    exact_decimal(
        weight.mantissa().checked_mul(mantissa)?,
        weight.scale() + scale,
    )
}
