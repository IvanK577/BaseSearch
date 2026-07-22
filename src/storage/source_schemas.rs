use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::domain::table::{
    ColumnRole, ColumnStorage, ImportSource, SemanticField, SourceColumn, SourceSchema,
    SourceSchemaField, TableShape, stable_column_id,
};
use crate::schema::COLUMNS;
use crate::storage::normalize::normalize_text_key;
use crate::storage::table_shape;

pub(crate) const FINGERPRINT_VERSION: u32 = 1;

pub(crate) type SourceFieldLookup = HashMap<String, SourceSchemaField>;
pub(crate) type CompatibilityShapeFields = (TableShape, HashMap<String, Vec<String>>);

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_schemas (
             id INTEGER PRIMARY KEY,
             public_id TEXT NOT NULL UNIQUE,
             fingerprint TEXT NOT NULL UNIQUE,
             fingerprint_version INTEGER NOT NULL,
             fixed_currency TEXT,
             fixed_weight_unit TEXT,
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS source_columns (
             id INTEGER PRIMARY KEY,
             schema_id INTEGER NOT NULL,
             field_id TEXT NOT NULL UNIQUE,
             source_index INTEGER NOT NULL,
             raw_header TEXT NOT NULL,
             display_header TEXT NOT NULL,
             normalized_header TEXT NOT NULL,
             role TEXT NOT NULL,
             semantic TEXT,
             storage_kind TEXT NOT NULL,
             storage_name TEXT,
             UNIQUE(schema_id, source_index),
             FOREIGN KEY(schema_id) REFERENCES source_schemas(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS import_sources (
             id INTEGER PRIMARY KEY,
             public_id TEXT NOT NULL UNIQUE,
             schema_id INTEGER NOT NULL,
             source_file TEXT NOT NULL,
             table_name TEXT NOT NULL,
             import_fingerprint TEXT NOT NULL,
             imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             FOREIGN KEY(schema_id) REFERENCES source_schemas(id)
         );
         CREATE INDEX IF NOT EXISTS idx_source_columns_schema_order
             ON source_columns(schema_id, source_index);
         CREATE INDEX IF NOT EXISTS idx_import_sources_schema
             ON import_sources(schema_id, id);
         CREATE TRIGGER IF NOT EXISTS records_canonical_schema_insert
         BEFORE INSERT ON records
         WHEN NEW.canonical_id IS NOT NULL
          AND NOT EXISTS (
              SELECT 1 FROM records canonical
              WHERE canonical.id = NEW.canonical_id
                AND canonical.schema_id IS NEW.schema_id
          )
         BEGIN
             SELECT RAISE(ABORT, 'canonical payload belongs to another source schema');
         END;
         CREATE TRIGGER IF NOT EXISTS records_canonical_schema_update
         BEFORE UPDATE OF canonical_id, schema_id ON records
         WHEN NEW.canonical_id IS NOT NULL
          AND NOT EXISTS (
              SELECT 1 FROM records canonical
              WHERE canonical.id = NEW.canonical_id
                AND canonical.schema_id IS NEW.schema_id
          )
         BEGIN
             SELECT RAISE(ABORT, 'canonical payload belongs to another source schema');
         END;",
    )
}

pub(crate) fn register_schema(
    conn: &Connection,
    raw_headers: &[String],
    shape: &TableShape,
    fixed_values: &BTreeMap<SemanticField, String>,
) -> rusqlite::Result<SourceSchema> {
    validate_shape(raw_headers, shape)?;
    let fingerprint = schema_fingerprint(raw_headers, shape, fixed_values);
    let public_id = format!("schema_{}", &fingerprint[..32]);
    let fixed_currency = fixed_value(fixed_values, SemanticField::Currency);
    let fixed_weight_unit = fixed_value(fixed_values, SemanticField::WeightUnit);

    conn.execute_batch("SAVEPOINT register_source_schema")?;
    let result = (|| -> rusqlite::Result<SourceSchema> {
        conn.execute(
            "INSERT OR IGNORE INTO source_schemas (
                 public_id, fingerprint, fingerprint_version,
                 fixed_currency, fixed_weight_unit
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                public_id,
                fingerprint,
                FINGERPRINT_VERSION as i64,
                fixed_currency,
                fixed_weight_unit,
            ],
        )?;
        let schema_id: i64 = conn.query_row(
            "SELECT id FROM source_schemas WHERE fingerprint = ?1",
            [&fingerprint],
            |row| row.get(0),
        )?;
        let existing_columns: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_columns WHERE schema_id = ?1",
            [schema_id],
            |row| row.get(0),
        )?;
        if existing_columns == 0 {
            insert_columns(conn, schema_id, &fingerprint, raw_headers, shape)?;
        } else if existing_columns != shape.columns.len() as i64 {
            return Err(rusqlite::Error::InvalidParameterName(
                "stored source schema does not match its fingerprint".to_string(),
            ));
        }
        get_by_id(conn, schema_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    })();
    match result {
        Ok(source_schema) => {
            conn.execute_batch("RELEASE register_source_schema")?;
            Ok(source_schema)
        }
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO register_source_schema; RELEASE register_source_schema;",
            );
            Err(error)
        }
    }
}

