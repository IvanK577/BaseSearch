use rusqlite::types::Value;

use crate::domain::table::{ColumnStorage, SemanticField, SourceSchemaField, TableShape};
use crate::schema::RESULT_COLUMNS;
use crate::search::{
    ConditionOp, ConditionValue, FieldKind, FieldRef, LogicOp, QueryCondition, QueryExpr,
    field_catalog, field_catalog_for_context, field_kind_for_column,
    field_kind_for_source_schema_field,
};
use crate::storage::derived;
use crate::storage::normalize::{
    NumberStyle, normalize_country_key, normalize_text_key, parse_number_styled, parse_year,
};
use crate::storage::search_text::glob_escape;
use crate::storage::source_schemas::SourceFieldLookup;

#[derive(Clone)]
struct SearchFieldSql {
    expr: String,
    extra_header: Option<String>,
    kind: FieldKind,
    /// For `Number` fields: the pre-materialized column to compare against
    /// (already parsed at import with the right style), so a value/quantity
    /// filter reads exactly what analytics aggregates.
    number_column: Option<String>,
    /// The style to parse the user's numeric bound with, matching how the
    /// stored value was parsed.
    number_style: NumberStyle,
    /// Pre-normalized country key for schema-backed country columns. Generic
    /// JSON fields keep using `country_key(extra_value(...))`.
    country_column: Option<String>,
}

/// A number filter must parse the user's bound and compare against the stored
/// value using the SAME style, or "value = 1,250" would compile to 1.25 while
/// the column stores 1250. Returns the SQL numeric expression and the parsed
/// bound, or an error when the bound is not numeric.
fn number_predicate(field: &SearchFieldSql, value: &str) -> rusqlite::Result<(String, f64)> {
    let number = parse_number_styled(value, field.number_style)
        .ok_or_else(|| invalid_search_input("number comparison requires a number"))?;
    let expr = match &field.number_column {
        // Read the materialized column directly — no per-row re-parse, and
        // guaranteed to match the analytics/aggregate value.
        Some(column) => column.clone(),
        None => match field.number_style {
            NumberStyle::PreferGrouped => format!("num_value_grouped({})", field.expr),
            NumberStyle::PreferDecimal => format!("num_value({})", field.expr),
        },
    };
    Ok((expr, number))
}

pub(crate) fn compile_query_expr(
    expr: &QueryExpr,
    shape: Option<&TableShape>,
    payload_alias: &str,
    source_fields: &SourceFieldLookup,
) -> rusqlite::Result<Option<(String, Vec<Value>)>> {
    match expr {
        QueryExpr::Group(group) => {
            let mut clauses = Vec::new();
            let mut params = Vec::new();
            for child in &group.children {
                if let Some((clause, child_params)) =
                    compile_query_expr(child, shape, payload_alias, source_fields)?
                {
                    clauses.push(clause);
                    params.extend(child_params);
                }
            }
            if clauses.is_empty() {
                return Ok(None);
            }
            let joiner = match group.op {
                LogicOp::And => " AND ",
                LogicOp::Or => " OR ",
            };
            let mut clause = format!("({})", clauses.join(joiner));
            if group.negated {
                clause = format!("NOT ({clause})");
            }
            Ok(Some((clause, params)))
        }
        QueryExpr::Condition(condition) => {
            compile_condition(condition, shape, payload_alias, source_fields)
        }
    }
}

