use std::collections::HashMap;

use rusqlite::{Connection, params_from_iter};

use crate::db::{
    AnalyticsCurrencyTotal, AnalyticsFilterAction, AnalyticsFilterField, AnalyticsGroupRow,
    AnalyticsMeasureExclusions, AnalyticsMeasures, AnalyticsMonthRow, AnalyticsOverview,
    AnalyticsPriceMetric, AnalyticsSection, AnalyticsSectionKind, AnalyticsUsdCompatibility,
    AnalyticsValuePerWeight, AnalyticsWeightTotal, CompanyProfile, PivotCompatibilityMatrix,
    PivotDim, PivotLimits, PivotMetric, PivotResult, PriceMetricKind, PriceRiskContract, Query,
    RiskCohort, RiskConfidence, RiskCurrencyTotal, RiskExclusions, RiskLimitation, Undervaluation,
    UndervaluedRow,
};
use crate::domain::table::SemanticField;
use crate::storage::analytics_columns::{AnalyticsColumns, UNKNOWN_CURRENCY_KEY, UNKNOWN_UNIT_KEY};
use crate::storage::effective_rows;
use crate::storage::query_plan::{self, FilterPlan};
use crate::storage::source_schemas;
use crate::storage::table_shape;

/// Analytics column resolver for the effective shape, including the
/// schema-level fixed currency/weight-unit values chosen at import.
fn columns_for(conn: &Connection, row_alias: &str) -> AnalyticsColumns {
    let (fixed_currency, fixed_weight_unit) =
        source_schemas::merged_fixed_values(conn).unwrap_or((None, None));
    AnalyticsColumns::for_alias(table_shape::effective(conn), row_alias)
        .with_fixed_values(fixed_currency, fixed_weight_unit)
}

/// Merges an extra condition into a plan's rendered WHERE clause.
fn and_where(where_sql: &str, condition: &str) -> String {
    if where_sql.is_empty() {
        format!(" WHERE {condition}")
    } else {
        format!("{where_sql} AND {condition}")
    }
}

fn weight_factor(unit_key: &str) -> Option<f64> {
    match unit_key {
        "kg" => Some(1.0),
        "g" => Some(0.001),
        "tonne" => Some(1000.0),
        "lb" => Some(0.45359237),
        _ => None,
    }
}

fn currency_is_known(key: &str) -> bool {
    !key.is_empty() && !key.starts_with(UNKNOWN_CURRENCY_KEY)
}

