use rusqlite::types::Value;

use crate::db::Query;
use crate::domain::table::{SemanticField, TableShape};
use crate::storage::analytics_columns::AnalyticsColumns;
use crate::storage::effective_rows;
use crate::storage::normalize::{normalize_country_key, parse_year};
use crate::storage::search_sql;
use crate::storage::search_text::{
    build_fts_query, fts_prefix_terms, glob_escape, plain_search_terms, product_code_search_prefix,
    search_text_expr_with_prefix,
};
use crate::storage::source_schemas::SourceFieldLookup;

#[derive(Clone)]
pub(crate) struct FilterPlan {
    pub(crate) joins: String,
    pub(crate) where_sql: String,
    pub(crate) params: Vec<Value>,
    pub(crate) payload_alias: &'static str,
}

pub(crate) fn build_filter_plan(
    q: &Query,
    unique_only: bool,
    fts_watermark: i64,
    shape: Option<&TableShape>,
    source_fields: &SourceFieldLookup,
) -> rusqlite::Result<FilterPlan> {
    let (joins, payload_alias) = if unique_only {
        (String::new(), effective_rows::OCCURRENCE_ALIAS)
    } else {
        (
            effective_rows::payload_join().to_string(),
            effective_rows::PAYLOAD_ALIAS,
        )
    };
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    let columns = AnalyticsColumns::for_alias(shape.cloned(), payload_alias);

    // The numeric-prefix shortcut scans the product-code column instead of
    // FTS. It only applies when the data actually has a product code: on a
    // generic table without that semantic, "8517" must stay a global text
    // search so values like "8517-ONLY-IN-INVENTORY" keep matching.
    let has_product_code = match shape {
        None => true,
        Some(shape) => shape
            .columns
            .iter()
            .any(|column| column.semantic == Some(SemanticField::ProductCode)),
    };
    let text_code_prefix = product_code_search_prefix(&q.text).filter(|_| has_product_code);
    let mut match_expr = if text_code_prefix.is_some() {
        String::new()
    } else {
        build_fts_query(&q.text)
    };
    let f = &q.filters;
    let mut contains_clauses: Vec<(String, String)> = Vec::new();
    let trademark = f.trademark.trim();
    if !trademark.is_empty()
        && let Some(terms) = fts_prefix_terms(trademark)
    {
        if !match_expr.is_empty() {
            match_expr.push(' ');
        }
        match_expr.push_str(&terms);
    }
    for (field, value) in [
        (SemanticField::Description, &f.description),
        (SemanticField::Sender, &f.sender),
        (SemanticField::Recipient, &f.recipient),
    ] {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if let Some(terms) = fts_prefix_terms(value) {
            if !match_expr.is_empty() {
                match_expr.push(' ');
            }
            match_expr.push_str(&terms);
        }
        if let Some(expr) = columns.text(field) {
            contains_clauses.push((format!("cyr_contains({expr}, ?)"), value.to_lowercase()));
        }
    }
    if !match_expr.is_empty() {
        let tail_terms = if text_code_prefix.is_none() {
            plain_search_terms(&q.text)
        } else {
            Vec::new()
        };
        let fts_clause = fts_filter_clause(tail_terms.len());
        params.push(match_expr.into());
        params.push(fts_watermark.into());
        for term in tail_terms {
            params.push(term.into());
        }
        clauses.push(fts_clause);
    }
    if let Some(year) = parse_year(&f.year) {
        if let Some(date_month_expr) = columns.month(SemanticField::Date) {
            clauses.push(format!(
                "({payload_alias}.year = ? OR ({payload_alias}.year IS NULL AND CAST(SUBSTR({date_month_expr}, 1, 4) AS INTEGER) = ?))"
            ));
            params.push(year.into());
            params.push(year.into());
        } else {
            clauses.push(format!("{payload_alias}.year = ?"));
            params.push(year.into());
        }
    }
    if let Some(code) = text_code_prefix {
        let expr = columns
            .text(SemanticField::ProductCode)
            .unwrap_or_else(|| format!("{payload_alias}.product_code"));
        clauses.push(format!("{expr} GLOB ?"));
        params.push(format!("{}*", glob_escape(code)).into());
    }
    let code = f.product_code.trim();
    if !code.is_empty() {
        let expr = columns
            .text(SemanticField::ProductCode)
            .unwrap_or_else(|| format!("{payload_alias}.product_code"));
        clauses.push(format!("{expr} GLOB ?"));
        params.push(format!("{}*", glob_escape(code)).into());
    }
    let edrpou = f.edrpou.trim();
    if !edrpou.is_empty() {
        let expr = columns
            .text(SemanticField::CompanyCode)
            .unwrap_or_else(|| format!("{payload_alias}.edrpou"));
        clauses.push(format!("text_key({expr}) = text_key(?)"));
        params.push(edrpou.to_string().into());
    }
    if !trademark.is_empty() {
        let expr = columns
            .text(SemanticField::Trademark)
            .unwrap_or_else(|| format!("{payload_alias}.trademark"));
        clauses.push(format!("text_key({expr}) = text_key(?)"));
        params.push(trademark.to_string().into());
    }
    for (field, fallback_key_col, value) in [
        (SemanticField::TradeCountry, "trade_key", &f.trade_country),
        (
            SemanticField::DispatchCountry,
            "dispatch_key",
            &f.dispatch_country,
        ),
        (
            SemanticField::OriginCountry,
            "origin_key",
            &f.origin_country,
        ),
    ] {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let expr = columns
            .country_key(field)
            .unwrap_or_else(|| format!("{payload_alias}.{fallback_key_col}"));
        clauses.push(format!("{expr} = ?"));
        params.push(normalize_country_key(value).into());
    }
    for (clause, param) in contains_clauses {
        clauses.push(clause);
        params.push(param.into());
    }
    if let Some(advanced) = &q.advanced
        && let Some((clause, advanced_params)) =
            search_sql::compile_query_expr(advanced, shape, payload_alias, source_fields)?
    {
        clauses.push(clause);
        params.extend(advanced_params);
    }
    if unique_only {
        clauses.push(effective_rows::canonical_scope_clause().into());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok(FilterPlan {
        joins,
        where_sql,
        params,
        payload_alias,
    })
}

fn fts_filter_clause(tail_term_count: usize) -> String {
    let mut tail_clauses = vec![
        effective_rows::searchable_payload_clause("tail_owner"),
        "tail_owner.id > ?".to_string(),
    ];
    for _ in 0..tail_term_count {
        tail_clauses.push(format!(
            "cyr_contains({}, ?)",
            search_text_expr_with_prefix("tail_owner.")
        ));
    }
    format!(
        "(r.row_hash IN (
             SELECT fts_owner.row_hash
             FROM records_fts
             JOIN records fts_owner ON fts_owner.id = records_fts.rowid
             WHERE records_fts MATCH ?
         ) OR r.row_hash IN (
             SELECT tail_owner.row_hash
             FROM records tail_owner
             WHERE {}
         ))",
        tail_clauses.join(" AND ")
    )
}