fn compile_condition(
    condition: &QueryCondition,
    shape: Option<&TableShape>,
    payload_alias: &str,
    source_fields: &SourceFieldLookup,
) -> rusqlite::Result<Option<(String, Vec<Value>)>> {
    if condition.is_empty() {
        return Ok(None);
    }
    let field = search_field_sql(&condition.field, shape, payload_alias, source_fields)?;
    validate_condition_operator(field.kind, condition.op)?;

    let mut params = Vec::new();
    let clause = match condition.op {
        ConditionOp::Contains => {
            let value = condition
                .value
                .single()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_search_input("contains requires a value"))?;
            push_field_params(&field, &mut params);
            params.push(value.to_lowercase().into());
            format!("cyr_contains({}, ?)", field.expr)
        }
        ConditionOp::Equals => {
            let value = condition
                .value
                .single()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_search_input("equals requires a value"))?;
            compile_equal_clause(&field, value, &mut params)?
        }
        ConditionOp::StartsWith => {
            let value = condition
                .value
                .single()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_search_input("starts with requires a value"))?;
            push_field_params(&field, &mut params);
            match field.kind {
                FieldKind::Code => {
                    params.push(format!("{}*", glob_escape(value)).into());
                    format!("{} GLOB ?", field.expr)
                }
                FieldKind::Text => {
                    params.push(format!("{}*", glob_escape(&normalize_text_key(value))).into());
                    format!("text_key({}) GLOB ?", field.expr)
                }
                _ => {
                    return Err(invalid_search_input(
                        "starts with is only valid for text and code fields",
                    ));
                }
            }
        }
        ConditionOp::IsAnyOf => {
            let values = condition
                .value
                .list()
                .ok_or_else(|| invalid_search_input("is any of requires a list"))?;
            let mut parts = Vec::new();
            for value in values.iter().map(|value| value.trim()) {
                if value.is_empty() {
                    continue;
                }
                parts.push(compile_equal_clause(&field, value, &mut params)?);
            }
            if parts.is_empty() {
                return Ok(None);
            }
            format!("({})", parts.join(" OR "))
        }
        ConditionOp::Range => compile_range_clause(&field, &condition.value, &mut params)?,
        ConditionOp::IsEmpty => {
            push_field_params(&field, &mut params);
            format!("TRIM(COALESCE({}, '')) = ''", field.expr)
        }
        ConditionOp::IsNotEmpty => {
            push_field_params(&field, &mut params);
            format!("TRIM(COALESCE({}, '')) <> ''", field.expr)
        }
    };
    let clause = if condition.negated {
        format!("NOT ({clause})")
    } else {
        clause
    };
    Ok(Some((format!("({clause})"), params)))
}