/// Currency- and unit-safe measures for the filtered row set: per-currency
/// value buckets, per-unit weight buckets, value-per-kg pairs, and exclusion
/// counters. Money is never added across currency buckets. Runs five focused
/// aggregate scans; only the query-level overview pays this cost — group and
/// month rows inherit compatibility from it instead of re-bucketing.
fn measures_for_plan(conn: &Connection, plan: &FilterPlan) -> rusqlite::Result<AnalyticsMeasures> {
    let cols = columns_for(conn, plan.payload_alias);
    let m = cols.measures();
    let joins = &plan.joins;
    let value = &m.value;
    let cur = &m.currency_key;

    // 1. Per-currency value totals.
    let currency_totals: Vec<AnalyticsCurrencyTotal> = {
        let filter = and_where(&plan.where_sql, &format!("{value} IS NOT NULL"));
        let sql = format!(
            "SELECT {cur} AS bucket_currency, COUNT(*) AS n,
                    COALESCE(SUM({value}), 0.0) AS total
             FROM records r{joins}{filter}
             GROUP BY bucket_currency ORDER BY total DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(plan.params.clone()), |row| {
            let currency: String = row.get(0)?;
            Ok(AnalyticsCurrencyTotal {
                known: currency_is_known(&currency),
                currency,
                valued_rows: row.get::<_, i64>(1)? as u64,
                total_value: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // 2-3. Per-unit weight totals (net, then gross).
    let weight_buckets =
        |weight_expr: &str, kg_expr: &str| -> rusqlite::Result<Vec<AnalyticsWeightTotal>> {
            let unit = &m.weight_unit_key;
            let filter = and_where(&plan.where_sql, &format!("{weight_expr} IS NOT NULL"));
            // Aliases avoid the customs schema's own column names ("unit").
            let sql = format!(
                "SELECT {unit} AS bucket_unit, COUNT(*) AS n,
                    COALESCE(SUM({weight_expr}), 0.0) AS raw_total,
                    SUM({kg_expr}) AS kg_total
             FROM records r{joins}{filter}
             GROUP BY bucket_unit ORDER BY raw_total DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(plan.params.clone()), |row| {
                let source_unit: String = row.get(0)?;
                let factor = weight_factor(&source_unit);
                Ok(AnalyticsWeightTotal {
                    known: factor.is_some(),
                    normalized_unit: factor.map(|_| "kg".to_string()),
                    factor_to_kg: factor,
                    source_unit,
                    weighted_rows: row.get::<_, i64>(1)? as u64,
                    total_source_weight: row.get(2)?,
                    total_kg: row.get(3)?,
                })
            })?;
            rows.collect()
        };
    let net_weight_totals = weight_buckets(&m.net_weight, &m.net_weight_kg)?;
    let gross_weight_totals = weight_buckets(&m.gross_weight, &m.gross_weight_kg)?;

    // 4. Value-per-net-kg pairs, bucketed by currency (weights normalized to kg).
    let value_per_net_weight: Vec<AnalyticsValuePerWeight> = {
        let net_kg = &m.net_weight_kg;
        let filter = and_where(
            &plan.where_sql,
            &format!("{value} IS NOT NULL AND ({net_kg}) IS NOT NULL AND ({net_kg}) > 0"),
        );
        let sql = format!(
            "SELECT {cur} AS bucket_currency, COUNT(*) AS n,
                    COALESCE(SUM({value}), 0.0) AS total_value,
                    COALESCE(SUM({net_kg}), 0.0) AS total_kg,
                    GROUP_CONCAT(DISTINCT {unit})
             FROM records r{joins}{filter}
             GROUP BY bucket_currency ORDER BY total_value DESC",
            unit = m.weight_unit_key,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(plan.params.clone()), |row| {
            let currency: String = row.get(0)?;
            let total_value: f64 = row.get(2)?;
            let total_weight: f64 = row.get(3)?;
            let units: Option<String> = row.get(4)?;
            Ok(AnalyticsValuePerWeight {
                currency,
                normalized_weight_unit: "kg".to_string(),
                source_weight_units: units
                    .unwrap_or_default()
                    .split(',')
                    .filter(|unit| !unit.is_empty())
                    .map(str::to_string)
                    .collect(),
                paired_rows: row.get::<_, i64>(1)? as u64,
                total_value,
                total_weight,
                value_per_weight: (total_weight > 0.0).then(|| total_value / total_weight),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // 5. Exclusion counters, one conditional scan.
    let exclusions = {
        let unknown_cur = format!("{cur} GLOB '{UNKNOWN_CURRENCY_KEY}*'");
        let unknown_unit = format!(
            "{unit} GLOB '{UNKNOWN_UNIT_KEY}*'",
            unit = m.weight_unit_key
        );
        let net = &m.net_weight;
        let gross = &m.gross_weight;
        let sql = format!(
            "SELECT
                COALESCE(SUM(CASE WHEN {value} IS NOT NULL AND {unknown_cur} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {net} IS NOT NULL AND {unknown_unit} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {gross} IS NOT NULL AND {unknown_unit} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {value} IS NOT NULL AND {net} IS NOT NULL
                    AND {unknown_cur} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {value} IS NOT NULL AND {net} IS NOT NULL
                    AND {unknown_unit} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {value} IS NOT NULL
                    AND ({net} IS NULL OR {net} <= 0) THEN 1 ELSE 0 END), 0)
             FROM records r{joins}{where_sql}",
            where_sql = plan.where_sql,
        );
        conn.query_row(&sql, params_from_iter(plan.params.clone()), |row| {
            Ok(AnalyticsMeasureExclusions {
                value_without_known_currency: row.get::<_, i64>(0)? as u64,
                net_weight_without_known_unit: row.get::<_, i64>(1)? as u64,
                gross_weight_without_known_unit: row.get::<_, i64>(2)? as u64,
                ratio_without_known_currency: row.get::<_, i64>(3)? as u64,
                ratio_without_known_weight_unit: row.get::<_, i64>(4)? as u64,
                ratio_with_zero_or_missing_weight: row.get::<_, i64>(5)? as u64,
            })
        })?
    };

    // The whole result set is currency-compatible only when every valued row
    // sits in one known currency bucket.
    let compatible_value_total = (currency_totals.len() == 1 && currency_totals[0].known)
        .then(|| currency_totals[0].clone());
    let compatible_value_per_net_weight = compatible_value_total.as_ref().and_then(|total| {
        value_per_net_weight
            .iter()
            .find(|pair| pair.currency == total.currency)
            .cloned()
    });

    Ok(AnalyticsMeasures {
        currency_totals,
        net_weight_totals,
        gross_weight_totals,
        value_per_net_weight,
        compatible_value_total,
        compatible_value_per_net_weight,
        exclusions,
    })
}

/// Wire-compatible USD fields, flattened only for a single known USD cohort.
fn usd_compatibility(measures: &AnalyticsMeasures) -> Option<AnalyticsUsdCompatibility> {
    let total = measures
        .compatible_value_total
        .as_ref()
        .filter(|total| total.known && total.currency == "USD")?;
    Some(AnalyticsUsdCompatibility {
        total_value_usd: total.total_value,
        avg_value_per_net_kg: measures.compatible_usd_per_net_kg(),
    })
}

/// Compatibility for a subset (a month or a group row) of a query whose whole
/// row set is a single known USD cohort: the subset inherits the cohort, so
/// its plain sums are honest USD numbers.
fn inherited_usd(
    query_is_usd: bool,
    total_value: f64,
    total_net_kg: f64,
) -> Option<AnalyticsUsdCompatibility> {
    query_is_usd.then(|| AnalyticsUsdCompatibility {
        total_value_usd: total_value,
        avg_value_per_net_kg: (total_net_kg > 0.0).then(|| total_value / total_net_kg),
    })
}

pub(crate) fn overview(conn: &Connection, plan: FilterPlan) -> rusqlite::Result<AnalyticsOverview> {
    let measures = measures_for_plan(conn, &plan)?;
    let payload_alias = plan.payload_alias;
    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    // Columns are resolved through the recorded table shape: typed customs
    // columns for recognized fields, or normalized expressions over the `extra`
    // JSON for fields the user assigned on a generic table.
    let cols = columns_for(conn, payload_alias);
    let label = |field| cols.label(field).unwrap_or_else(|| "''".to_string());
    let country = |field| cols.country_key(field).unwrap_or_else(|| "''".to_string());
    let number = |field| cols.number(field).unwrap_or_else(|| "NULL".to_string());
    let sender = label(SemanticField::Sender);
    let recipient = label(SemanticField::Recipient);
    let edrpou = label(SemanticField::CompanyCode);
    let declaration = label(SemanticField::DeclarationNumber);
    let trademark = label(SemanticField::Trademark);
    let product = label(SemanticField::ProductCode);
    let origin = country(SemanticField::OriginCountry);
    let dispatch = country(SemanticField::DispatchCountry);
    let trade = country(SemanticField::TradeCountry);
    let value = number(SemanticField::Value);
    let gross = number(SemanticField::GrossWeight);
    let net = number(SemanticField::NetWeight);
    let quantity = number(SemanticField::Quantity);
    let sql = format!(
        "SELECT
            COUNT(*),
            COUNT(DISTINCT NULLIF({declaration}, '')),
            COUNT(DISTINCT NULLIF({sender}, '')),
            COUNT(DISTINCT NULLIF({recipient}, '')),
            COUNT(DISTINCT NULLIF({edrpou}, '')),
            COUNT(DISTINCT NULLIF({trademark}, '')),
            COUNT(DISTINCT NULLIF({product}, '')),
            COUNT(DISTINCT NULLIF({origin}, '')),
            COUNT(DISTINCT NULLIF({dispatch}, '')),
            COUNT(DISTINCT NULLIF({trade}, '')),
            SUM({value}),
            SUM({gross}),
            SUM({net}),
            SUM({quantity})
         FROM records r{joins}{where_sql}"
    );
    let overview = conn.query_row(&sql, params_from_iter(params), |row| {
        Ok(AnalyticsOverview {
            row_count: row.get::<_, i64>(0)? as u64,
            declaration_count: row.get::<_, i64>(1)? as u64,
            distinct_senders: row.get::<_, i64>(2)? as u64,
            distinct_recipients: row.get::<_, i64>(3)? as u64,
            distinct_edrpou: row.get::<_, i64>(4)? as u64,
            distinct_trademarks: row.get::<_, i64>(5)? as u64,
            distinct_product_codes: row.get::<_, i64>(6)? as u64,
            distinct_origin_countries: row.get::<_, i64>(7)? as u64,
            distinct_dispatch_countries: row.get::<_, i64>(8)? as u64,
            distinct_trade_countries: row.get::<_, i64>(9)? as u64,
            total_value_usd: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
            total_gross_kg: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
            total_net_kg: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
            total_quantity: row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
            avg_value_per_net_kg: 0.0,
            compatible_usd: None,
            measures: AnalyticsMeasures::default(),
        })
    })?;
    Ok(AnalyticsOverview {
        avg_value_per_net_kg: ratio(overview.total_value_usd, overview.total_net_kg),
        compatible_usd: usd_compatibility(&measures),
        measures,
        ..overview
    })
}

pub(crate) fn months(
    conn: &Connection,
    plan: FilterPlan,
    query_is_usd: bool,
) -> rusqlite::Result<Vec<AnalyticsMonthRow>> {
    let payload_alias = plan.payload_alias;
    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    let cols = columns_for(conn, payload_alias);
    let month = cols
        .month(SemanticField::Date)
        .unwrap_or_else(|| "''".to_string());
    let declaration = cols
        .label(SemanticField::DeclarationNumber)
        .unwrap_or_else(|| "''".to_string());
    let value = cols
        .number(SemanticField::Value)
        .unwrap_or_else(|| "NULL".to_string());
    let net = cols
        .number(SemanticField::NetWeight)
        .unwrap_or_else(|| "NULL".to_string());
    let month_filter = format!("{month} <> ''");
    let filter_sql = if where_sql.is_empty() {
        format!(" WHERE {month_filter}")
    } else {
        format!("{where_sql} AND {month_filter}")
    };
    let sql = format!(
        "SELECT
            {month} AS month,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF({declaration}, '')) AS declarations_count,
            COALESCE(SUM({value}), 0.0) AS total_value_usd,
            COALESCE(SUM({net}), 0.0) AS total_net_kg
         FROM records r{joins}{filter_sql}
         GROUP BY {month}
         ORDER BY {month} DESC
         LIMIT 48"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        let total_value_usd: f64 = row.get(3)?;
        let total_net_kg: f64 = row.get(4)?;
        Ok(AnalyticsMonthRow {
            month: row.get(0)?,
            rows: row.get::<_, i64>(1)? as u64,
            declarations: row.get::<_, i64>(2)? as u64,
            total_value_usd,
            total_net_kg,
            compatible_usd: inherited_usd(query_is_usd, total_value_usd, total_net_kg),
            measures: AnalyticsMeasures::default(),
        })
    })?;
    let mut months: Vec<AnalyticsMonthRow> = rows.flatten().collect();
    months.reverse();
    Ok(months)
}

pub(crate) fn section(
    conn: &Connection,
    plan: FilterPlan,
    kind: AnalyticsSectionKind,
    hs_level: u8,
    limit: u64,
    overview: &AnalyticsOverview,
) -> rusqlite::Result<AnalyticsSection> {
    let cols = columns_for(conn, plan.payload_alias);
    let Some(grouping) = section_grouping(&cols, kind, hs_level) else {
        return Ok(AnalyticsSection {
            kind,
            rows: Vec::new(),
        });
    };
    let label_sql = grouping.label_sql;
    let declaration = cols
        .label(SemanticField::DeclarationNumber)
        .unwrap_or_else(|| "''".to_string());
    let company = cols
        .label(SemanticField::CompanyCode)
        .unwrap_or_else(|| "''".to_string());
    let value = cols
        .number(SemanticField::Value)
        .unwrap_or_else(|| "NULL".to_string());
    let net = cols
        .number(SemanticField::NetWeight)
        .unwrap_or_else(|| "NULL".to_string());
    let gross = cols
        .number(SemanticField::GrossWeight)
        .unwrap_or_else(|| "NULL".to_string());
    let quantity = cols
        .number(SemanticField::Quantity)
        .unwrap_or_else(|| "NULL".to_string());
    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let mut params = plan.params;
    let non_empty = format!("{label_sql} <> ''");
    let filter_sql = if where_sql.is_empty() {
        format!(" WHERE {non_empty}")
    } else {
        format!("{where_sql} AND {non_empty}")
    };
    let sql = format!(
        "SELECT
            {label_sql} AS label,
            COUNT(*) AS rows_count,
            COUNT(DISTINCT NULLIF({declaration}, '')) AS declarations_count,
            COUNT(DISTINCT NULLIF({company}, '')) AS companies_count,
            COALESCE(SUM({value}), 0.0) AS total_value_usd,
            COALESCE(SUM({net}), 0.0) AS total_net_kg,
            COALESCE(SUM({gross}), 0.0) AS total_gross_kg,
            COALESCE(SUM({quantity}), 0.0) AS total_quantity
         FROM records r{joins}{filter_sql}
         GROUP BY {label_sql}
         ORDER BY total_value_usd DESC, total_net_kg DESC, rows_count DESC, label COLLATE NOCASE
         LIMIT ?"
    );
    params.push((limit as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        let label: String = row.get(0)?;
        let total_value_usd: f64 = row.get(4)?;
        let total_net_kg: f64 = row.get(5)?;
        let total_gross_kg: f64 = row.get(6)?;
        let total_quantity: f64 = row.get(7)?;
        let share_base = if overview.total_value_usd > 0.0 {
            overview.total_value_usd
        } else if overview.total_net_kg > 0.0 {
            overview.total_net_kg
        } else {
            overview.row_count as f64
        };
        let share_value = if overview.total_value_usd > 0.0 {
            total_value_usd
        } else if overview.total_net_kg > 0.0 {
            total_net_kg
        } else {
            row.get::<_, i64>(1)? as f64
        };
        Ok(AnalyticsGroupRow {
            filter_action: grouping.filter_field.map(|field| AnalyticsFilterAction {
                field,
                value: label.clone(),
            }),
            label,
            rows: row.get::<_, i64>(1)? as u64,
            declarations: row.get::<_, i64>(2)? as u64,
            companies: row.get::<_, i64>(3)? as u64,
            total_value_usd,
            total_net_kg,
            total_gross_kg,
            total_quantity,
            share_percent: ratio(share_value * 100.0, share_base),
            avg_value_per_net_kg: ratio(total_value_usd, total_net_kg),
            compatible_usd: inherited_usd(
                overview.compatible_usd.is_some(),
                total_value_usd,
                total_net_kg,
            ),
            measures: AnalyticsMeasures::default(),
        })
    })?;
    Ok(AnalyticsSection {
        kind,
        rows: rows.collect::<rusqlite::Result<Vec<_>>>()?,
    })
}

pub(crate) fn price_metrics(
    conn: &Connection,
    q: &Query,
    fts_watermark: i64,
) -> rusqlite::Result<Vec<AnalyticsPriceMetric>> {
    let shape = table_shape::effective(conn);
    let source_fields = source_schemas::field_lookup(conn)?;
    let plan = query_plan::build_filter_plan(
        q,
        q.record_scope == crate::db::RecordScope::Canonical,
        fts_watermark,
        shape.as_ref(),
        &source_fields,
    )?;
    let cols = AnalyticsColumns::for_alias(shape, plan.payload_alias);
    price_metrics_for_plan(conn, plan, &cols)
}

pub(crate) fn company_profile(
    conn: &Connection,
    identifier: &str,
    limit: u64,
) -> rusqlite::Result<CompanyProfile> {
    let identifier = identifier.trim();
    let cols = AnalyticsColumns::for_alias(
        table_shape::effective(conn),
        effective_rows::OCCURRENCE_ALIAS,
    );
    let Some(company) = cols.label(SemanticField::CompanyCode) else {
        return Ok(CompanyProfile {
            edrpou: identifier.to_string(),
            ..Default::default()
        });
    };
    let plan = FilterPlan {
        joins: String::new(),
        where_sql: format!(
            " WHERE {company} = label_value(?) AND {}",
            effective_rows::canonical_scope_clause()
        ),
        params: vec![identifier.to_string().into()],
        payload_alias: effective_rows::OCCURRENCE_ALIAS,
    };
    let overview = overview(conn, plan.clone())?;
    let months = months(conn, plan.clone(), overview.compatible_usd.is_some())?;
    let product_sections = vec![
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::ProductCodes,
            10,
            limit,
            &overview,
        )?,
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::Trademarks,
            10,
            limit,
            &overview,
        )?,
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::ProductGroups,
            10,
            limit,
            &overview,
        )?,
    ];
    let country_sections = vec![
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::OriginCountries,
            10,
            limit,
            &overview,
        )?,
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::DispatchCountries,
            10,
            limit,
            &overview,
        )?,
        section(
            conn,
            plan.clone(),
            AnalyticsSectionKind::TradeCountries,
            10,
            limit,
            &overview,
        )?,
    ];
    let price_sections = price_metrics_for_plan(conn, plan.clone(), &cols)?;
    let top_products = section_rows(&product_sections, AnalyticsSectionKind::ProductCodes);
    let top_origin_countries =
        section_rows(&country_sections, AnalyticsSectionKind::OriginCountries);
    let top_senders = section(
        conn,
        plan.clone(),
        AnalyticsSectionKind::Senders,
        10,
        limit,
        &overview,
    )?
    .rows;
    let names = profile_names(conn, plan, &cols)?;

    Ok(CompanyProfile {
        edrpou: identifier.to_string(),
        names,
        overview,
        months,
        top_products,
        top_senders,
        top_origin_countries,
        product_sections,
        country_sections,
        price_sections,
    })
}

fn section_rows(
    sections: &[AnalyticsSection],
    kind: AnalyticsSectionKind,
) -> Vec<AnalyticsGroupRow> {
    sections
        .iter()
        .find(|section| section.kind == kind)
        .map(|section| section.rows.clone())
        .unwrap_or_default()
}

fn profile_names(
    conn: &Connection,
    plan: FilterPlan,
    cols: &AnalyticsColumns,
) -> rusqlite::Result<Vec<String>> {
    let Some(recipient) = cols.label(SemanticField::Recipient) else {
        return Ok(Vec::new());
    };
    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    let filter_sql = if where_sql.is_empty() {
        format!(" WHERE {recipient} <> ''")
    } else {
        format!("{where_sql} AND {recipient} <> ''")
    };
    let sql = format!(
        "SELECT {recipient} AS name, COUNT(*) AS n
         FROM records r{joins}{filter_sql}
         GROUP BY {recipient}
         ORDER BY n DESC, name COLLATE NOCASE
         LIMIT 8"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0))?;
    rows.collect()
}

fn price_metrics_for_plan(
    conn: &Connection,
    plan: FilterPlan,
    cols: &AnalyticsColumns,
) -> rusqlite::Result<Vec<AnalyticsPriceMetric>> {
    let value = cols
        .number(SemanticField::Value)
        .unwrap_or_else(|| "NULL".to_string());
    let net = cols
        .number(SemanticField::NetWeight)
        .unwrap_or_else(|| "NULL".to_string());
    let gross = cols
        .number(SemanticField::GrossWeight)
        .unwrap_or_else(|| "NULL".to_string());
    let quantity = cols
        .number(SemanticField::Quantity)
        .unwrap_or_else(|| "NULL".to_string());
    let rfv = cols.raw_column("rfv_num");
    let rmv_net = cols.raw_column("rmv_net_num");
    let rmv_extra = cols.raw_column("rmv_extra_num");
    let rmv_gross = cols.raw_column("rmv_gross_num");
    let min_base = cols.raw_column("min_base_num");
    let metric = |kind, price_expr, denominator| {
        price_metric(conn, plan.clone(), kind, price_expr, denominator)
    };
    Ok(vec![
        metric(
            PriceMetricKind::ValuePerNetKg,
            &format!(
                "CASE
                    WHEN {value} IS NOT NULL
                        AND {net} IS NOT NULL
                        AND {net} > 0
                    THEN {value} / {net}
                 END"
            ),
            &net,
        )?,
        metric(PriceMetricKind::RfvUsdKg, &rfv, &net)?,
        metric(PriceMetricKind::RmvNetUsdKg, &rmv_net, &net)?,
        metric(PriceMetricKind::RmvUsdExtraUnit, &rmv_extra, &quantity)?,
        metric(PriceMetricKind::RmvGrossUsdKg, &rmv_gross, &gross)?,
        metric(PriceMetricKind::MinBaseUsdKg, &min_base, &net)?,
    ])
}

pub(crate) fn pivot(
    conn: &Connection,
    plan: FilterPlan,
    row_dim: PivotDim,
    col_dim: PivotDim,
    metric: PivotMetric,
    limits: PivotLimits,
    others_label: &str,
) -> rusqlite::Result<PivotResult> {
    let cols = columns_for(conn, plan.payload_alias);
    let Some(row_dim_sql) = pivot_dim_sql(&cols, row_dim) else {
        return Ok(empty_pivot());
    };
    let Some(col_dim_sql) = pivot_dim_sql(&cols, col_dim) else {
        return Ok(empty_pivot());
    };
    let Some(metric_sql) = pivot_metric_sql(&cols, metric) else {
        return Ok(empty_pivot());
    };

    // A metric unit the whole matrix can honestly share: rows always count as
    // one unit; money and weight qualify only when every contributing row sits
    // in a single known currency/unit bucket.
    let metric_unit = pivot_metric_unit(conn, &plan, &cols, metric)?;

    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    let row_sql = row_dim_sql.expr;
    let col_sql = col_dim_sql.expr;
    let non_empty = format!("{row_sql} <> '' AND {col_sql} <> ''");
    let filter_sql = if where_sql.is_empty() {
        format!(" WHERE {non_empty}")
    } else {
        format!("{where_sql} AND {non_empty}")
    };
    let sql = format!(
        "SELECT {row_sql} AS rk, {col_sql} AS ck, {metric_sql} AS v
         FROM records r{joins}{filter_sql}
         GROUP BY {row_sql}, {col_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;

    let mut row_totals: HashMap<String, f64> = HashMap::new();
    let mut col_totals: HashMap<String, f64> = HashMap::new();
    let mut triples: Vec<(String, String, f64)> = Vec::new();
    while let Some(row) = rows.next()? {
        let rk: String = row.get(0)?;
        let ck: String = row.get(1)?;
        let v: f64 = row.get(2)?;
        *row_totals.entry(rk.clone()).or_default() += v;
        *col_totals.entry(ck.clone()).or_default() += v;
        triples.push((rk, ck, v));
    }

    let col_chrono = matches!(col_dim, PivotDim::Month | PivotDim::Year);
    let (row_labels, rows_truncated) = rank_pivot_labels(&row_totals, limits.rows, false);
    let (col_labels, cols_truncated) = rank_pivot_labels(&col_totals, limits.cols, col_chrono);

    let row_index: HashMap<&str, usize> = row_labels
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let col_index: HashMap<&str, usize> = col_labels
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();

    let n_rows = row_labels.len() + usize::from(rows_truncated);
    let n_cols = col_labels.len() + usize::from(cols_truncated);
    let others_row = row_labels.len();
    let others_col = col_labels.len();
    let mut cells = vec![vec![0.0_f64; n_cols]; n_rows];
    for (rk, ck, v) in triples {
        let ri = row_index.get(rk.as_str()).copied().unwrap_or(others_row);
        let ci = col_index.get(ck.as_str()).copied().unwrap_or(others_col);
        if ri < n_rows && ci < n_cols {
            cells[ri][ci] += v;
        }
    }

    let mut final_row_labels = row_labels;
    if rows_truncated {
        final_row_labels.push(others_label.to_string());
    }
    let mut final_col_labels = col_labels;
    if cols_truncated {
        final_col_labels.push(others_label.to_string());
    }
    let row_totals: Vec<f64> = cells.iter().map(|r| r.iter().sum()).collect();
    let mut col_totals = vec![0.0_f64; n_cols];
    for r in &cells {
        for (ci, v) in r.iter().enumerate() {
            col_totals[ci] += v;
        }
    }
    let grand_total: f64 = row_totals.iter().sum();

    let compatible_matrix = metric_unit.map(|unit| PivotCompatibilityMatrix {
        cells: cells.clone(),
        row_totals: row_totals.clone(),
        col_totals: col_totals.clone(),
        grand_total,
        metric_unit: unit,
    });

    Ok(PivotResult {
        row_labels: final_row_labels,
        col_labels: final_col_labels,
        cells,
        row_totals,
        col_totals,
        grand_total,
        compatible_matrix,
        // Per-bucket matrices for mixed-currency/unit data are not computed
        // yet; an empty list means "no partition detail", never zeroes.
        partitions: Vec::new(),
        rows_truncated,
        cols_truncated,
        row_filterable: row_dim_sql.filterable,
        col_filterable: col_dim_sql.filterable,
    })
}

/// The single honest unit for a pivot matrix, or `None` when the filtered rows
/// mix currencies/units and a flat matrix would add incompatible numbers.
fn pivot_metric_unit(
    conn: &Connection,
    plan: &FilterPlan,
    cols: &AnalyticsColumns,
    metric: PivotMetric,
) -> rusqlite::Result<Option<String>> {
    let measures = cols.measures();
    let (key_expr, present_expr, known) = match metric {
        PivotMetric::Rows => return Ok(Some("rows".to_string())),
        PivotMetric::Value => (
            measures.currency_key.clone(),
            measures.value.clone(),
            currency_is_known as fn(&str) -> bool,
        ),
        PivotMetric::NetKg => (
            measures.weight_unit_key.clone(),
            measures.net_weight.clone(),
            (|key: &str| weight_factor(key).is_some()) as fn(&str) -> bool,
        ),
    };
    let filter = and_where(&plan.where_sql, &format!("({present_expr}) IS NOT NULL"));
    let sql = format!(
        "SELECT DISTINCT {key_expr} FROM records r{joins}{filter} LIMIT 2",
        joins = plan.joins,
    );
    let mut stmt = conn.prepare(&sql)?;
    let keys = stmt
        .query_map(params_from_iter(plan.params.clone()), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match keys.as_slice() {
        // No contributing rows at all: an all-zero matrix is trivially honest.
        [] => Ok(Some(match metric {
            PivotMetric::NetKg => "kg".to_string(),
            _ => "USD".to_string(),
        })),
        [key] if known(key) => Ok(Some(key.clone())),
        _ => Ok(None),
    }
}

const RISK_MIN_SAMPLES: u64 = 20;
const RISK_MAX_SAMPLES: u64 = 1_000;
const RISK_IQR_MULTIPLIER: f64 = 1.5;

fn risk_contract(threshold: f64, min_samples: u64) -> PriceRiskContract {
    PriceRiskContract {
        price_basis: "mapped value / mapped net weight".to_string(),
        period_granularity: "calendar_quarter".to_string(),
        required_dimensions: vec![
            "product_code".to_string(),
            "period".to_string(),
            "currency".to_string(),
            "weight_unit".to_string(),
        ],
        optional_dimensions: vec!["brand".to_string(), "country".to_string()],
        min_samples,
        max_median_ratio: threshold,
        iqr_multiplier: RISK_IQR_MULTIPLIER,
        // Including the subject makes the test conservative: an unusually low
        // record can only pull its own baseline down, never inflate it.
        includes_subject_record: true,
    }
}

fn limitation(code: &str, message: impl Into<String>) -> RiskLimitation {
    RiskLimitation {
        code: code.to_string(),
        message: message.into(),
    }
}

fn unavailable_risk(
    contract: PriceRiskContract,
    limitations: Vec<RiskLimitation>,
) -> Undervaluation {
    Undervaluation {
        contract,
        limitations,
        ..Undervaluation::default()
    }
}

fn risk_confidence(sample_count: u64, median: f64, iqr: f64, cohort_level: &str) -> RiskConfidence {
    let relative_iqr = if median > 0.0 {
        iqr / median
    } else {
        f64::INFINITY
    };
    if sample_count >= 50 && relative_iqr <= 0.5 && cohort_level != "base" {
        RiskConfidence::High
    } else if sample_count >= 30 && relative_iqr <= 0.75 {
        RiskConfidence::Medium
    } else {
        RiskConfidence::Low
    }
}

fn row_limitations(
    sample_count: u64,
    median: f64,
    iqr: f64,
    cohort_level: &str,
    row_has_brand: bool,
    row_has_country: bool,
    brand_supported: bool,
    country_supported: bool,
) -> Vec<RiskLimitation> {
    let mut out = vec![limitation(
        "heuristic_not_proof",
        "This is a statistical screening signal, not proof of undervaluation.",
    )];
    if sample_count < 30 {
        out.push(limitation(
            "limited_sample",
            format!("The selected cohort contains only {sample_count} records."),
        ));
    }
    if median > 0.0 && iqr / median > 0.5 {
        out.push(limitation(
            "high_dispersion",
            "The middle half of cohort prices is widely dispersed.",
        ));
    }
    if !brand_supported {
        out.push(limitation(
            "brand_unavailable",
            "No mapped brand field is available for cohort refinement.",
        ));
    } else if row_has_brand && !matches!(cohort_level, "brand" | "brand_country") {
        out.push(limitation(
            "brand_cohort_too_small",
            "The matching brand subgroup was below the minimum sample size, so brand was not used.",
        ));
    }
    if !country_supported {
        out.push(limitation(
            "country_unavailable",
            "No mapped country field is available for cohort refinement.",
        ));
    } else if row_has_country && !matches!(cohort_level, "country" | "brand_country") {
        out.push(limitation(
            "country_cohort_too_small",
            "The matching country subgroup was below the minimum sample size, so country was not used.",
        ));
    }
    out
}

pub(crate) fn undervaluation(
    conn: &Connection,
    plan: FilterPlan,
    threshold: f64,
    min_samples: u64,
    limit: u64,
) -> rusqlite::Result<Undervaluation> {
    let cols = columns_for(conn, plan.payload_alias);
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.01, 1.0)
    } else {
        0.5
    };
    let min_samples = min_samples.clamp(RISK_MIN_SAMPLES, RISK_MAX_SAMPLES);
    let contract = risk_contract(threshold, min_samples);
    let mut required_missing = Vec::new();
    let product = cols.label(SemanticField::ProductCode).unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_product_semantic",
            "Map a product/code column before running price-risk analysis.",
        ));
        "''".to_string()
    });
    let value = cols.number(SemanticField::Value).unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_value_semantic",
            "Map the monetary value column before running price-risk analysis.",
        ));
        "NULL".to_string()
    });
    let net = cols.number(SemanticField::NetWeight).unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_net_weight_semantic",
            "Map the net-weight column before running price-risk analysis.",
        ));
        "NULL".to_string()
    });
    let month = cols.month(SemanticField::Date).unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_date_semantic",
            "Map a date column so records can be compared within a calendar quarter.",
        ));
        "''".to_string()
    });
    let currency = cols.currency_expr().unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_currency_semantic",
            "Map a currency column; price-risk analysis never combines unknown currencies.",
        ));
        "''".to_string()
    });
    let weight_unit = cols.weight_unit_expr().unwrap_or_else(|| {
        required_missing.push(limitation(
            "missing_weight_unit_semantic",
            "Map the unit used by net weight; price-risk analysis never combines unknown units.",
        ));
        "''".to_string()
    });
    if !required_missing.is_empty() {
        return Ok(unavailable_risk(contract, required_missing));
    }

    let label = |field| cols.label(field).unwrap_or_else(|| "''".to_string());
    let date = label(SemanticField::Date);
    let declaration = label(SemanticField::DeclarationNumber);
    let recipient = label(SemanticField::Recipient);
    let sender = label(SemanticField::Sender);
    let company = label(SemanticField::CompanyCode);
    let description = label(SemanticField::Description);
    let brand_supported = cols.label(SemanticField::Trademark).is_some();
    let brand = label(SemanticField::Trademark);
    let country = [
        SemanticField::OriginCountry,
        SemanticField::Country,
        SemanticField::DispatchCountry,
        SemanticField::TradeCountry,
    ]
    .into_iter()
    .find_map(|field| cols.country_key(field));
    let country_supported = country.is_some();
    let country = country.unwrap_or_else(|| "''".to_string());
    let period = format!(
        "CASE
            WHEN CAST(SUBSTR({month}, 6, 2) AS INTEGER) BETWEEN 1 AND 3
                THEN SUBSTR({month}, 1, 4) || '-Q1'
            WHEN CAST(SUBSTR({month}, 6, 2) AS INTEGER) BETWEEN 4 AND 6
                THEN SUBSTR({month}, 1, 4) || '-Q2'
            WHEN CAST(SUBSTR({month}, 6, 2) AS INTEGER) BETWEEN 7 AND 9
                THEN SUBSTR({month}, 1, 4) || '-Q3'
            WHEN CAST(SUBSTR({month}, 6, 2) AS INTEGER) BETWEEN 10 AND 12
                THEN SUBSTR({month}, 1, 4) || '-Q4'
            ELSE '' END"
    );

    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    let cte = format!(
        "WITH raw AS (
            SELECT r.id AS id,
                text_key({product}) AS code_key,
                {product} AS code,
                {value} AS source_value,
                {net} AS net_weight,
                {date} AS dt,
                {declaration} AS num,
                {recipient} AS recipient,
                {sender} AS sender,
                {company} AS edrpou,
                {description} AS descr,
                {period} AS period,
                UPPER({currency}) AS currency,
                UPPER({weight_unit}) AS weight_unit,
                text_key({brand}) AS brand_key,
                {brand} AS brand_label,
                {country} AS country_key
            FROM records r{joins}{where_sql}
         ),
         classified AS (
            SELECT *, CASE
                WHEN code_key = '' THEN 'missing_product_code'
                WHEN period = '' THEN 'missing_period'
                WHEN currency = '' THEN 'missing_currency'
                WHEN weight_unit = '' THEN 'missing_weight_unit'
                WHEN source_value IS NULL OR source_value <= 0 THEN 'invalid_value'
                WHEN net_weight IS NULL OR net_weight <= 0 THEN 'invalid_weight'
                ELSE '' END AS exclusion
            FROM raw
         ),
         priced AS (
            SELECT *, source_value / net_weight AS price
            FROM classified WHERE exclusion = ''
         ),
         base_raw AS (
            SELECT code_key, period, currency, weight_unit,
                pctl_text(price) AS pctls, COUNT(*) AS n
            FROM priced GROUP BY code_key, period, currency, weight_unit
            HAVING COUNT(*) >= {min_samples}
         ),
         base_stats AS (
            SELECT code_key, period, currency, weight_unit, n,
                pctl_num(pctls, 0) AS p25,
                pctl_num(pctls, 1) AS med,
                pctl_num(pctls, 2) AS p75
            FROM base_raw
         ),
         brand_raw AS (
            SELECT code_key, period, currency, weight_unit, brand_key,
                pctl_text(price) AS pctls, COUNT(*) AS n
            FROM priced WHERE brand_key <> ''
            GROUP BY code_key, period, currency, weight_unit, brand_key
            HAVING COUNT(*) >= {min_samples}
         ),
         brand_stats AS (
            SELECT code_key, period, currency, weight_unit, brand_key, n,
                pctl_num(pctls, 0) AS p25,
                pctl_num(pctls, 1) AS med,
                pctl_num(pctls, 2) AS p75
            FROM brand_raw
         ),
         country_raw AS (
            SELECT code_key, period, currency, weight_unit, country_key,
                pctl_text(price) AS pctls, COUNT(*) AS n
            FROM priced WHERE country_key <> ''
            GROUP BY code_key, period, currency, weight_unit, country_key
            HAVING COUNT(*) >= {min_samples}
         ),
         country_stats AS (
            SELECT code_key, period, currency, weight_unit, country_key, n,
                pctl_num(pctls, 0) AS p25,
                pctl_num(pctls, 1) AS med,
                pctl_num(pctls, 2) AS p75
            FROM country_raw
         ),
         brand_country_raw AS (
            SELECT code_key, period, currency, weight_unit, brand_key, country_key,
                pctl_text(price) AS pctls, COUNT(*) AS n
            FROM priced WHERE brand_key <> '' AND country_key <> ''
            GROUP BY code_key, period, currency, weight_unit, brand_key, country_key
            HAVING COUNT(*) >= {min_samples}
         ),
         brand_country_stats AS (
            SELECT code_key, period, currency, weight_unit, brand_key, country_key, n,
                pctl_num(pctls, 0) AS p25,
                pctl_num(pctls, 1) AS med,
                pctl_num(pctls, 2) AS p75
            FROM brand_country_raw
         ),
         evaluated AS (
            SELECT p.*,
                CASE WHEN bc.n IS NOT NULL THEN 'brand_country'
                     WHEN b.n IS NOT NULL THEN 'brand'
                     WHEN c.n IS NOT NULL THEN 'country'
                     ELSE 'base' END AS cohort_level,
                COALESCE(bc.p25, b.p25, c.p25, base.p25) AS p25,
                COALESCE(bc.med, b.med, c.med, base.med) AS med,
                COALESCE(bc.p75, b.p75, c.p75, base.p75) AS p75,
                COALESCE(bc.n, b.n, c.n, base.n) AS n
            FROM priced p
            JOIN base_stats base
              ON base.code_key = p.code_key AND base.period = p.period
             AND base.currency = p.currency AND base.weight_unit = p.weight_unit
            LEFT JOIN brand_stats b
              ON b.code_key = p.code_key AND b.period = p.period
             AND b.currency = p.currency AND b.weight_unit = p.weight_unit
             AND b.brand_key = p.brand_key
            LEFT JOIN country_stats c
              ON c.code_key = p.code_key AND c.period = p.period
             AND c.currency = p.currency AND c.weight_unit = p.weight_unit
             AND c.country_key = p.country_key
            LEFT JOIN brand_country_stats bc
              ON bc.code_key = p.code_key AND bc.period = p.period
             AND bc.currency = p.currency AND bc.weight_unit = p.weight_unit
             AND bc.brand_key = p.brand_key AND bc.country_key = p.country_key
         ),
         scored_base AS (
            SELECT *,
                p75 - p25 AS iqr,
                MAX(p25 - {RISK_IQR_MULTIPLIER} * (p75 - p25), 0.0) AS lower_fence,
                med * {threshold} AS median_ratio_cutoff
            FROM evaluated
            WHERE med > 0 AND p25 >= 0 AND p75 >= p25
         ),
         scored AS (
            SELECT *,
                MIN(lower_fence, median_ratio_cutoff) AS robust_cutoff,
                price / med AS ratio,
                MAX((med * net_weight) - source_value, 0.0) AS estimated_gap
            FROM scored_base
         ),
         flagged AS (
            SELECT * FROM scored
            WHERE price < median_ratio_cutoff AND price < lower_fence
         )
         "
    );

    let summary_sql = format!(
        "{cte}
         SELECT
            (SELECT COUNT(*) FROM classified),
            COALESCE((SELECT SUM(exclusion = 'missing_product_code') FROM classified), 0),
            COALESCE((SELECT SUM(exclusion = 'missing_period') FROM classified), 0),
            COALESCE((SELECT SUM(exclusion = 'missing_currency') FROM classified), 0),
            COALESCE((SELECT SUM(exclusion = 'missing_weight_unit') FROM classified), 0),
            COALESCE((SELECT SUM(exclusion = 'invalid_value') FROM classified), 0),
            COALESCE((SELECT SUM(exclusion = 'invalid_weight') FROM classified), 0),
            (SELECT COUNT(*) FROM priced),
            (SELECT COUNT(*) FROM evaluated),
            (SELECT COUNT(DISTINCT code_key) FROM evaluated),
            (SELECT COUNT(DISTINCT code_key || CHAR(31) || period || CHAR(31) || currency ||
                CHAR(31) || weight_unit || CHAR(31) || cohort_level || CHAR(31) ||
                CASE WHEN cohort_level IN ('brand', 'brand_country') THEN brand_key ELSE '' END ||
                CHAR(31) || CASE WHEN cohort_level IN ('country', 'brand_country')
                    THEN country_key ELSE '' END) FROM evaluated),
            (SELECT COUNT(*) FROM flagged),
            (SELECT COUNT(DISTINCT code_key) FROM flagged),
            CASE WHEN (SELECT COUNT(DISTINCT currency) FROM flagged) = 1
                THEN COALESCE((SELECT SUM(source_value) FROM flagged), 0.0) ELSE 0.0 END,
            CASE WHEN (SELECT COUNT(DISTINCT currency) FROM flagged) = 1
                THEN COALESCE((SELECT SUM(estimated_gap) FROM flagged), 0.0) ELSE 0.0 END,
            COALESCE((SELECT json_group_array(json_object(
                'currency', currency,
                'flagged_rows', flagged_rows,
                'flagged_value', flagged_value,
                'estimated_gap', estimated_gap
            )) FROM (
                SELECT currency, COUNT(*) AS flagged_rows,
                    SUM(source_value) AS flagged_value,
                    SUM(estimated_gap) AS estimated_gap
                FROM flagged GROUP BY currency ORDER BY currency
            )), '[]')"
    );
    let summary = conn.query_row(
        &summary_sql,
        params_from_iter(params.clone()),
        |row| -> rusqlite::Result<(
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            f64,
            f64,
            String,
        )> {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as u64,
                row.get::<_, i64>(3)? as u64,
                row.get::<_, i64>(4)? as u64,
                row.get::<_, i64>(5)? as u64,
                row.get::<_, i64>(6)? as u64,
                row.get::<_, i64>(7)? as u64,
                row.get::<_, i64>(8)? as u64,
                row.get::<_, i64>(9)? as u64,
                row.get::<_, i64>(10)? as u64,
                row.get::<_, i64>(11)? as u64,
                row.get::<_, i64>(12)? as u64,
                row.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
                row.get::<_, Option<f64>>(14)?.unwrap_or(0.0),
                row.get(15)?,
            ))
        },
    )?;
    let currency_totals =
        serde_json::from_str::<Vec<RiskCurrencyTotal>>(&summary.15).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                15,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;

    let sql = format!(
        "{cte}
         SELECT id, dt, num, recipient, sender, edrpou, code, descr,
                source_value, net_weight, price, period, currency, weight_unit,
                CASE WHEN cohort_level IN ('brand', 'brand_country') THEN brand_label ELSE '' END,
                CASE WHEN cohort_level IN ('country', 'brand_country') THEN country_key ELSE '' END,
                p25, med, p75, n, iqr, lower_fence, median_ratio_cutoff, robust_cutoff,
                ratio, estimated_gap, cohort_level, brand_key, country_key
         FROM flagged
         ORDER BY ratio ASC, estimated_gap DESC
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut bind = params;
    bind.push((limit as i64).into());
    let mut rows = stmt.query(params_from_iter(bind))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let period: String = row.get(11)?;
        let currency: String = row.get(12)?;
        let weight_unit: String = row.get(13)?;
        let cohort_brand: String = row.get(14)?;
        let cohort_country: String = row.get(15)?;
        let p25: f64 = row.get(16)?;
        let median: f64 = row.get(17)?;
        let p75: f64 = row.get(18)?;
        let sample_count = row.get::<_, i64>(19)? as u64;
        let iqr: f64 = row.get(20)?;
        let lower_fence: f64 = row.get(21)?;
        let median_ratio_cutoff: f64 = row.get(22)?;
        let robust_cutoff: f64 = row.get(23)?;
        let ratio: f64 = row.get(24)?;
        let estimated_gap = row.get::<_, Option<f64>>(25)?.unwrap_or(0.0);
        let cohort_level: String = row.get(26)?;
        let row_brand_key: String = row.get(27)?;
        let row_country_key: String = row.get(28)?;
        let code = row.get::<_, Option<String>>(6)?.unwrap_or_default();
        let price: f64 = row.get(10)?;
        let deviation_percent = ((1.0 - ratio) * 100.0).max(0.0);
        let confidence = risk_confidence(sample_count, median, iqr, &cohort_level);
        let limitations = row_limitations(
            sample_count,
            median,
            iqr,
            &cohort_level,
            !row_brand_key.is_empty(),
            !row_country_key.is_empty(),
            brand_supported,
            country_supported,
        );
        let mut dimensions = contract.required_dimensions.clone();
        if matches!(cohort_level.as_str(), "brand" | "brand_country") {
            dimensions.push("brand".to_string());
        }
        if matches!(cohort_level.as_str(), "country" | "brand_country") {
            dimensions.push("country".to_string());
        }
        let reason = format!(
            "Price {price:.4} {currency}/{weight_unit} is {deviation_percent:.1}% below the cohort median {median:.4} and below both the {:.0}% median cap ({median_ratio_cutoff:.4}) and the {RISK_IQR_MULTIPLIER:.1}×IQR lower fence ({lower_fence:.4}).",
            threshold * 100.0,
        );
        out.push(UndervaluedRow {
            id: row.get(0)?,
            declaration_date: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            declaration_number: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            recipient: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            sender: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            edrpou: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            product_code: code.clone(),
            description: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            source_value: row.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
            net_kg: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
            price_per_kg: price,
            code_median: median,
            code_p25: p25,
            code_p75: p75,
            code_sample_count: sample_count,
            ratio,
            estimated_gap,
            cohort: RiskCohort {
                product_code: code,
                period,
                currency,
                weight_unit,
                brand: (!cohort_brand.is_empty()).then_some(cohort_brand),
                country: (!cohort_country.is_empty()).then_some(cohort_country),
                dimensions,
                sample_count,
                median,
                p25,
                p75,
                iqr,
                lower_fence,
                median_ratio_cutoff,
                robust_cutoff,
            },
            deviation_percent,
            confidence,
            reason,
            limitations,
        });
    }
    let mut global_limitations = Vec::new();
    if !brand_supported {
        global_limitations.push(limitation(
            "brand_unavailable",
            "Brand is not mapped; cohorts cannot be refined by brand.",
        ));
    }
    if !country_supported {
        global_limitations.push(limitation(
            "country_unavailable",
            "Country is not mapped; cohorts cannot be refined by country.",
        ));
    }
    if currency_totals.len() > 1 {
        global_limitations.push(limitation(
            "multiple_currencies_not_summed",
            "Flagged values and estimated gaps are reported separately by currency and are not added together.",
        ));
    }
    Ok(Undervaluation {
        available: true,
        rows: out,
        checked_rows: summary.8,
        checked_codes: summary.9,
        flagged_rows: summary.11,
        flagged_codes: summary.12,
        flagged_value: summary.13,
        estimated_gap: summary.14,
        eligible_rows: summary.7,
        evaluated_rows: summary.8,
        checked_cohorts: summary.10,
        contract,
        exclusions: RiskExclusions {
            query_rows: summary.0,
            missing_product_code: summary.1,
            missing_period: summary.2,
            missing_currency: summary.3,
            missing_weight_unit: summary.4,
            invalid_value: summary.5,
            invalid_weight: summary.6,
            insufficient_cohort: summary.7.saturating_sub(summary.8),
        },
        limitations: global_limitations,
        currency_totals,
    })
}

