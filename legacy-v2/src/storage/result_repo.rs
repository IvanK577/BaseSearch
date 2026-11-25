use rusqlite::{Connection, params_from_iter};

use crate::db::{DynamicSearchPage, Query, RecordCard, ResultSort, SearchPage};
use crate::schema::{COLUMNS, RESULT_COLUMNS};
use crate::search::{FieldInfo, FieldKind};
use crate::storage::derived;
use crate::storage::effective_rows;
use crate::storage::extra::parse_extra;
use crate::storage::query_plan::FilterPlan;
use crate::storage::search_text::product_code_search_prefix;
use crate::storage::source_fields;
use crate::storage::source_fields::ResolvedRef;
use crate::storage::source_schemas::SourceFieldLookup;

pub(crate) fn capture_search_snapshot(conn: &Connection) -> rusqlite::Result<u64> {
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM records", [], |row| {
        row.get(0)
    })?;
    Ok(max_id.max(0) as u64)
}

pub(crate) fn count(
    conn: &Connection,
    mut plan: FilterPlan,
    snapshot: u64,
) -> rusqlite::Result<u64> {
    apply_search_snapshot(&mut plan, snapshot);
    let sql = format!(
        "SELECT COUNT(*) FROM records r{}{}",
        plan.joins, plan.where_sql
    );
    let n: i64 = conn.query_row(&sql, params_from_iter(plan.params), |r| r.get(0))?;
    Ok(n as u64)
}

pub(crate) fn legacy_search_page(
    conn: &Connection,
    q: &Query,
    mut plan: FilterPlan,
    snapshot: u64,
    limit: u64,
    offset: u64,
) -> rusqlite::Result<SearchPage> {
    apply_search_snapshot(&mut plan, snapshot);
    let payload_alias = plan.payload_alias;
    let select: Vec<String> = RESULT_COLUMNS
        .iter()
        .map(|column| effective_rows::result_column(payload_alias, column))
        .collect();
    let order = result_order(q, payload_alias);
    let sql = format!(
        "SELECT r.id, {select}, r.dup_first_file FROM records r{joins}{where_sql} ORDER BY {order} LIMIT ? OFFSET ?",
        select = select.join(", "),
        joins = plan.joins,
        where_sql = plan.where_sql,
    );
    plan.params.push((limit as i64).into());
    plan.params.push((offset as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(plan.params))?;
    let mut ids = Vec::new();
    let mut data = Vec::new();
    let mut dups = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get::<_, i64>(0)?);
        let mut values = Vec::with_capacity(RESULT_COLUMNS.len());
        for i in 0..RESULT_COLUMNS.len() {
            values.push(row.get::<_, Option<String>>(i + 1)?.unwrap_or_default());
        }
        data.push(values);
        dups.push(row.get::<_, Option<String>>(RESULT_COLUMNS.len() + 1)?);
    }
    Ok((ids, data, dups))
}

#[expect(
    clippy::too_many_arguments,
    reason = "repository boundary keeps query, paging, sort, and schema context explicit"
)]
pub(crate) fn dynamic_search_page(
    conn: &Connection,
    q: &Query,
    fields: Vec<FieldInfo>,
    mut plan: FilterPlan,
    snapshot: u64,
    limit: u64,
    offset: u64,
    sort: Option<&ResultSort>,
    lookup: &SourceFieldLookup,
) -> rusqlite::Result<DynamicSearchPage> {
    apply_search_snapshot(&mut plan, snapshot);
    let payload_alias = plan.payload_alias;
    let field_select = source_fields::select_for_fields(&fields, payload_alias, lookup);
    let order = sort
        .and_then(|sort| sort_order_sql(&fields, sort, payload_alias, lookup))
        .unwrap_or_else(|| result_order(q, payload_alias));
    let sql = format!(
        "SELECT r.id, {select}, r.dup_first_file FROM records r{joins}{where_sql} ORDER BY {order} LIMIT ? OFFSET ?",
        select = field_select.expressions.join(", "),
        joins = plan.joins,
        where_sql = plan.where_sql,
    );
    let mut final_params = field_select.params;
    final_params.extend(plan.params);
    final_params.push((limit as i64).into());
    final_params.push((offset as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(final_params))?;
    let mut ids = Vec::new();
    let mut data = Vec::new();
    let mut dups = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get::<_, i64>(0)?);
        let mut values = Vec::with_capacity(fields.len());
        for i in 0..fields.len() {
            values.push(row.get::<_, Option<String>>(i + 1)?.unwrap_or_default());
        }
        data.push(values);
        dups.push(row.get::<_, Option<String>>(fields.len() + 1)?);
    }
    Ok((fields, ids, data, dups))
}

