use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::{
    Error, Operation, Result,
    import::{ImportFileResult, ImportReport, currency_in, weight_unit_in},
    model::{IncomingColumn, NewSource, Role},
    workspace::{RecordWriter, Workspace, add_source, publish},
};

// A v2 occurrence supplies the provenance. Its compact payload may live in
// another row. Earlier full duplicates resolve against their matching owner,
// and fall back to their own payload if that owner is absent.
const EFFECTIVE_PAYLOAD: &str = "
    FROM records r
    LEFT JOIN records p ON p.id = COALESCE(
        r.canonical_id,
        CASE WHEN r.dup_first_file IS NULL THEN r.id ELSE COALESCE(
            (SELECT owner.id FROM records owner
             WHERE owner.row_hash = r.row_hash AND owner.schema_id IS r.schema_id
               AND owner.dup_first_file IS NULL AND owner.canonical_id IS NULL
             ORDER BY owner.id LIMIT 1),
            r.id
        ) END
    ) AND p.schema_id IS r.schema_id";

// Pre-schema v2 records carry the compatibility profile directly in SQLite.
const PHYSICAL_COLUMNS: &[(&str, &str, Option<&str>)] = &[
    ("clearance_time", "Час оформлення", None),
    ("customs_office", "Назва ПМО", None),
    ("declaration_type", "Тип", None),
    ("declaration_number", "Номер МД", None),
    ("declaration_date", "Дата", Some("date")),
    ("sender", "Відправник", Some("sender")),
    ("edrpou", "ЕДРПОУ", Some("company_code")),
    ("recipient", "Одержувач", Some("recipient")),
    ("item_number", "№", None),
    ("product_code", "Код товару", Some("product_code")),
    ("description", "Опис товару", Some("description")),
    ("trade_country", "Кр.торг.", None),
    ("dispatch_country", "Кр.відпр.", None),
    ("origin_country", "Кр.пох.", None),
    ("delivery_terms", "Умови пост.", None),
    ("delivery_place", "Місце пост", None),
    ("quantity", "К-ть", None),
    ("unit", "Один.вим.", None),
    ("gross_kg", "Брутто, кг.", None),
    ("net_kg", "Нетто, кг.", Some("net_weight")),
    ("declaration_weight", "Вага по МД", None),
    ("currency_control_value", "ФВ вал.контр", Some("value")),
    ("movement_feature", "Особ.перем.", None),
    ("field_43", "43", None),
    ("field_43_01", "43_01", None),
    ("rfv_usd_kg", "РФВ Дол/кг.", None),
    ("unit_weight", "Вага.один.", None),
    ("weight_difference", "Вага різн.", None),
    ("contract", "Контракт", None),
    ("field_3001", "3001", None),
    ("field_3002", "3002", None),
    ("field_9610", "9610", None),
    ("trademark", "Торг.марк.", None),
    ("rmv_net_usd_kg", "РМВ Нетто Дол/кг.", None),
    ("rmv_usd_extra_unit", "РМВ Дол/дод.од.", None),
    ("rmv_gross_usd_kg", "РМВ Брутто Дол/кг", None),
    ("zed_purpose", "Призн.Зед", None),
    ("min_base_usd_kg", "Мін.База Дол/кг.", None),
    ("min_base_difference", "Різн.мін.база", None),
    ("preferential", "пільгова", None),
    ("full_rate", "повна", None),
];