struct SectionGrouping {
    label_sql: String,
    filter_field: Option<AnalyticsFilterField>,
}

/// Final grouping/label SQL for a section. The expression is resolved from the
/// recorded source shape first, so generic files can drive analytics once the
/// user assigns semantic meanings to their columns.
fn section_grouping(
    cols: &AnalyticsColumns,
    kind: AnalyticsSectionKind,
    hs_level: u8,
) -> Option<SectionGrouping> {
    let grouping = |semantic, filter_field| {
        Some(SectionGrouping {
            label_sql: cols.label(semantic)?,
            filter_field: cols.is_schema_backed(semantic).then_some(filter_field),
        })
    };
    let country_grouping = |semantic, filter_field| {
        Some(SectionGrouping {
            label_sql: cols.country_key(semantic)?,
            filter_field: cols.is_schema_backed(semantic).then_some(filter_field),
        })
    };
    match kind {
        AnalyticsSectionKind::Recipients => {
            grouping(SemanticField::Recipient, AnalyticsFilterField::Recipient)
        }
        AnalyticsSectionKind::Senders => {
            grouping(SemanticField::Sender, AnalyticsFilterField::Sender)
        }
        AnalyticsSectionKind::Edrpou => {
            grouping(SemanticField::CompanyCode, AnalyticsFilterField::Edrpou)
        }
        AnalyticsSectionKind::ProductCodes => {
            let product = cols.label(SemanticField::ProductCode)?;
            let expr = if hs_level >= 10 {
                product
            } else {
                format!(
                    "label_value(SUBSTR({product}, 1, {}))",
                    hs_level.clamp(2, 8)
                )
            };
            Some(SectionGrouping {
                label_sql: expr,
                filter_field: cols
                    .is_schema_backed(SemanticField::ProductCode)
                    .then_some(AnalyticsFilterField::ProductCode),
            })
        }
        AnalyticsSectionKind::Trademarks => {
            grouping(SemanticField::Trademark, AnalyticsFilterField::Trademark)
        }
        AnalyticsSectionKind::ProductGroups => {
            let description = cols.label(SemanticField::Description)?;
            Some(SectionGrouping {
                label_sql: format!("label_value(SUBSTR({description}, 1, 80))"),
                filter_field: cols
                    .is_schema_backed(SemanticField::Description)
                    .then_some(AnalyticsFilterField::Description),
            })
        }
        AnalyticsSectionKind::OriginCountries => country_grouping(
            SemanticField::OriginCountry,
            AnalyticsFilterField::OriginCountry,
        ),
        AnalyticsSectionKind::DispatchCountries => country_grouping(
            SemanticField::DispatchCountry,
            AnalyticsFilterField::DispatchCountry,
        ),
        AnalyticsSectionKind::TradeCountries => country_grouping(
            SemanticField::TradeCountry,
            AnalyticsFilterField::TradeCountry,
        ),
    }
}