fn apply_search_snapshot(plan: &mut FilterPlan, snapshot: u64) {
    let conjunction = if plan.where_sql.is_empty() {
        " WHERE"
    } else {
        " AND"
    };
    plan.where_sql.push_str(&format!("{conjunction} r.id <= ?"));
    plan.params
        .push(i64::try_from(snapshot).unwrap_or(i64::MAX).into());
}

pub(crate) fn legacy_export_batch(
    conn: &Connection,
    mut plan: FilterPlan,
    last_id: i64,
    limit: u64,
) -> rusqlite::Result<(i64, Vec<Vec<String>>)> {
    let payload_alias = plan.payload_alias;
    let select: Vec<String> = COLUMNS
        .iter()
        .map(|column| effective_rows::payload_column(payload_alias, column.name))
        .collect();
    let cond = keyset_condition_prefix(&plan.where_sql);
    let sql = format!(
        "SELECT r.id, {select}, r.source_file FROM records r{joins}{where_sql}{cond} r.id > ? ORDER BY r.id LIMIT ?",
        select = select.join(", "),
        joins = plan.joins,
        where_sql = plan.where_sql,
    );
    plan.params.push(last_id.into());
    plan.params.push((limit as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(plan.params))?;
    let mut data = Vec::new();
    let mut max_id = last_id;
    while let Some(row) = rows.next()? {
        max_id = row.get::<_, i64>(0)?;
        let mut values = Vec::with_capacity(COLUMNS.len() + 1);
        for i in 0..=COLUMNS.len() {
            values.push(row.get::<_, Option<String>>(i + 1)?.unwrap_or_default());
        }
        data.push(values);
    }
    Ok((max_id, data))
}

pub(crate) fn export_batch_fields(
    conn: &Connection,
    fields: &[FieldInfo],
    plan: FilterPlan,
    last_id: i64,
    limit: u64,
    lookup: &SourceFieldLookup,
) -> rusqlite::Result<(i64, Vec<Vec<String>>)> {
    let field_select = source_fields::select_for_fields(fields, plan.payload_alias, lookup);
    let cond = keyset_condition_prefix(&plan.where_sql);
    let sql = format!(
        "SELECT r.id, {select} FROM records r{joins}{where_sql}{cond} r.id > ? ORDER BY r.id LIMIT ?",
        select = field_select.expressions.join(", "),
        joins = plan.joins,
        where_sql = plan.where_sql,
    );
    let mut final_params = field_select.params;
    final_params.extend(plan.params);
    final_params.push(last_id.into());
    final_params.push((limit as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(final_params))?;
    let mut data = Vec::new();
    let mut max_id = last_id;
    while let Some(row) = rows.next()? {
        max_id = row.get::<_, i64>(0)?;
        let mut values = Vec::with_capacity(fields.len());
        for i in 0..fields.len() {
            values.push(row.get::<_, Option<String>>(i + 1)?.unwrap_or_default());
        }
        data.push(values);
    }
    Ok((max_id, data))
}

#[expect(
    clippy::too_many_arguments,
    reason = "repository boundary keeps export selection, sort, visitor, and schema context explicit"
)]
pub(crate) fn visit_export_rows_fields(
    conn: &Connection,
    q: &Query,
    fields: &[FieldInfo],
    sort_catalog: &[FieldInfo],
    plan: FilterPlan,
    sort: Option<&ResultSort>,
    mut visit: impl FnMut(Vec<String>) -> bool,
    lookup: &SourceFieldLookup,
) -> rusqlite::Result<u64> {
    let payload_alias = plan.payload_alias;
    let field_select = source_fields::select_for_fields(fields, payload_alias, lookup);
    let order = sort
        .and_then(|sort| sort_order_sql(sort_catalog, sort, payload_alias, lookup))
        .unwrap_or_else(|| result_order(q, payload_alias));
    let sql = format!(
        "SELECT {select} FROM records r{joins}{where_sql} ORDER BY {order}",
        select = field_select.expressions.join(", "),
        joins = plan.joins,
        where_sql = plan.where_sql,
    );
    let mut final_params = field_select.params;
    final_params.extend(plan.params);
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(final_params))?;
    let mut visited = 0_u64;
    while let Some(row) = rows.next()? {
        let mut values = Vec::with_capacity(fields.len());
        for index in 0..fields.len() {
            values.push(row.get::<_, Option<String>>(index)?.unwrap_or_default());
        }
        if !visit(values) {
            break;
        }
        visited += 1;
    }
    Ok(visited)
}