pub(crate) fn register_import_source(
    conn: &Connection,
    schema_id: i64,
    source_file: &str,
    table_name: &str,
    import_fingerprint: &str,
) -> rusqlite::Result<ImportSource> {
    conn.execute(
        "INSERT INTO import_sources (
             public_id, schema_id, source_file, table_name, import_fingerprint
         ) VALUES ('source_' || lower(hex(randomblob(16))), ?1, ?2, ?3, ?4)",
        params![schema_id, source_file, table_name, import_fingerprint],
    )?;
    get_import_source_by_id(conn, conn.last_insert_rowid())?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub(crate) fn list(conn: &Connection) -> rusqlite::Result<Vec<SourceSchema>> {
    let mut statement = conn.prepare(
        "SELECT id, public_id, fingerprint, fingerprint_version,
                fixed_currency, fixed_weight_unit
         FROM source_schemas ORDER BY id",
    )?;
    let rows = statement.query_map([], read_schema_header)?;
    let mut schemas = Vec::new();
    for row in rows {
        let mut source_schema = row?;
        source_schema.columns = list_fields_by_schema_id(conn, source_schema.id)?;
        schemas.push(source_schema);
    }
    Ok(schemas)
}

pub(crate) fn get(conn: &Connection, public_id: &str) -> rusqlite::Result<Option<SourceSchema>> {
    let header = conn
        .query_row(
            "SELECT id, public_id, fingerprint, fingerprint_version,
                    fixed_currency, fixed_weight_unit
             FROM source_schemas WHERE public_id = ?1",
            [public_id],
            read_schema_header,
        )
        .optional()?;
    with_columns(conn, header)
}

pub(crate) fn get_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<SourceSchema>> {
    let header = conn
        .query_row(
            "SELECT id, public_id, fingerprint, fingerprint_version,
                    fixed_currency, fixed_weight_unit
             FROM source_schemas WHERE id = ?1",
            [id],
            read_schema_header,
        )
        .optional()?;
    with_columns(conn, header)
}