pub fn migrate_legacy(
    workspace: &Workspace,
    path: &Path,
    op: &Operation<'_>,
) -> Result<ImportReport> {
    op.check()?;
    let mut old = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    old.busy_timeout(Duration::from_secs(10))?;
    let snapshot = old.transaction()?;
    let schema_version: Option<String> = snapshot
        .query_row(
            "SELECT value FROM meta WHERE key = 'records_schema'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if schema_version.as_deref() != Some("7") {
        return Err(message(
            "Для переноса нужна база Base Search 2.2.0 со схемой записей 7",
        ));
    }
    let total =
        snapshot.query_row("SELECT COUNT(*) FROM records", [], |r| r.get::<_, i64>(0))? as u64;

    let mut target = workspace.connect()?;
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM records) OR EXISTS(SELECT 1 FROM sources)
             OR EXISTS(SELECT 1 FROM columns)",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(message("Для переноса выберите пустую базу Base Search 1.0"));
    }

    op.report(0, Some(total), "Чтение схемы базы Base Search 2.2.0");
    let physical = columns_of(&snapshot, "records")?;
    let physical: Vec<_> = PHYSICAL_COLUMNS
        .iter()
        .filter(|(name, _, _)| physical.iter().any(|item| item == name))
        .copied()
        .collect();
    let layouts = read_layouts(&snapshot, &physical, op)?;
    let mut sources = read_sources(&snapshot, &tx, &layouts, op)?;
    let synthetic_headers = discover_synthetic_headers(&snapshot, op)?;
    let mut copied = 0;
    {
        let mut writer = RecordWriter::new(&tx)?;
        let mut fields = String::new();
        for (name, _, _) in &physical {
            // Identifiers originate in the fixed compatibility profile above.
            fields.push_str(&format!(", p.{name}"));
        }
        let mut statement = snapshot.prepare(&format!(
            "SELECT r.id, r.source_file, r.schema_id, r.source_id, p.id, p.extra{fields}
             {EFFECTIVE_PAYLOAD} ORDER BY r.id"
        ))?;
        let mut records = statement.query([])?;
        while let Some(row) = records.next()? {
            op.check()?;
            let legacy_id: i64 = row.get(0)?;
            let file_name: String = row.get(1)?;
            let schema_id: Option<i64> = row.get(2)?;
            let legacy_source: Option<i64> = row.get(3)?;
            if row.get::<_, Option<i64>>(4)?.is_none() {
                return Err(message(format!(
                    "В старой базе отсутствует содержимое записи {legacy_id}"
                )));
            }
            let key = match legacy_source {
                Some(id) => SourceKey::Registered(id),
                None => SourceKey::Unregistered {
                    file_name: file_name.clone(),
                    schema_id,
                },
            };
            let transfer = match sources.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    if let Some(id) = legacy_source {
                        return Err(message(format!("В старой базе отсутствует источник {id}")));
                    }
                    let layout = match schema_id {
                        Some(id) => get_layout(&layouts, id)?.clone(),
                        None => Layout::new(
                            physical
                                .iter()
                                .enumerate()
                                .map(|(index, (_, label, semantic))| {
                                    LegacyColumn::physical(index, label, *semantic)
                                })
                                .chain(
                                    synthetic_headers
                                        .get(&file_name)
                                        .into_iter()
                                        .flatten()
                                        .map(|name| LegacyColumn::extra(name.clone())),
                                )
                                .collect(),
                            None,
                            None,
                        ),
                    };
                    entry.insert(Transfer::register(
                        &tx, key, &file_name, &file_name, "", layout,
                    )?)
                }
            };
            if transfer.schema_id != schema_id || transfer.source.file_name != file_name {
                return Err(message(format!(
                    "Источник старой записи {legacy_id} не совпадает с его описанием"
                )));
            }
            let values = transfer.layout.read_row(row)?;
            // v2 stores occurrence IDs, without original worksheet row numbers.
            writer.insert(transfer.target_id, &transfer.source, None, &values)?;
            transfer
                .expected
                .add(None, values.iter().map(|value| value.as_deref()));
            copied += 1;
            if copied % 1_000 == 0 {
                op.report(
                    copied,
                    Some(total),
                    "Копирование строк из Base Search 2.2.0",
                );
            }
        }
        op.check()?;
        writer.finish()?;
    }
    if copied != total {
        return Err(message("В старой базе изменилось число строк в снимке"));
    }
    op.report(
        copied,
        Some(total),
        "Проверка строк и контрольных сумм в новой базе",
    );
    op.check()?;
    verify_target(&tx, &sources, total, op)?;
    let mut files = BTreeMap::<String, u64>::new();
    for transfer in sources.values() {
        *files.entry(transfer.source.file_name.clone()).or_default() += transfer.expected.count;
    }
    if !sources.is_empty() {
        publish(&tx)?;
        op.check()?;
        tx.commit()?;
    }
    Ok(ImportReport {
        inserted_rows: copied,
        skipped_sheets: 0,
        files: files
            .into_iter()
            .map(|(file_name, inserted_rows)| ImportFileResult {
                file_name,
                inserted_rows,
                ..Default::default()
            })
            .collect(),
    })
}