pub(crate) fn record_card(
    conn: &Connection,
    fields: Vec<FieldInfo>,
    id: i64,
    lookup: &SourceFieldLookup,
) -> rusqlite::Result<RecordCard> {
    let card_fields: Vec<FieldInfo> = fields
        .into_iter()
        .filter(|field| !source_fields::is_source_file_field(field))
        .collect();
    let field_select =
        source_fields::select_for_fields(&card_fields, effective_rows::PAYLOAD_ALIAS, lookup);
    let sql = format!(
        "SELECT {}, r.source_file FROM records r{} WHERE r.id = ?",
        field_select.expressions.join(", "),
        effective_rows::payload_join()
    );
    let mut params = field_select.params;
    params.push(id.into());
    conn.query_row(&sql, params_from_iter(params), |row| {
        let mut fields = Vec::with_capacity(card_fields.len());
        for (i, field) in card_fields.iter().enumerate() {
            fields.push((
                field.label.clone(),
                row.get::<_, Option<String>>(i)?.unwrap_or_default(),
            ));
        }
        let source_file: String = row.get(card_fields.len())?;
        Ok(RecordCard {
            fields,
            source_file,
            extra: Vec::new(),
        })
    })
}

pub(crate) fn legacy_record_card(conn: &Connection, id: i64) -> rusqlite::Result<RecordCard> {
    let select: Vec<String> = COLUMNS
        .iter()
        .map(|column| effective_rows::payload_column(effective_rows::PAYLOAD_ALIAS, column.name))
        .collect();
    let sql = format!(
        "SELECT {}, r.source_file, p.extra FROM records r{} WHERE r.id = ?1",
        select.join(", "),
        effective_rows::payload_join()
    );
    conn.query_row(&sql, [id], |row| {
        let mut fields = Vec::with_capacity(COLUMNS.len());
        for (i, col) in COLUMNS.iter().enumerate() {
            fields.push((
                col.header.to_string(),
                row.get::<_, Option<String>>(i)?.unwrap_or_default(),
            ));
        }
        let source_file: String = row.get(COLUMNS.len())?;
        let extra = parse_extra(row.get::<_, Option<String>>(COLUMNS.len() + 1)?.as_deref());
        Ok(RecordCard {
            fields,
            source_file,
            extra,
        })
    })
}

fn keyset_condition_prefix(where_sql: &str) -> &'static str {
    if where_sql.is_empty() {
        " WHERE"
    } else {
        " AND"
    }
}

/// Builds an `ORDER BY` expression for a user-chosen result column, or `None`
/// when the field id is unknown. Numeric and year columns sort on their
/// materialized/typed value so "1 250" outranks "999"; ISO dates sort as text
/// (which is chronological); everything else sorts case-insensitively. A stable
/// `r.id` tiebreaker keeps paging deterministic.
pub(crate) fn sort_order_sql(
    fields: &[FieldInfo],
    sort: &ResultSort,
    payload_alias: &str,
    lookup: &SourceFieldLookup,
) -> Option<String> {
    let field = fields.iter().find(|field| field.id == sort.field)?;
    // Source-schema fields sort exactly like the column or extra header that
    // physically stores them, scoped to their schema's rows.
    let resolved = source_fields::resolve_ref(&field.source, lookup);
    let base = match &resolved {
        ResolvedRef::Column { name, schema_id } => source_fields::schema_guard(
            effective_rows::result_column(payload_alias, name),
            payload_alias,
            *schema_id,
        ),
        ResolvedRef::Extra { header, schema_id } => source_fields::schema_guard(
            format!(
                "extra_value({payload_alias}.extra, '{}')",
                header.replace('\'', "''")
            ),
            payload_alias,
            *schema_id,
        ),
    };
    let expr = match field.kind {
        FieldKind::Number | FieldKind::Year => numeric_sort_expr(&resolved, &base, payload_alias),
        // Dates are stored normalized to ISO "YYYY-MM-DD", which sorts
        // chronologically as text.
        FieldKind::Date => base,
        FieldKind::Text | FieldKind::Code | FieldKind::Country => {
            format!("{base} COLLATE NOCASE")
        }
    };
    let direction = if sort.descending { "DESC" } else { "ASC" };
    Some(format!("{expr} {direction}, r.id DESC"))
}

fn numeric_sort_expr(source: &ResolvedRef, base: &str, payload_alias: &str) -> String {
    match source {
        ResolvedRef::Column { name, schema_id } => {
            if let Some(column) = derived::num_column_for(name) {
                // Exact materialized numeric value (same one analytics sums).
                source_fields::schema_guard(
                    format!("{payload_alias}.{column}"),
                    payload_alias,
                    *schema_id,
                )
            } else if name == "year" {
                format!("{payload_alias}.year")
            } else {
                format!("num_value_grouped({base})")
            }
        }
        ResolvedRef::Extra { .. } => format!("num_value_grouped({base})"),
    }
}

fn result_order(q: &Query, payload_alias: &str) -> String {
    if uses_fast_result_order(q) {
        "r.id DESC".to_string()
    } else {
        format!("{payload_alias}.declaration_date DESC, r.id DESC")
    }
}