fn search_field_sql(
    field: &FieldRef,
    shape: Option<&TableShape>,
    payload_alias: &str,
    source_fields: &SourceFieldLookup,
) -> rusqlite::Result<SearchFieldSql> {
    match field {
        FieldRef::SourceField(field_id) => {
            let Some(source_field) = source_fields.get(field_id) else {
                return Err(invalid_search_input(&format!(
                    "Unknown search field: {field_id}"
                )));
            };
            // Semantic-driven kind so operator validation matches the operator
            // list the field catalog advertised for this field. Every
            // expression is scoped to the field's schema, so a same-named
            // column in another file's schema never matches.
            let kind = field_kind_for_source_schema_field(source_field);
            let sid = source_field.schema_id;
            let guard = |expr: String| {
                format!("CASE WHEN {payload_alias}.schema_id = {sid} THEN {expr} END")
            };
            match &source_field.storage {
                ColumnStorage::SchemaColumn(name) => Ok(SearchFieldSql {
                    expr: guard(format!("{payload_alias}.{name}")),
                    extra_header: None,
                    kind,
                    number_column: derived::num_column_for(name)
                        .map(|column| guard(format!("{payload_alias}.{column}"))),
                    number_style: derived::number_style_for(name)
                        .unwrap_or(NumberStyle::PreferDecimal),
                    country_column: derived::key_column_for(name)
                        .map(|column| guard(format!("{payload_alias}.{column}"))),
                }),
                ColumnStorage::SourceJson => Ok(SearchFieldSql {
                    expr: guard(format!("extra_value({payload_alias}.extra, ?)")),
                    extra_header: Some(source_field.header.clone()),
                    kind,
                    number_column: None,
                    number_style: source_json_number_style(source_field),
                    country_column: None,
                }),
            }
        }
        FieldRef::Column(name) if name == "year" => Ok(SearchFieldSql {
            expr: format!("{payload_alias}.year"),
            extra_header: None,
            kind: FieldKind::Year,
            number_column: None,
            number_style: NumberStyle::PreferDecimal,
            country_column: None,
        }),
        FieldRef::Column(name) if RESULT_COLUMNS.contains(&name.as_str()) => Ok(SearchFieldSql {
            expr: format!("{payload_alias}.{name}"),
            extra_header: None,
            kind: field_kind_for_column(name),
            number_column: derived::num_column_for(name)
                .map(|column| format!("{payload_alias}.{column}")),
            number_style: derived::number_style_for(name).unwrap_or(NumberStyle::PreferDecimal),
            country_column: derived::key_column_for(name)
                .map(|column| format!("{payload_alias}.{column}")),
        }),
        FieldRef::Column(name) => Err(invalid_search_input(&format!(
            "Unknown search field: {name}"
        ))),
        FieldRef::Extra(header) if header.trim().is_empty() => {
            Err(invalid_search_input("Extra search field header is empty"))
        }
        FieldRef::Extra(header) => Ok(SearchFieldSql {
            expr: format!("extra_value({payload_alias}.extra, ?)"),
            extra_header: Some(header.trim().to_string()),
            kind: kind_for_field_ref(field, shape)
                .or_else(|| {
                    field_catalog([header.trim().to_string()])
                        .pop()
                        .map(|field| field.kind)
                })
                .unwrap_or(FieldKind::Text),
            number_column: None,
            // A user-mapped generic Value/Quantity column stores grouped
            // numbers; match that when the shape assigns the semantic.
            number_style: extra_number_style(header.trim(), shape),
            country_column: None,
        }),
    }
}

/// Number style for a JSON-stored source-schema field, decided by its own
/// semantic: Value and Quantity read a lone three-digit tail as a thousands
/// group, matching how the importer parsed the stored number.
fn source_json_number_style(field: &SourceSchemaField) -> NumberStyle {
    match field.semantic {
        Some(SemanticField::Value) | Some(SemanticField::Quantity) => NumberStyle::PreferGrouped,
        _ => NumberStyle::PreferDecimal,
    }
}

/// Number style for an extra-JSON column, decided by the semantic the user
/// assigned to it in the table shape. Value and Quantity read a lone
/// three-digit tail as a thousands group; everything else stays decimal.
fn extra_number_style(header: &str, shape: Option<&TableShape>) -> NumberStyle {
    let matches_value = shape.is_some_and(|shape| {
        shape.columns.iter().any(|column| {
            column.header.trim().eq_ignore_ascii_case(header)
                && matches!(
                    column.semantic,
                    Some(SemanticField::Value) | Some(SemanticField::Quantity)
                )
        })
    });
    if matches_value {
        NumberStyle::PreferGrouped
    } else {
        NumberStyle::PreferDecimal
    }
}

fn kind_for_field_ref(field: &FieldRef, shape: Option<&TableShape>) -> Option<FieldKind> {
    let shape = shape.filter(|shape| !shape.columns.is_empty())?;
    let field_id = field.id();
    field_catalog_for_context(Some(shape), Vec::<String>::new())
        .into_iter()
        .find(|info| info.source == *field || info.id == field_id)
        .map(|field| field.kind)
}