fn price_metric(
    conn: &Connection,
    plan: FilterPlan,
    kind: PriceMetricKind,
    price_expr: &str,
    weight_expr: &str,
) -> rusqlite::Result<AnalyticsPriceMetric> {
    let joins = &plan.joins;
    let where_sql = &plan.where_sql;
    let params = plan.params;
    let sql = format!(
        "SELECT
            COUNT(price),
            AVG(price),
            MIN(price),
            MAX(price),
            SUM(CASE WHEN price IS NOT NULL AND weight IS NOT NULL AND weight > 0
                THEN price * weight ELSE 0 END),
            SUM(CASE WHEN price IS NOT NULL AND weight IS NOT NULL AND weight > 0
                THEN weight ELSE 0 END),
            pctl_text(price)
         FROM (
            SELECT {price_expr} AS price, {weight_expr} AS weight
            FROM records r{joins}{where_sql}
         )"
    );
    conn.query_row(&sql, params_from_iter(params), |row| {
        let weighted_sum = row.get::<_, Option<f64>>(4)?.unwrap_or(0.0);
        let weighted_kg = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
        let pctls: Option<String> = row.get(6)?;
        let mut parts = pctls
            .as_deref()
            .unwrap_or("")
            .split('|')
            .map(|p| p.parse::<f64>().unwrap_or(0.0));
        let p25 = parts.next().unwrap_or(0.0);
        let median = parts.next().unwrap_or(0.0);
        let p75 = parts.next().unwrap_or(0.0);
        Ok(AnalyticsPriceMetric {
            kind,
            count: row.get::<_, i64>(0)? as u64,
            average: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            minimum: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            maximum: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            weighted_average: ratio(weighted_sum, weighted_kg),
            median,
            p25,
            p75,
            // Per-currency/unit cohort statistics are not computed for the
            // legacy metric list yet; empty means "no cohort detail".
            cohorts: Vec::new(),
            excluded_rows: 0,
        })
    })
}