#[cfg(test)]
mod tests {
    use crate::db::{Filters, Query};
    use crate::domain::table::TableShape;
    use crate::search::{ConditionOp, ConditionValue, FieldRef, QueryCondition, QueryExpr};

    /// Test shim: plans here never involve registered source-schema fields.
    fn build_filter_plan(
        q: &Query,
        unique_only: bool,
        fts_watermark: i64,
        shape: Option<&TableShape>,
    ) -> rusqlite::Result<super::FilterPlan> {
        super::build_filter_plan(q, unique_only, fts_watermark, shape, &Default::default())
    }

    #[test]
    fn occurrence_plan_joins_effective_payload() {
        let plan = build_filter_plan(&Query::default(), false, 0, None).unwrap();
        assert!(plan.joins.contains("JOIN records p"));
        assert!(plan.where_sql.is_empty());
        assert!(plan.params.is_empty());
        assert_eq!(plan.payload_alias, "p");
    }

    #[test]
    fn unique_plan_filters_duplicates() {
        let plan = build_filter_plan(&Query::default(), true, 0, None).unwrap();
        assert_eq!(plan.where_sql, " WHERE r.dup_first_file IS NULL");
        assert!(plan.joins.is_empty());
        assert_eq!(plan.payload_alias, "r");
    }

    #[test]
    fn product_code_text_uses_range_scan_without_fts() {
        let plan = build_filter_plan(
            &Query {
                text: "8504".to_string(),
                ..Default::default()
            },
            true,
            0,
            None,
        )
        .unwrap();
        assert!(plan.joins.is_empty());
        assert!(plan.where_sql.contains("r.product_code GLOB ?"));
        assert_eq!(plan.params.len(), 1);
    }

    #[test]
    fn text_query_uses_fts_and_unindexed_tail() {
        let plan = build_filter_plan(
            &Query {
                text: "apple phone".to_string(),
                ..Default::default()
            },
            true,
            42,
            None,
        )
        .unwrap();
        assert!(plan.where_sql.contains("records_fts MATCH"));
        assert!(plan.where_sql.contains("tail_owner.id > ?"));
        assert!(plan.joins.is_empty());
        assert!(plan.params.len() >= 2);
    }

