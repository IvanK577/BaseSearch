use rusqlite::types::Value;

use crate::domain::table::ColumnStorage;
use crate::search::{FieldInfo, FieldRef};
use crate::storage::effective_rows;
use crate::storage::source_schemas::SourceFieldLookup;

pub(crate) struct SourceFieldSelect {
    pub(crate) expressions: Vec<String>,
    pub(crate) params: Vec<Value>,
}

/// A field reference resolved down to its physical storage: a canonical
/// records column or an entry in the `extra` JSON payload. A `schema_id`
/// pins the reference to one registered source schema, so a schema-exact
/// field never reads a same-named column of an unrelated file.
pub(crate) enum ResolvedRef {
    Column {
        name: String,
        schema_id: Option<i64>,
    },
    Extra {
        header: String,
        schema_id: Option<i64>,
    },
}

/// Wraps an expression so it only yields a value for rows of one schema.
pub(crate) fn schema_guard(expr: String, payload_alias: &str, schema_id: Option<i64>) -> String {
    match schema_id {
        Some(id) => format!("CASE WHEN {payload_alias}.schema_id = {id} THEN {expr} END"),
        None => expr,
    }
}

/// Resolves any field reference to physical storage. Registered source-schema
/// fields resolve through the lookup — schema-scoped, so the same header in
/// another file's schema never leaks in. An unknown source-field id degrades
/// to an extra lookup by the raw id, which compiles and simply yields empty
/// values instead of failing the whole query.
pub(crate) fn resolve_ref(source: &FieldRef, lookup: &SourceFieldLookup) -> ResolvedRef {
    match source {
        FieldRef::Column(name) => ResolvedRef::Column {
            name: name.clone(),
            schema_id: None,
        },
        FieldRef::Extra(header) => ResolvedRef::Extra {
            header: header.clone(),
            schema_id: None,
        },
        FieldRef::SourceField(field_id) => match lookup.get(field_id) {
            Some(field) => match &field.storage {
                ColumnStorage::SchemaColumn(name) => ResolvedRef::Column {
                    name: name.clone(),
                    schema_id: Some(field.schema_id),
                },
                ColumnStorage::SourceJson => ResolvedRef::Extra {
                    header: field.header.clone(),
                    schema_id: Some(field.schema_id),
                },
            },
            None => ResolvedRef::Extra {
                header: field_id.clone(),
                schema_id: None,
            },
        },
    }
}

pub(crate) fn select_for_fields(
    fields: &[FieldInfo],
    payload_alias: &str,
    lookup: &SourceFieldLookup,
) -> SourceFieldSelect {
    let mut expressions = Vec::with_capacity(fields.len());
    let mut params = Vec::new();
    for field in fields {
        match resolve_ref(&field.source, lookup) {
            ResolvedRef::Column { name, schema_id } => expressions.push(schema_guard(
                effective_rows::result_column(payload_alias, &name),
                payload_alias,
                schema_id,
            )),
            ResolvedRef::Extra { header, schema_id } => {
                expressions.push(schema_guard(
                    format!("extra_value({payload_alias}.extra, ?)"),
                    payload_alias,
                    schema_id,
                ));
                params.push(header.into());
            }
        }
    }
    SourceFieldSelect {
        expressions,
        params,
    }
}

pub(crate) fn is_source_file_field(field: &FieldInfo) -> bool {
    matches!(&field.source, FieldRef::Column(name) if name == "source_file")
}

#[cfg(test)]
mod tests {
    use super::select_for_fields;
    use rusqlite::types::Value;

    use crate::search::{FieldInfo, FieldKind, FieldRef, operators_for_kind};

    fn field(id: &str, source: FieldRef) -> FieldInfo {
        FieldInfo {
            id: id.to_string(),
            label: id.to_string(),
            kind: FieldKind::Text,
            source,
            operators: operators_for_kind(FieldKind::Text).to_vec(),
        }
    }

    #[test]
    fn select_plan_reads_schema_and_json_backed_source_fields() {
        let fields = [
            field(
                "source:product",
                FieldRef::Column("description".to_string()),
            ),
            field("source:sku", FieldRef::Extra("SKU".to_string())),
        ];

        let plan = select_for_fields(&fields, "r", &Default::default());

        assert_eq!(
            plan.expressions,
            vec![
                "r.description".to_string(),
                "extra_value(r.extra, ?)".to_string()
            ]
        );
        assert_eq!(plan.params, vec![Value::Text("SKU".to_string())]);
    }
}