fn columns_of(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut query = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    Ok(query
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn read_layouts(
    conn: &Connection,
    physical: &[(&str, &str, Option<&str>)],
    op: &Operation<'_>,
) -> Result<HashMap<i64, Layout>> {
    let schema_columns = columns_of(conn, "source_schemas")?;
    if schema_columns.is_empty() {
        return Ok(HashMap::new());
    }
    let detected = if schema_columns
        .iter()
        .any(|name| name == "detected_currency")
    {
        "detected_currency"
    } else {
        "NULL"
    };
    let mut schema_statement = conn.prepare(&format!(
        "SELECT id, COALESCE(fixed_currency, {detected}), fixed_weight_unit FROM source_schemas"
    ))?;
    let mut schemas = schema_statement.query([])?;
    let mut column_statement = conn.prepare(
        "SELECT raw_header, display_header, normalized_header, semantic, storage_kind, storage_name
         FROM source_columns WHERE schema_id = ?1 ORDER BY source_index",
    )?;
    let mut layouts = HashMap::new();
    while let Some(row) = schemas.next()? {
        op.check()?;
        let id: i64 = row.get(0)?;
        let mut columns = Vec::new();
        let mut schema_columns = column_statement.query([id])?;
        while let Some(column) = schema_columns.next()? {
            let name = column.get(0)?;
            let display = column.get(1)?;
            let normalized = column.get(2)?;
            let semantic = column.get::<_, Option<String>>(3)?;
            let storage = match column.get::<_, String>(4)?.as_str() {
                "source_json" => Storage::Extra,
                "schema_column" => {
                    let name: String = column.get(5)?;
                    let position = physical
                        .iter()
                        .position(|(candidate, _, _)| *candidate == name)
                        .ok_or_else(|| {
                            message(format!(
                                "В схеме {id} указан отсутствующий столбец «{name}»"
                            ))
                        })?;
                    Storage::Physical(position)
                }
                kind => {
                    return Err(message(format!(
                        "Неизвестный тип хранения «{kind}» в старой схеме {id}"
                    )));
                }
            };
            columns.push(LegacyColumn {
                name,
                display,
                normalized,
                mapping: semantic.as_deref().and_then(map_semantic),
                storage,
            });
        }
        if columns.is_empty() {
            return Err(message(format!("В старой схеме {id} отсутствуют столбцы")));
        }
        layouts.insert(id, Layout::new(columns, row.get(1)?, row.get(2)?));
    }
    Ok(layouts)
}

fn read_sources(
    conn: &Connection,
    tx: &Transaction<'_>,
    layouts: &HashMap<i64, Layout>,
    op: &Operation<'_>,
) -> Result<HashMap<SourceKey, Transfer>> {
    let mut transfers = HashMap::new();
    if columns_of(conn, "import_sources")?.is_empty() {
        return Ok(transfers);
    }
    let mut statement = conn.prepare(
        "SELECT id, schema_id, source_file, table_name, import_fingerprint FROM import_sources ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        op.check()?;
        let key = SourceKey::Registered(row.get(0)?);
        let schema_id = row.get(1)?;
        let legacy_layout = get_layout(layouts, schema_id)?.clone();
        let transfer = Transfer::register(
            tx,
            key.clone(),
            &row.get::<_, String>(2)?,
            &row.get::<_, String>(3)?,
            &row.get::<_, String>(4)?,
            legacy_layout,
        )?;
        transfers.insert(
            key,
            Transfer {
                schema_id: Some(schema_id),
                ..transfer
            },
        );
    }
    Ok(transfers)
}

fn get_layout(layouts: &HashMap<i64, Layout>, id: i64) -> Result<&Layout> {
    layouts
        .get(&id)
        .ok_or_else(|| message(format!("В старой базе отсутствует схема {id}")))
}

fn discover_synthetic_headers(
    conn: &Connection,
    op: &Operation<'_>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut files = HashMap::<String, Vec<String>>::new();
    let mut aliases = HashMap::<String, HashMap<String, usize>>::new();
    let mut statement = conn.prepare(&format!(
        "SELECT r.source_file, p.extra {EFFECTIVE_PAYLOAD}
         WHERE r.schema_id IS NULL AND p.extra IS NOT NULL ORDER BY r.id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        op.check()?;
        let file: String = row.get(0)?;
        let headers = files.entry(file.clone()).or_default();
        let known = aliases.entry(file).or_default();
        let mut occurrences = HashMap::new();
        for (key, _) in extra(row.get::<_, String>(1)?.as_str())? {
            let normalized = normalize_alias(&key);
            let count = occurrences.entry(normalized.clone()).or_insert(0);
            *count += 1;
            let maximum = known.entry(normalized).or_insert(0);
            if *maximum < *count {
                *maximum = *count;
                headers.push(key);
            }
        }
    }
    Ok(files)
}

fn normalize_alias(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn extra(raw: &str) -> Result<Vec<(String, String)>> {
    Ok(serde_json::from_str(raw)?)
}

#[derive(Clone)]
enum Storage {
    Physical(usize),
    Extra,
}

#[derive(Clone)]
struct LegacyColumn {
    name: String,
    display: String,
    normalized: String,
    mapping: Option<(Role, u8)>,
    storage: Storage,
}

impl LegacyColumn {
    fn physical(index: usize, label: &str, semantic: Option<&str>) -> Self {
        Self {
            name: label.to_owned(),
            display: label.to_owned(),
            normalized: String::new(),
            mapping: semantic.and_then(map_semantic),
            storage: Storage::Physical(index),
        }
    }

    fn extra(name: String) -> Self {
        Self {
            name: name.clone(),
            display: name,
            normalized: String::new(),
            mapping: None,
            storage: Storage::Extra,
        }
    }
}

fn map_semantic(semantic: &str) -> Option<(Role, u8)> {
    Some(match semantic {
        "date" => (Role::Date, 0),
        "company" | "recipient" => (Role::Company, 0),
        "sender" => (Role::Company, 1),
        "company_code" => (Role::Company, 2),
        "product" | "description" => (Role::Product, 0),
        "product_code" => (Role::Product, 1),
        "value" => (Role::Value, 0),
        "currency" => (Role::Currency, 0),
        "net_weight" => (Role::Weight, 0),
        "weight" => (Role::Weight, 1),
        "weight_unit" => (Role::WeightUnit, 0),
        _ => return None,
    })
}

#[derive(Clone)]
struct Layout {
    columns: Vec<LegacyColumn>,
    incoming: Vec<IncomingColumn>,
    aliases: HashMap<String, Vec<usize>>,
    fixed_currency: Option<String>,
    fixed_weight_unit: Option<String>,
}

impl Layout {
    fn new(
        columns: Vec<LegacyColumn>,
        fixed_currency: Option<String>,
        fixed_weight_unit: Option<String>,
    ) -> Self {
        let mut mapped = HashMap::<Role, (usize, u8)>::new();
        let mut aliases = HashMap::<String, Vec<usize>>::new();
        for (index, column) in columns.iter().enumerate() {
            if let Some((role, priority)) = column.mapping {
                let binding = mapped.entry(role).or_insert((index, priority));
                if priority < binding.1 {
                    *binding = (index, priority);
                }
            }
            if matches!(column.storage, Storage::Extra) {
                aliases
                    .entry(normalize_alias(&column.display))
                    .or_default()
                    .push(index);
            }
        }
        for (index, column) in columns.iter().enumerate() {
            if matches!(column.storage, Storage::Extra) {
                for alias in [&column.name, &column.normalized] {
                    if !alias.is_empty() {
                        aliases
                            .entry(normalize_alias(alias))
                            .or_insert_with(|| vec![index]);
                    }
                }
            }
        }
        let incoming = columns
            .iter()
            .enumerate()
            .map(|(index, column)| IncomingColumn {
                name: column.name.clone(),
                role: column.mapping.map(|(role, _)| role).filter(|role| {
                    mapped
                        .get(role)
                        .is_some_and(|(position, _)| *position == index)
                }),
            })
            .collect();
        let fixed_currency = fixed_currency.or_else(|| {
            let &(index, _) = mapped.get(&Role::Value)?;
            currency_in(&columns[index].display)
        });
        let fixed_weight_unit = fixed_weight_unit.or_else(|| {
            let &(index, _) = mapped.get(&Role::Weight)?;
            weight_unit_in(&columns[index].display)
        });
        Self {
            columns,
            incoming,
            aliases,
            fixed_currency,
            fixed_weight_unit,
        }
    }

    fn read_row(&self, row: &rusqlite::Row<'_>) -> Result<Vec<Option<String>>> {
        let mut values = self
            .columns
            .iter()
            .map(|column| match column.storage {
                Storage::Physical(index) => row.get(6 + index),
                Storage::Extra => Ok(None),
            })
            .collect::<rusqlite::Result<Vec<Option<String>>>>()?;
        if let Some(encoded) = row.get::<_, Option<String>>(5)? {
            for (header, value) in extra(&encoded)? {
                let column = self
                    .aliases
                    .get(&normalize_alias(&header))
                    .and_then(|indices| indices.iter().find(|&&index| values[index].is_none()))
                    .copied()
                    .ok_or_else(|| {
                        message(format!(
                            "В старой записи найден неизвестный столбец «{header}»"
                        ))
                    })?;
                values[column] = Some(value);
            }
        }
        Ok(values)
    }
}

#[derive(Hash, Eq, PartialEq, Clone)]
enum SourceKey {
    Registered(i64),
    Unregistered {
        file_name: String,
        schema_id: Option<i64>,
    },
}

struct Transfer {
    target_id: i64,
    schema_id: Option<i64>,
    source: NewSource,
    layout: Layout,
    expected: Checksum,
}

impl Transfer {
    fn register(
        tx: &Transaction<'_>,
        key: SourceKey,
        file_name: &str,
        sheet: &str,
        old_fingerprint: &str,
        layout: Layout,
    ) -> Result<Self> {
        let mut digest = Sha256::new();
        digest.update(b"base-search-v2-source\0");
        hash_text(&mut digest, file_name);
        hash_text(&mut digest, sheet);
        let schema_id = match key {
            SourceKey::Registered(id) => {
                digest.update(b"registered");
                digest.update(id.to_le_bytes());
                hash_text(&mut digest, old_fingerprint);
                None
            }
            SourceKey::Unregistered { schema_id, .. } => {
                digest.update(b"unregistered");
                if let Some(id) = schema_id {
                    digest.update(id.to_le_bytes());
                }
                schema_id
            }
        };
        let source = NewSource {
            file_name: file_name.to_owned(),
            sheet: sheet.to_owned(),
            fingerprint: format!("{:x}", digest.finalize()),
            columns: layout.incoming.clone(),
            fixed_currency: layout.fixed_currency.clone(),
            fixed_weight_unit: layout.fixed_weight_unit.clone(),
        };
        let target_id = add_source(tx, &source)?.ok_or_else(|| {
            message(format!(
                "В новой базе повторяется источник «{file_name}», лист «{sheet}»"
            ))
        })?;
        Ok(Self {
            target_id,
            schema_id,
            source,
            layout,
            expected: Checksum::default(),
        })
    }
}

#[derive(Default)]
struct Checksum {
    count: u64,
    digest: Sha256,
}

impl Checksum {
    fn add<'a>(&mut self, row_number: Option<i64>, values: impl Iterator<Item = Option<&'a str>>) {
        self.count += 1;
        self.digest.update(b"row\0");
        hash_optional(
            &mut self.digest,
            row_number
                .map(i64::to_le_bytes)
                .as_ref()
                .map(|bytes| bytes.as_slice()),
        );
        for value in values {
            hash_optional(&mut self.digest, value.map(str::as_bytes));
        }
    }
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

fn hash_optional(digest: &mut Sha256, value: Option<&[u8]>) {
    if let Some(value) = value {
        digest.update([1]);
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    } else {
        digest.update([0]);
    }
}

fn verify_target(
    conn: &Connection,
    transfers: &HashMap<SourceKey, Transfer>,
    total: u64,
    op: &Operation<'_>,
) -> Result<()> {
    let mut readback = BTreeMap::new();
    let mut source_statement = conn.prepare(
        "SELECT file_name, sheet, fingerprint, rows, fixed_currency, fixed_weight_unit FROM sources WHERE id = ?1",
    )?;
    let mut column_statement = conn.prepare(
        "SELECT column_id, label, role, ordinal FROM source_columns WHERE source_id = ?1 ORDER BY ordinal",
    )?;
    let source_count = conn.query_row("SELECT COUNT(*) FROM sources", [], |row| {
        row.get::<_, i64>(0)
    })?;
    if source_count as usize != transfers.len() {
        return Err(message(
            "Проверка не подтвердила количество источников в новой базе",
        ));
    }
    for transfer in transfers.values() {
        op.check()?;
        let source = &transfer.source;
        let source_matches = source_statement.query_row([transfer.target_id], |row| {
            Ok(row.get::<_, String>(0)? == source.file_name
                && row.get::<_, String>(1)? == source.sheet
                && row.get::<_, String>(2)? == source.fingerprint
                && row.get::<_, i64>(3)? as u64 == transfer.expected.count
                && row.get::<_, Option<String>>(4)? == source.fixed_currency
                && row.get::<_, Option<String>>(5)? == source.fixed_weight_unit)
        })?;
        if !source_matches {
            return Err(verification_error(source));
        }
        let mut keys: Vec<String> = Vec::with_capacity(source.columns.len());
        let mut stored = column_statement.query([transfer.target_id])?;
        for (index, column) in source.columns.iter().enumerate() {
            let row = stored.next()?.ok_or_else(|| verification_error(source))?;
            if row.get::<_, String>(1)? != column.name
                || row.get::<_, Option<String>>(2)?.as_deref() != column.role.map(Role::name)
                || row.get::<_, i64>(3)? != index as i64
            {
                return Err(verification_error(source));
            }
            keys.push(row.get(0)?);
        }
        if stored.next()?.is_some() {
            return Err(verification_error(source));
        }
        readback.insert(transfer.target_id, (keys, Checksum::default(), transfer));
    }

    let mut statement =
        conn.prepare("SELECT source_id, row_number, raw FROM records ORDER BY id")?;
    let mut records = statement.query([])?;
    let mut verified = 0;
    while let Some(row) = records.next()? {
        op.check()?;
        let source_id: i64 = row.get(0)?;
        let (keys, checksum, transfer) = readback
            .get_mut(&source_id)
            .ok_or_else(|| message("Проверка обнаружила неизвестный источник в новой базе"))?;
        let raw: String = row.get(2)?;
        let values: HashMap<String, Option<String>> = serde_json::from_str(&raw)?;
        if values.len() != keys.len() || keys.iter().any(|key| !values.contains_key(key)) {
            return Err(verification_error(&transfer.source));
        }
        checksum.add(row.get(1)?, keys.iter().map(|key| values[key].as_deref()));
        verified += 1;
        if verified % 1_000 == 0 {
            op.report(
                verified,
                Some(total),
                "Проверка контрольных сумм новой базы",
            );
        }
    }
    if verified != total {
        return Err(message(
            "Проверка не подтвердила количество строк в новой базе",
        ));
    }
    for (_, checksum, transfer) in readback.into_values() {
        if checksum.count != transfer.expected.count
            || checksum.digest.finalize() != transfer.expected.digest.clone().finalize()
        {
            return Err(verification_error(&transfer.source));
        }
    }
    Ok(())
}

fn verification_error(source: &NewSource) -> Error {
    message(format!(
        "Не совпали данные при проверке файла «{}», листа «{}»",
        source.file_name, source.sheet
    ))
}

fn message(text: impl Into<String>) -> Error {
    Error::Message(text.into())
}