    #[test]
    fn occurrence_text_query_expands_matching_payload_hashes() {
        let plan = build_filter_plan(
            &Query {
                text: "apple phone".to_string(),
                record_scope: crate::db::RecordScope::Occurrences,
                ..Default::default()
            },
            false,
            42,
            None,
        )
        .unwrap();

        assert!(plan.where_sql.contains("r.row_hash IN"));
        assert!(plan.where_sql.contains("fts_owner.row_hash"));
    }

    #[test]
    fn structured_filters_are_added_to_plan() {
        let plan = build_filter_plan(
            &Query {
                filters: Filters {
                    year: "2024".to_string(),
                    edrpou: "12345678".to_string(),
                    origin_country: "CN".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
            0,
            None,
        )
        .unwrap();
        assert!(plan.where_sql.contains("r.year"));
        assert!(plan.where_sql.contains(" = ?"));
        assert!(plan.where_sql.contains("text_key(r.edrpou) = text_key(?)"));
        assert!(plan.where_sql.contains("r.origin_key = ?"));
    }

    #[test]
    fn schema_backed_year_filter_keeps_an_indexable_year_branch() {
        let plan = build_filter_plan(
            &Query {
                filters: Filters {
                    year: "2024".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
            0,
            None,
        )
        .unwrap();

        assert!(
            plan.where_sql.contains("r.year = ? OR (r.year IS NULL"),
            "year plan must expose r.year to the SQLite index: {}",
            plan.where_sql
        );
    }

    #[test]
    fn structured_filters_use_extra_backed_semantics() {
        let shape = TableShape::from_headers([
            "Order Date".to_string(),
            "Item code".to_string(),
            "Importer".to_string(),
            "Brand".to_string(),
            "Origin country".to_string(),
        ]);
        let plan = build_filter_plan(
            &Query {
                filters: Filters {
                    year: "2024".to_string(),
                    product_code: "8517".to_string(),
                    recipient: "Apple".to_string(),
                    trademark: "Apple".to_string(),
                    origin_country: "China".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
            0,
            Some(&shape),
        )
        .unwrap();

        assert!(plan.where_sql.contains("month_key(extra_value"));
        assert!(
            plan.where_sql
                .contains("extra_value(r.extra, 'Item code') GLOB ?")
        );
        assert!(
            plan.where_sql
                .contains("cyr_contains(extra_value(r.extra, 'Importer'), ?)")
        );
        assert!(
            plan.where_sql
                .contains("text_key(extra_value(r.extra, 'Brand'))")
        );
        assert!(
            plan.where_sql
                .contains("country_key(extra_value(r.extra, 'Origin country'))")
        );
    }

    #[test]
    fn advanced_filters_use_shape_field_kinds_for_extra_columns() {
        let shape = TableShape::from_headers([
            "Product Name".to_string(),
            "Value USD".to_string(),
            "Origin country".to_string(),
        ]);
        let plan = build_filter_plan(
            &Query {
                advanced: Some(QueryExpr::Condition(QueryCondition {
                    field: FieldRef::Extra("Value USD".to_string()),
                    op: ConditionOp::Range,
                    value: ConditionValue::Range {
                        from: Some("100".to_string()),
                        to: Some("250".to_string()),
                    },
                    negated: false,
                })),
                ..Default::default()
            },
            true,
            0,
            Some(&shape),
        )
        .unwrap();

        // "Value USD" carries the Value semantic, so its numeric filter must
        // parse the same grouped style the column is stored and aggregated
        // with — otherwise "value >= 1,250" would disagree with the totals.
        assert!(plan.where_sql.contains("num_value_grouped(extra_value"));
        assert_eq!(plan.params.len(), 4);
    }

    #[test]
    fn advanced_schema_country_uses_its_materialized_key() {
        let plan = build_filter_plan(
            &Query {
                advanced: Some(QueryExpr::Condition(QueryCondition {
                    field: FieldRef::Column("origin_country".to_string()),
                    op: ConditionOp::Equals,
                    value: ConditionValue::Single("China".to_string()),
                    negated: false,
                })),
                ..Default::default()
            },
            true,
            0,
            None,
        )
        .unwrap();

        assert!(plan.where_sql.contains("r.origin_key = ?"));
        assert!(!plan.where_sql.contains("country_key(r.origin_country)"));
    }
}