fn uses_fast_result_order(q: &Query) -> bool {
    if q.is_empty() || product_code_search_prefix(&q.text).is_some() {
        return true;
    }
    if !q.text.trim().is_empty() {
        return false;
    }
    let f = &q.filters;
    [
        &f.trademark,
        &f.description,
        &f.sender,
        &f.recipient,
        &f.trade_country,
        &f.dispatch_country,
        &f.origin_country,
    ]
    .iter()
    .all(|value| value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::uses_fast_result_order;
    use crate::db::{Filters, Query, ResultSort};
    use crate::import;
    use crate::search::{FieldInfo, FieldKind, FieldRef};

    /// Test shim: these fields never resolve through a source-schema lookup.
    fn sort_order_sql(fields: &[FieldInfo], sort: &ResultSort, alias: &str) -> Option<String> {
        super::sort_order_sql(fields, sort, alias, &Default::default())
    }

    fn field(id: &str, kind: FieldKind, source: FieldRef) -> FieldInfo {
        FieldInfo {
            id: id.to_string(),
            label: id.to_string(),
            kind,
            source,
            operators: Vec::new(),
        }
    }

    #[test]
    fn sort_order_uses_typed_expressions_per_field_kind() {
        let fields = [
            field(
                "currency_control_value",
                FieldKind::Number,
                FieldRef::Column("currency_control_value".to_string()),
            ),
            field(
                "recipient",
                FieldKind::Text,
                FieldRef::Column("recipient".to_string()),
            ),
            field(
                "extra:Price",
                FieldKind::Number,
                FieldRef::Extra("Price".to_string()),
            ),
        ];

        // A materialized numeric column is sorted by its typed value, not text.
        assert_eq!(
            sort_order_sql(
                &fields,
                &ResultSort {
                    field: "currency_control_value".to_string(),
                    descending: true,
                },
                "r",
            ),
            Some("r.value_num DESC, r.id DESC".to_string())
        );
        // Text sorts case-insensitively.
        assert_eq!(
            sort_order_sql(
                &fields,
                &ResultSort {
                    field: "recipient".to_string(),
                    descending: false,
                },
                "r",
            ),
            Some("r.recipient COLLATE NOCASE ASC, r.id DESC".to_string())
        );
        // Extra numeric columns parse localized numbers for ordering.
        assert_eq!(
            sort_order_sql(
                &fields,
                &ResultSort {
                    field: "extra:Price".to_string(),
                    descending: true,
                },
                "r",
            ),
            Some("num_value_grouped(extra_value(r.extra, 'Price')) DESC, r.id DESC".to_string())
        );
        // An unknown field falls back to the default order.
        assert_eq!(
            sort_order_sql(
                &fields,
                &ResultSort {
                    field: "nope".to_string(),
                    descending: false,
                },
                "r",
            ),
            None
        );
    }

    #[test]
    fn fast_order_is_only_used_for_structural_queries() {
        assert!(uses_fast_result_order(&Query::default()));
        assert!(uses_fast_result_order(&Query {
            filters: Filters {
                year: "2024".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }));
        assert!(!uses_fast_result_order(&Query {
            text: "apple phone".to_string(),
            ..Default::default()
        }));
        assert!(!uses_fast_result_order(&Query {
            filters: Filters {
                sender: "ACME".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }));
    }

    #[test]
    fn snapshot_keeps_offset_pages_and_count_stable_across_an_insert() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("snapshot.db");
        let initial = temp.path().join("initial.csv");
        let later = temp.path().join("later.csv");
        std::fs::write(&initial, "Item,Value\nA,1\nB,2\nC,3\n").unwrap();
        std::fs::write(&later, "Item,Value\nD,4\n").unwrap();
        let cancel = AtomicBool::new(false);
        let mut writer = crate::db::Db::open(&db_path).unwrap();
        let imported = import::import_file(&mut writer, &initial, &cancel, &mut |_, _, _| {});
        assert_eq!(imported.error, None);

        let reader = crate::db::Db::open(&db_path).unwrap();
        let query = Query::default();
        let snapshot = reader.capture_search_snapshot().unwrap();
        let (_, first_ids, _, _) = reader
            .search_page_dynamic_sorted_at_snapshot(&query, 2, 0, None, snapshot)
            .unwrap();
        assert_eq!(first_ids.len(), 2);

        let imported = import::import_file(&mut writer, &later, &cancel, &mut |_, _, _| {});
        assert_eq!(imported.error, None);
        assert_eq!(reader.count_at_snapshot(&query, snapshot).unwrap(), 3);

        let (_, second_ids, _, _) = reader
            .search_page_dynamic_sorted_at_snapshot(&query, 2, 2, None, snapshot)
            .unwrap();
        assert_eq!(second_ids.len(), 1);
        assert!(first_ids.iter().all(|first| !second_ids.contains(first)));
        assert_eq!(reader.count(&query).unwrap(), 4);
    }
}