pub(crate) fn list_fields(
    conn: &Connection,
    schema_public_id: Option<&str>,
) -> rusqlite::Result<Vec<SourceSchemaField>> {
    match schema_public_id {
        Some(public_id) => {
            let schema_id = conn
                .query_row(
                    "SELECT id FROM source_schemas WHERE public_id = ?1",
                    [public_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            match schema_id {
                Some(schema_id) => list_fields_by_schema_id(conn, schema_id),
                None => Ok(Vec::new()),
            }
        }
        None => list_all_fields(conn),
    }
}

pub(crate) fn field_lookup(conn: &Connection) -> rusqlite::Result<SourceFieldLookup> {
    Ok(list_all_fields(conn)?
        .into_iter()
        .map(|field| (field.field_id.clone(), field))
        .collect())
}

pub(crate) fn list_import_sources(conn: &Connection) -> rusqlite::Result<Vec<ImportSource>> {
    let mut statement = conn.prepare(
        "SELECT s.id, s.public_id, s.schema_id, sc.public_id,
                s.source_file, s.table_name, s.import_fingerprint, s.imported_at
         FROM import_sources s
         JOIN source_schemas sc ON sc.id = s.schema_id
         ORDER BY s.id",
    )?;
    statement
        .query_map([], read_import_source)?
        .collect::<rusqlite::Result<Vec<_>>>()
}

pub(crate) fn get_import_source(
    conn: &Connection,
    public_id: &str,
) -> rusqlite::Result<Option<ImportSource>> {
    conn.query_row(
        "SELECT s.id, s.public_id, s.schema_id, sc.public_id,
                s.source_file, s.table_name, s.import_fingerprint, s.imported_at
         FROM import_sources s
         JOIN source_schemas sc ON sc.id = s.schema_id
         WHERE s.public_id = ?1",
        [public_id],
        read_import_source,
    )
    .optional()
}

pub(crate) fn schema_id_for_record(
    conn: &Connection,
    record_id: i64,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT schema_id FROM records WHERE id = ?1",
        [record_id],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
}

pub(crate) fn has_legacy_rows(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM records WHERE schema_id IS NULL LIMIT 1)",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
}

pub(crate) fn compatibility_shape(conn: &Connection) -> rusqlite::Result<Option<TableShape>> {
    Ok(compatibility_shape_with_fields(conn)?.map(|(shape, _)| shape))
}

/// Legacy-contract shape derived from the registered source schemas. Column
/// ids follow the historical header-based algorithm ("Value USD" ->
/// "value_usd"), and columns that repeat across schemas fold into one, so
/// saved semantics, filters, and the desktop/browser column UIs keep working
/// on the ids they always used. The returned map records which physical
/// source fields stand behind each compatibility column, so semantic edits
/// can write through to the registry.
pub(crate) fn compatibility_shape_with_fields(
    conn: &Connection,
) -> rusqlite::Result<Option<CompatibilityShapeFields>> {
    let fields = list_all_fields(conn)?;
    if fields.is_empty() {
        return Ok(None);
    }
    let mut columns: Vec<SourceColumn> = Vec::new();
    let mut backing: HashMap<String, Vec<String>> = HashMap::new();
    for field in fields {
        let incoming = SourceColumn {
            id: stable_column_id(&field.header, field.source_index),
            header: field.header.clone(),
            source_index: field.source_index,
            role: field.role,
            semantic: field.semantic,
            storage: field.storage.clone(),
        };
        let assigned = table_shape::merge_column(&mut columns, &incoming);
        backing.entry(assigned).or_default().push(field.field_id);
    }
    Ok(Some((TableShape { columns }, backing)))
}

/// Writes an analytical meaning through to the physical source fields behind
/// one compatibility column. Returns true when at least one field changed.
pub(crate) fn set_fields_semantic(
    conn: &Connection,
    field_ids: &[String],
    semantic: Option<SemanticField>,
) -> rusqlite::Result<bool> {
    let mut statement =
        conn.prepare("UPDATE source_columns SET semantic = ?1 WHERE field_id = ?2")?;
    let mut updated = false;
    for field_id in field_ids {
        updated |= statement.execute(params![semantic.map(semantic_name), field_id])? > 0;
    }
    Ok(updated)
}

pub(crate) fn schema_fingerprint(
    raw_headers: &[String],
    shape: &TableShape,
    fixed_values: &BTreeMap<SemanticField, String>,
) -> String {
    let mut digest = Sha256::new();
    hash_part(&mut digest, b"base-search-source-schema");
    hash_part(&mut digest, &FINGERPRINT_VERSION.to_le_bytes());
    for (source_index, raw_header) in raw_headers.iter().enumerate() {
        let field = shape
            .columns
            .iter()
            .find(|field| field.source_index == source_index);
        hash_part(&mut digest, &(source_index as u64).to_le_bytes());
        hash_part(
            &mut digest,
            normalize_raw_header(raw_header, source_index).as_bytes(),
        );
        match field {
            Some(field) => {
                hash_part(&mut digest, role_name(field.role).as_bytes());
                hash_part(
                    &mut digest,
                    field
                        .semantic
                        .map(semantic_name)
                        .unwrap_or("none")
                        .as_bytes(),
                );
                let (kind, name) = storage_parts(&field.storage);
                hash_part(&mut digest, kind.as_bytes());
                hash_part(&mut digest, name.unwrap_or("").as_bytes());
            }
            None => {
                hash_part(&mut digest, b"missing");
            }
        }
    }
    for semantic in [SemanticField::Currency, SemanticField::WeightUnit] {
        hash_part(&mut digest, semantic_name(semantic).as_bytes());
        hash_part(
            &mut digest,
            fixed_values
                .get(&semantic)
                .map(|value| normalize_text_key(value.trim()))
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    format!("ssf1:{:x}", digest.finalize())
}

fn validate_shape(raw_headers: &[String], shape: &TableShape) -> rusqlite::Result<()> {
    if raw_headers.len() != shape.columns.len() {
        return Err(rusqlite::Error::InvalidParameterName(
            "source headers and detected shape have different widths".to_string(),
        ));
    }
    for (expected_index, field) in shape.columns.iter().enumerate() {
        if field.source_index != expected_index {
            return Err(rusqlite::Error::InvalidParameterName(
                "source schema columns must preserve source order".to_string(),
            ));
        }
        validate_storage(&field.storage)?;
    }
    Ok(())
}

fn validate_storage(storage: &ColumnStorage) -> rusqlite::Result<()> {
    if let ColumnStorage::SchemaColumn(name) = storage
        && !COLUMNS.iter().any(|column| column.name == name)
    {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "unknown canonical storage column: {name}"
        )));
    }
    Ok(())
}