fn validate_condition_operator(kind: FieldKind, op: ConditionOp) -> rusqlite::Result<()> {
    let allowed = match kind {
        FieldKind::Text => matches!(
            op,
            ConditionOp::Contains
                | ConditionOp::Equals
                | ConditionOp::StartsWith
                | ConditionOp::IsAnyOf
                | ConditionOp::IsEmpty
                | ConditionOp::IsNotEmpty
        ),
        FieldKind::Code => matches!(
            op,
            ConditionOp::StartsWith
                | ConditionOp::Equals
                | ConditionOp::IsAnyOf
                | ConditionOp::IsEmpty
                | ConditionOp::IsNotEmpty
        ),
        FieldKind::Country => matches!(
            op,
            ConditionOp::Equals
                | ConditionOp::IsAnyOf
                | ConditionOp::IsEmpty
                | ConditionOp::IsNotEmpty
        ),
        FieldKind::Number | FieldKind::Date | FieldKind::Year => matches!(
            op,
            ConditionOp::Equals
                | ConditionOp::Range
                | ConditionOp::IsEmpty
                | ConditionOp::IsNotEmpty
        ),
    };
    if allowed {
        Ok(())
    } else {
        Err(invalid_search_input(&format!(
            "{} is not valid for {:?} fields",
            op.label(),
            kind
        )))
    }
}

fn push_field_params(field: &SearchFieldSql, params: &mut Vec<Value>) {
    if let Some(header) = &field.extra_header {
        params.push(header.clone().into());
    }
}

fn compile_equal_clause(
    field: &SearchFieldSql,
    value: &str,
    params: &mut Vec<Value>,
) -> rusqlite::Result<String> {
    push_field_params(field, params);
    match field.kind {
        FieldKind::Text => {
            params.push(value.to_string().into());
            Ok(format!("text_key({}) = text_key(?)", field.expr))
        }
        FieldKind::Code | FieldKind::Date => {
            params.push(value.to_string().into());
            Ok(format!("TRIM(COALESCE({}, '')) = ?", field.expr))
        }
        FieldKind::Country => {
            params.push(normalize_country_key(value).into());
            Ok(match &field.country_column {
                Some(column) => format!("{column} = ?"),
                None => format!("country_key({}) = ?", field.expr),
            })
        }
        FieldKind::Number => {
            let (expr, number) = number_predicate(field, value)?;
            params.push(number.into());
            Ok(format!("{expr} = ?"))
        }
        FieldKind::Year => {
            let year = parse_year(value)
                .ok_or_else(|| invalid_search_input("year comparison requires a 4-digit year"))?;
            params.push(year.into());
            Ok(format!("{} = ?", field.expr))
        }
    }
}

fn compile_range_clause(
    field: &SearchFieldSql,
    value: &ConditionValue,
    params: &mut Vec<Value>,
) -> rusqlite::Result<String> {
    let ConditionValue::Range { from, to } = value else {
        return Err(invalid_search_input("range requires from/to values"));
    };
    let mut parts = Vec::new();
    if let Some(from) = from
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_field_params(field, params);
        parts.push(compile_range_bound(field, ">=", from, params)?);
    }
    if let Some(to) = to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_field_params(field, params);
        parts.push(compile_range_bound(field, "<=", to, params)?);
    }
    if parts.is_empty() {
        return Err(invalid_search_input("range requires at least one boundary"));
    }
    Ok(format!("({})", parts.join(" AND ")))
}

fn compile_range_bound(
    field: &SearchFieldSql,
    cmp: &str,
    value: &str,
    params: &mut Vec<Value>,
) -> rusqlite::Result<String> {
    match field.kind {
        FieldKind::Number => {
            let (expr, number) = number_predicate(field, value)?;
            params.push(number.into());
            Ok(format!("{expr} {cmp} ?"))
        }
        FieldKind::Year => {
            let year = parse_year(value)
                .ok_or_else(|| invalid_search_input("year range requires 4-digit years"))?;
            params.push(year.into());
            Ok(format!("{} {cmp} ?", field.expr))
        }
        FieldKind::Date => {
            params.push(value.to_string().into());
            Ok(format!("TRIM(COALESCE({}, '')) {cmp} ?", field.expr))
        }
        _ => Err(invalid_search_input(
            "range is only valid for number, date, and year fields",
        )),
    }
}

fn invalid_search_input(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_string())
}
