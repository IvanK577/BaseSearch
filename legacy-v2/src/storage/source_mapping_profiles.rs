use std::collections::BTreeMap;
use std::fmt::Write as _;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::table::{ColumnRole, SemanticField};

const SIGNATURE_VERSION: &str = "smp1";
const MAX_COLUMNS: usize = 4096;
const MAX_NAME_CHARS: usize = 100;
const MAX_FIXED_VALUE_CHARS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// One source column used to identify a reusable mapping layout.
pub struct SourceMappingColumn {
    pub header: String,
    pub role: ColumnRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// A persisted mapping profile scoped to the current database workspace.
pub struct SourceMappingProfile {
    pub id: i64,
    pub name: String,
    pub signature: String,
    pub mapping: Vec<Option<SemanticField>>,
    #[serde(default)]
    pub fixed_values: BTreeMap<SemanticField, String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Validated create/update input for a source mapping profile.
pub struct SourceMappingProfileUpsert {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub name: String,
    pub signature: String,
    pub mapping: Vec<Option<SemanticField>>,
    #[serde(default)]
    pub fixed_values: BTreeMap<SemanticField, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// A stored row skipped by a collection read because its payload is invalid.
pub struct SourceMappingProfileCorruption {
    pub id: i64,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Valid profiles plus any corrupt rows safely ignored by `list` or `suggest`.
pub struct SourceMappingProfileCollection {
    pub profiles: Vec<SourceMappingProfile>,
    pub ignored_corrupt_rows: Vec<SourceMappingProfileCorruption>,
}

#[derive(Debug)]
/// Typed storage, validation, lookup, uniqueness, and corruption failures.
pub enum SourceMappingProfileError {
    Database(rusqlite::Error),
    Validation(String),
    NotFound(i64),
    NameConflict(String),
    CorruptRow { id: i64, reason: String },
}

impl std::fmt::Display for SourceMappingProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "{error}"),
            Self::Validation(message) => formatter.write_str(message),
            Self::NotFound(id) => write!(formatter, "source mapping profile {id} was not found"),
            Self::NameConflict(name) => {
                write!(
                    formatter,
                    "a source mapping profile named '{name}' already exists"
                )
            }
            Self::CorruptRow { id, reason } => {
                write!(
                    formatter,
                    "source mapping profile {id} is corrupt: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for SourceMappingProfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Validation(_)
            | Self::NotFound(_)
            | Self::NameConflict(_)
            | Self::CorruptRow { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for SourceMappingProfileError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// Builds a versioned signature from ordered headers and roles.
///
/// File and sheet names are intentionally not part of the input. Header case
/// and whitespace are normalized, while role, order, and column count remain
/// significant.
pub fn source_mapping_signature(columns: &[SourceMappingColumn]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"base-search/source-mapping-signature/smp1\0");
    hasher.update((columns.len() as u64).to_be_bytes());
    for column in columns {
        let header = normalize_header(&column.header);
        hasher.update((header.len() as u64).to_be_bytes());
        hasher.update(header.as_bytes());
        let role = role_name(column.role);
        hasher.update((role.len() as u64).to_be_bytes());
        hasher.update(role.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{SIGNATURE_VERSION}:{}:{hex}", columns.len())
}

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_mapping_profiles (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            signature TEXT NOT NULL,
            mapping_json TEXT NOT NULL,
            fixed_values_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL
                DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
            updated_at TEXT NOT NULL
                DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );",
    )?;
    if !table_has_column(conn, "source_mapping_profiles", "fixed_values_json")? {
        conn.execute_batch(
            "ALTER TABLE source_mapping_profiles
             ADD COLUMN fixed_values_json TEXT NOT NULL DEFAULT '{}';",
        )?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_source_mapping_profiles_name
             ON source_mapping_profiles(name_key);
         CREATE INDEX IF NOT EXISTS idx_source_mapping_profiles_signature
             ON source_mapping_profiles(signature);",
    )
}

pub(crate) fn list(
    conn: &Connection,
) -> Result<SourceMappingProfileCollection, SourceMappingProfileError> {
    query_collection(
        conn,
        "SELECT id, name, signature, mapping_json, fixed_values_json, created_at, updated_at
         FROM source_mapping_profiles
         ORDER BY updated_at DESC, id DESC",
        None,
    )
}

pub(crate) fn get(
    conn: &Connection,
    id: i64,
) -> Result<Option<SourceMappingProfile>, SourceMappingProfileError> {
    let mut statement = conn.prepare(
        "SELECT id, name, signature, mapping_json, fixed_values_json, created_at, updated_at
         FROM source_mapping_profiles
         WHERE id = ?1",
    )?;
    let raw = statement.query_row([id], read_raw_profile).optional()?;
    raw.map(decode_profile).transpose()
}

pub(crate) fn suggest(
    conn: &Connection,
    signature: &str,
) -> Result<SourceMappingProfileCollection, SourceMappingProfileError> {
    validate_signature(signature)?;
    query_collection(
        conn,
        "SELECT id, name, signature, mapping_json, fixed_values_json, created_at, updated_at
         FROM source_mapping_profiles
         WHERE signature = ?1
         ORDER BY updated_at DESC, id DESC",
        Some(signature),
    )
}

pub(crate) fn upsert(
    conn: &Connection,
    profile: SourceMappingProfileUpsert,
) -> Result<SourceMappingProfile, SourceMappingProfileError> {
    let validated = validate_upsert(profile)?;
    conn.execute_batch("SAVEPOINT source_mapping_profile_upsert")?;
    let result = upsert_in_savepoint(conn, &validated);
    match result {
        Ok(profile) => {
            conn.execute_batch("RELEASE source_mapping_profile_upsert")?;
            Ok(profile)
        }
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO source_mapping_profile_upsert;
                 RELEASE source_mapping_profile_upsert;",
            );
            Err(error)
        }
    }
}

pub(crate) fn delete(conn: &Connection, id: i64) -> Result<bool, SourceMappingProfileError> {
    Ok(conn.execute("DELETE FROM source_mapping_profiles WHERE id = ?1", [id])? != 0)
}

struct ValidatedUpsert {
    id: Option<i64>,
    name: String,
    name_key: String,
    signature: String,
    mapping_json: String,
    fixed_values_json: String,
}

fn validate_upsert(
    profile: SourceMappingProfileUpsert,
) -> Result<ValidatedUpsert, SourceMappingProfileError> {
    let name = validate_name(&profile.name)?;
    let expected_columns = validate_signature(&profile.signature)?;
    validate_mapping(&profile.mapping, expected_columns)?;
    let fixed_values = validate_fixed_values(profile.fixed_values)?;
    let mapping_json = serde_json::to_string(&profile.mapping)
        .map_err(|error| SourceMappingProfileError::Validation(error.to_string()))?;
    let fixed_values_json = serde_json::to_string(&fixed_values)
        .map_err(|error| SourceMappingProfileError::Validation(error.to_string()))?;
    Ok(ValidatedUpsert {
        id: profile.id,
        name_key: normalize_name_key(&name),
        name,
        signature: profile.signature,
        mapping_json,
        fixed_values_json,
    })
}

fn upsert_in_savepoint(
    conn: &Connection,
    profile: &ValidatedUpsert,
) -> Result<SourceMappingProfile, SourceMappingProfileError> {
    let id = if let Some(id) = profile.id {
        let conflicting_id = conn
            .query_row(
                "SELECT id FROM source_mapping_profiles WHERE name_key = ?1 AND id <> ?2",
                params![profile.name_key, id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if conflicting_id.is_some() {
            return Err(SourceMappingProfileError::NameConflict(
                profile.name.clone(),
            ));
        }
        let changed = conn.execute(
            "UPDATE source_mapping_profiles
             SET name = ?1,
                 name_key = ?2,
                 signature = ?3,
                 mapping_json = ?4,
                 fixed_values_json = ?5,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?6",
            params![
                profile.name,
                profile.name_key,
                profile.signature,
                profile.mapping_json,
                profile.fixed_values_json,
                id
            ],
        )?;
        if changed == 0 {
            return Err(SourceMappingProfileError::NotFound(id));
        }
        id
    } else if let Some(existing_id) = conn
        .query_row(
            "SELECT id FROM source_mapping_profiles WHERE name_key = ?1",
            [&profile.name_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        conn.execute(
            "UPDATE source_mapping_profiles
             SET name = ?1,
                 signature = ?2,
                 mapping_json = ?3,
                 fixed_values_json = ?4,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?5",
            params![
                profile.name,
                profile.signature,
                profile.mapping_json,
                profile.fixed_values_json,
                existing_id
            ],
        )?;
        existing_id
    } else {
        conn.execute(
            "INSERT INTO source_mapping_profiles (
                name, name_key, signature, mapping_json, fixed_values_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                profile.name,
                profile.name_key,
                profile.signature,
                profile.mapping_json,
                profile.fixed_values_json
            ],
        )?;
        conn.last_insert_rowid()
    };
    get(conn, id)?.ok_or(SourceMappingProfileError::NotFound(id))
}

fn query_collection(
    conn: &Connection,
    sql: &str,
    signature: Option<&str>,
) -> Result<SourceMappingProfileCollection, SourceMappingProfileError> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = match signature {
        Some(signature) => statement.query([signature])?,
        None => statement.query([])?,
    };
    let mut collection = SourceMappingProfileCollection::default();
    while let Some(row) = rows.next()? {
        let raw = read_raw_profile(row)?;
        match decode_profile(raw) {
            Ok(profile) => collection.profiles.push(profile),
            Err(SourceMappingProfileError::CorruptRow { id, reason }) => collection
                .ignored_corrupt_rows
                .push(SourceMappingProfileCorruption { id, reason }),
            Err(error) => return Err(error),
        }
    }
    Ok(collection)
}

struct RawProfile {
    id: i64,
    name: String,
    signature: String,
    mapping_json: String,
    fixed_values_json: String,
    created_at: String,
    updated_at: String,
}

fn read_raw_profile(row: &Row<'_>) -> rusqlite::Result<RawProfile> {
    Ok(RawProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        signature: row.get(2)?,
        mapping_json: row.get(3)?,
        fixed_values_json: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn decode_profile(raw: RawProfile) -> Result<SourceMappingProfile, SourceMappingProfileError> {
    let corrupt = |reason: String| SourceMappingProfileError::CorruptRow { id: raw.id, reason };
    let name = validate_name(&raw.name).map_err(|error| corrupt(error.to_string()))?;
    let expected_columns =
        validate_signature(&raw.signature).map_err(|error| corrupt(error.to_string()))?;
    let mapping = serde_json::from_str::<Vec<Option<SemanticField>>>(&raw.mapping_json)
        .map_err(|error| corrupt(format!("invalid mapping JSON: {error}")))?;
    validate_mapping(&mapping, expected_columns).map_err(|error| corrupt(error.to_string()))?;
    let fixed_values =
        serde_json::from_str::<BTreeMap<SemanticField, String>>(&raw.fixed_values_json)
            .map_err(|error| corrupt(format!("invalid fixed values JSON: {error}")))?;
    let fixed_values =
        validate_fixed_values(fixed_values).map_err(|error| corrupt(error.to_string()))?;
    if raw.created_at.trim().is_empty() || raw.updated_at.trim().is_empty() {
        return Err(corrupt("timestamps must not be empty".to_string()));
    }
    Ok(SourceMappingProfile {
        id: raw.id,
        name,
        signature: raw.signature,
        mapping,
        fixed_values,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn validate_name(name: &str) -> Result<String, SourceMappingProfileError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SourceMappingProfileError::Validation(
            "profile name must not be empty".to_string(),
        ));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(SourceMappingProfileError::Validation(format!(
            "profile name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn validate_signature(signature: &str) -> Result<usize, SourceMappingProfileError> {
    let mut parts = signature.split(':');
    let version = parts.next();
    let count = parts.next();
    let digest = parts.next();
    if parts.next().is_some()
        || version != Some(SIGNATURE_VERSION)
        || digest.is_none_or(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(SourceMappingProfileError::Validation(
            "source signature has an unsupported format".to_string(),
        ));
    }
    let count = count
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| {
            SourceMappingProfileError::Validation(
                "source signature has an invalid column count".to_string(),
            )
        })?;
    if count > MAX_COLUMNS {
        return Err(SourceMappingProfileError::Validation(format!(
            "source mappings support at most {MAX_COLUMNS} columns"
        )));
    }
    Ok(count)
}

fn validate_mapping(
    mapping: &[Option<SemanticField>],
    expected_columns: usize,
) -> Result<(), SourceMappingProfileError> {
    if mapping.len() > MAX_COLUMNS {
        return Err(SourceMappingProfileError::Validation(format!(
            "source mappings support at most {MAX_COLUMNS} assignments"
        )));
    }
    if mapping.len() != expected_columns {
        return Err(SourceMappingProfileError::Validation(format!(
            "source mapping has {} assignments but its signature describes {expected_columns} columns",
            mapping.len()
        )));
    }
    Ok(())
}

fn validate_fixed_values(
    values: BTreeMap<SemanticField, String>,
) -> Result<BTreeMap<SemanticField, String>, SourceMappingProfileError> {
    if values.len() > 2 {
        return Err(SourceMappingProfileError::Validation(
            "at most two fixed semantic values are supported".to_string(),
        ));
    }
    let mut validated = BTreeMap::new();
    for (semantic, value) in values {
        if !matches!(
            semantic,
            SemanticField::Currency | SemanticField::WeightUnit
        ) {
            return Err(SourceMappingProfileError::Validation(format!(
                "fixed values are not supported for {semantic:?}"
            )));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(SourceMappingProfileError::Validation(format!(
                "fixed {semantic:?} value must not be empty"
            )));
        }
        if value.chars().count() > MAX_FIXED_VALUE_CHARS {
            return Err(SourceMappingProfileError::Validation(format!(
                "fixed {semantic:?} value must be at most {MAX_FIXED_VALUE_CHARS} characters"
            )));
        }
        validated.insert(semantic, value.to_string());
    }
    Ok(validated)
}

fn normalize_header(header: &str) -> String {
    header
        .split_whitespace()
        .map(|word| {
            word.chars()
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_name_key(name: &str) -> String {
    normalize_header(name)
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

fn table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{normalize_header, validate_signature};

    #[test]
    fn header_normalization_is_unicode_case_and_whitespace_insensitive() {
        assert_eq!(normalize_header("  ÄBC\t ТОВАР  "), "äbc товар");
    }

    #[test]
    fn malformed_signatures_are_rejected() {
        for signature in ["", "smp1:2:nope", "smp2:2:", "smp1:x:"] {
            assert!(validate_signature(signature).is_err());
        }
    }
}