fn insert_columns(
    conn: &Connection,
    schema_id: i64,
    fingerprint: &str,
    raw_headers: &[String],
    shape: &TableShape,
) -> rusqlite::Result<()> {
    let mut statement = conn.prepare_cached(
        "INSERT INTO source_columns (
             schema_id, field_id, source_index, raw_header, display_header,
             normalized_header, role, semantic, storage_kind, storage_name
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for field in &shape.columns {
        let raw_header = raw_headers
            .get(field.source_index)
            .map(String::as_str)
            .unwrap_or("");
        let normalized_header = normalize_raw_header(raw_header, field.source_index);
        let field_id = stable_field_id(fingerprint, field.source_index, &normalized_header);
        let (storage_kind, storage_name) = storage_parts(&field.storage);
        statement.execute(params![
            schema_id,
            field_id,
            field.source_index as i64,
            raw_header,
            field.header,
            normalized_header,
            role_name(field.role),
            field.semantic.map(semantic_name),
            storage_kind,
            storage_name,
        ])?;
    }
    Ok(())
}

fn list_all_fields(conn: &Connection) -> rusqlite::Result<Vec<SourceSchemaField>> {
    let mut statement = conn.prepare(
        "SELECT field_id, schema_id, source_index, raw_header, display_header,
                normalized_header, role, semantic, storage_kind, storage_name
         FROM source_columns ORDER BY schema_id, source_index",
    )?;
    statement
        .query_map([], read_field)?
        .collect::<rusqlite::Result<Vec<_>>>()
}

pub(crate) fn list_fields_by_schema_id(
    conn: &Connection,
    schema_id: i64,
) -> rusqlite::Result<Vec<SourceSchemaField>> {
    let mut statement = conn.prepare(
        "SELECT field_id, schema_id, source_index, raw_header, display_header,
                normalized_header, role, semantic, storage_kind, storage_name
         FROM source_columns WHERE schema_id = ?1 ORDER BY source_index",
    )?;
    statement
        .query_map([schema_id], read_field)?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn read_field(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceSchemaField> {
    let role_raw: String = row.get(6)?;
    let semantic_raw: Option<String> = row.get(7)?;
    let storage_kind: String = row.get(8)?;
    let storage_name: Option<String> = row.get(9)?;
    Ok(SourceSchemaField {
        field_id: row.get(0)?,
        schema_id: row.get(1)?,
        source_index: row.get::<_, i64>(2)?.max(0) as usize,
        raw_header: row.get(3)?,
        header: row.get(4)?,
        normalized_header: row.get(5)?,
        role: parse_role(&role_raw)?,
        semantic: semantic_raw.as_deref().map(parse_semantic).transpose()?,
        storage: parse_storage(&storage_kind, storage_name)?,
    })
}

fn read_schema_header(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceSchema> {
    Ok(SourceSchema {
        id: row.get(0)?,
        public_id: row.get(1)?,
        fingerprint: row.get(2)?,
        fingerprint_version: row.get::<_, i64>(3)?.max(0) as u32,
        fixed_currency: row.get(4)?,
        fixed_weight_unit: row.get(5)?,
        columns: Vec::new(),
    })
}

fn with_columns(
    conn: &Connection,
    source_schema: Option<SourceSchema>,
) -> rusqlite::Result<Option<SourceSchema>> {
    source_schema
        .map(|mut source_schema| {
            source_schema.columns = list_fields_by_schema_id(conn, source_schema.id)?;
            Ok(source_schema)
        })
        .transpose()
}

fn get_import_source_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<ImportSource>> {
    conn.query_row(
        "SELECT s.id, s.public_id, s.schema_id, sc.public_id,
                s.source_file, s.table_name, s.import_fingerprint, s.imported_at
         FROM import_sources s
         JOIN source_schemas sc ON sc.id = s.schema_id
         WHERE s.id = ?1",
        [id],
        read_import_source,
    )
    .optional()
}

fn read_import_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportSource> {
    Ok(ImportSource {
        id: row.get(0)?,
        public_id: row.get(1)?,
        schema_id: row.get(2)?,
        schema_public_id: row.get(3)?,
        source_file: row.get(4)?,
        table_name: row.get(5)?,
        import_fingerprint: row.get(6)?,
        imported_at: row.get(7)?,
    })
}

fn stable_field_id(fingerprint: &str, source_index: usize, normalized_header: &str) -> String {
    let mut digest = Sha256::new();
    hash_part(&mut digest, b"base-search-source-field");
    hash_part(&mut digest, fingerprint.as_bytes());
    hash_part(&mut digest, &(source_index as u64).to_le_bytes());
    hash_part(&mut digest, normalized_header.as_bytes());
    let encoded = format!("{:x}", digest.finalize());
    format!("field_{}", &encoded[..32])
}

fn normalize_raw_header(header: &str, source_index: usize) -> String {
    let collapsed = header.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalize_text_key(&collapsed);
    if normalized.is_empty() {
        format!("column_{}", source_index + 1)
    } else {
        normalized
    }
}

fn fixed_value(
    fixed_values: &BTreeMap<SemanticField, String>,
    semantic: SemanticField,
) -> Option<String> {
    fixed_values
        .get(&semantic)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn hash_part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn storage_parts(storage: &ColumnStorage) -> (&'static str, Option<&str>) {
    match storage {
        ColumnStorage::SourceJson => ("source_json", None),
        ColumnStorage::SchemaColumn(name) => ("schema_column", Some(name.as_str())),
    }
}

fn parse_storage(kind: &str, name: Option<String>) -> rusqlite::Result<ColumnStorage> {
    match kind {
        "source_json" if name.is_none() => Ok(ColumnStorage::SourceJson),
        "schema_column" => {
            let name = name.ok_or_else(|| {
                rusqlite::Error::InvalidParameterName(
                    "source schema column has no canonical storage name".to_string(),
                )
            })?;
            let storage = ColumnStorage::SchemaColumn(name);
            validate_storage(&storage)?;
            Ok(storage)
        }
        _ => Err(rusqlite::Error::InvalidParameterName(format!(
            "unknown source schema storage kind: {kind}"
        ))),
    }
}

fn role_name(role: ColumnRole) -> &'static str {
    match role {
        ColumnRole::Text => "text",
        ColumnRole::Number => "number",
        ColumnRole::Date => "date",
        ColumnRole::Year => "year",
        ColumnRole::Country => "country",
        ColumnRole::Code => "code",
        ColumnRole::Identifier => "identifier",
        ColumnRole::Money => "money",
        ColumnRole::Weight => "weight",
    }
}

fn parse_role(value: &str) -> rusqlite::Result<ColumnRole> {
    match value {
        "text" => Ok(ColumnRole::Text),
        "number" => Ok(ColumnRole::Number),
        "date" => Ok(ColumnRole::Date),
        "year" => Ok(ColumnRole::Year),
        "country" => Ok(ColumnRole::Country),
        "code" => Ok(ColumnRole::Code),
        "identifier" => Ok(ColumnRole::Identifier),
        "money" => Ok(ColumnRole::Money),
        "weight" => Ok(ColumnRole::Weight),
        _ => Err(rusqlite::Error::InvalidParameterName(format!(
            "unknown source schema role: {value}"
        ))),
    }
}

fn semantic_name(semantic: SemanticField) -> &'static str {
    match semantic {
        SemanticField::Date => "date",
        SemanticField::DeclarationNumber => "declaration_number",
        SemanticField::CompanyCode => "company_code",
        SemanticField::Sender => "sender",
        SemanticField::Recipient => "recipient",
        SemanticField::ProductCode => "product_code",
        SemanticField::Description => "description",
        SemanticField::Trademark => "trademark",
        SemanticField::Country => "country",
        SemanticField::OriginCountry => "origin_country",
        SemanticField::DispatchCountry => "dispatch_country",
        SemanticField::TradeCountry => "trade_country",
        SemanticField::Quantity => "quantity",
        SemanticField::NetWeight => "net_weight",
        SemanticField::GrossWeight => "gross_weight",
        SemanticField::Value => "value",
        SemanticField::Currency => "currency",
        SemanticField::WeightUnit => "weight_unit",
    }
}

fn parse_semantic(value: &str) -> rusqlite::Result<SemanticField> {
    match value {
        "date" => Ok(SemanticField::Date),
        "declaration_number" => Ok(SemanticField::DeclarationNumber),
        "company_code" => Ok(SemanticField::CompanyCode),
        "sender" => Ok(SemanticField::Sender),
        "recipient" => Ok(SemanticField::Recipient),
        "product_code" => Ok(SemanticField::ProductCode),
        "description" => Ok(SemanticField::Description),
        "trademark" => Ok(SemanticField::Trademark),
        "country" => Ok(SemanticField::Country),
        "origin_country" => Ok(SemanticField::OriginCountry),
        "dispatch_country" => Ok(SemanticField::DispatchCountry),
        "trade_country" => Ok(SemanticField::TradeCountry),
        "quantity" => Ok(SemanticField::Quantity),
        "net_weight" => Ok(SemanticField::NetWeight),
        "gross_weight" => Ok(SemanticField::GrossWeight),
        "value" => Ok(SemanticField::Value),
        "currency" => Ok(SemanticField::Currency),
        "weight_unit" => Ok(SemanticField::WeightUnit),
        _ => Err(rusqlite::Error::InvalidParameterName(format!(
            "unknown source schema semantic: {value}"
        ))),
    }
}