struct PivotDimSql {
    expr: String,
    filterable: bool,
}

fn pivot_dim_sql(cols: &AnalyticsColumns, dim: PivotDim) -> Option<PivotDimSql> {
    let semantic = match dim {
        PivotDim::Recipient => Some((SemanticField::Recipient, false)),
        PivotDim::Sender => Some((SemanticField::Sender, false)),
        PivotDim::Edrpou => Some((SemanticField::CompanyCode, false)),
        PivotDim::ProductCode => Some((SemanticField::ProductCode, false)),
        PivotDim::Trademark => Some((SemanticField::Trademark, false)),
        PivotDim::OriginCountry => Some((SemanticField::OriginCountry, true)),
        PivotDim::DispatchCountry => Some((SemanticField::DispatchCountry, true)),
        PivotDim::TradeCountry => Some((SemanticField::TradeCountry, true)),
        PivotDim::Month | PivotDim::Year => None,
    };
    if let Some((field, is_country)) = semantic {
        let expr = if is_country {
            cols.country_key(field)?
        } else {
            cols.label(field)?
        };
        return Some(PivotDimSql {
            expr,
            filterable: cols.is_schema_backed(field) && dim.filter_field().is_some(),
        });
    }

    match dim {
        PivotDim::Month => Some(PivotDimSql {
            expr: cols.month(SemanticField::Date)?,
            filterable: false,
        }),
        PivotDim::Year => {
            let expr = if cols.is_schema_backed(SemanticField::Date) {
                format!("CAST({} AS TEXT)", cols.raw_column("year"))
            } else {
                format!("SUBSTR({}, 1, 4)", cols.month(SemanticField::Date)?)
            };
            Some(PivotDimSql {
                expr,
                filterable: false,
            })
        }
        _ => None,
    }
}

fn pivot_metric_sql(cols: &AnalyticsColumns, metric: PivotMetric) -> Option<String> {
    match metric {
        PivotMetric::Rows => Some("CAST(COUNT(*) AS REAL)".to_string()),
        PivotMetric::Value => cols
            .number(SemanticField::Value)
            .map(|expr| format!("COALESCE(SUM({expr}), 0.0)")),
        PivotMetric::NetKg => cols
            .number(SemanticField::NetWeight)
            .map(|expr| format!("COALESCE(SUM({expr}), 0.0)")),
    }
}

fn rank_pivot_labels(
    totals: &HashMap<String, f64>,
    limit: usize,
    sort_label: bool,
) -> (Vec<String>, bool) {
    let mut items: Vec<(String, f64)> = totals.iter().map(|(k, v)| (k.clone(), *v)).collect();
    if sort_label {
        items.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        items.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
    let truncated = items.len() > limit;
    items.truncate(limit);
    (
        items.into_iter().map(|(k, _)| k).collect::<Vec<_>>(),
        truncated,
    )
}

fn empty_pivot() -> PivotResult {
    PivotResult {
        row_filterable: false,
        col_filterable: false,
        ..Default::default()
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}
