//! Resolves analytics SQL expressions for semantic fields against the recorded
//! table shape, so analytics is driven by the shape rather than hardcoded column
//! names.
//!
//! For a recognized customs column the semantic resolves to its materialized
//! typed column (`storage::derived`), so customs analytics stays fully typed and
//! unchanged. For a column the user assigned a meaning to that lives in the
//! `extra` JSON (a generic table), the semantic resolves to a normalized
//! expression over that JSON value. When the shape carries no column for a
//! semantic, it falls back to the customs profile.

use crate::domain::table::{ColumnStorage, SemanticField, TableShape};
use crate::schema::column_for_semantic;
use crate::storage::derived;

pub(crate) const UNKNOWN_CURRENCY_KEY: &str = "__unknown__";
pub(crate) const UNKNOWN_UNIT_KEY: &str = "__unknown__";

#[derive(Clone, Debug)]
pub(crate) struct AnalyticsMeasureSql {
    pub value: String,
    pub currency_key: String,
    pub net_weight: String,
    pub gross_weight: String,
    pub weight_unit_key: String,
    /// Standalone factor expression; consumers currently read the pre-combined
    /// `*_weight_kg` expressions instead, but the factor stays available for
    /// future per-unit aggregation.
    #[allow(dead_code)]
    pub weight_factor_to_kg: String,
    pub net_weight_kg: String,
    pub gross_weight_kg: String,
}

pub(crate) struct AnalyticsColumns {
    shape: Option<TableShape>,
    row_alias: String,
    /// Schema-level fixed values chosen at import ("this whole file is USD").
    /// Present only when every registered schema agrees on the same value.
    fixed_currency: Option<String>,
    fixed_weight_unit: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
enum Source {
    /// A physical column in the `records` table.
    Schema(String),
    /// A value inside the `extra` JSON, keyed by header.
    Extra(String),
}

impl AnalyticsColumns {
    pub(crate) fn for_alias(shape: Option<TableShape>, row_alias: &str) -> Self {
        Self {
            shape,
            row_alias: row_alias.to_string(),
            fixed_currency: None,
            fixed_weight_unit: None,
        }
    }

    pub(crate) fn with_fixed_values(
        mut self,
        fixed_currency: Option<String>,
        fixed_weight_unit: Option<String>,
    ) -> Self {
        self.fixed_currency = fixed_currency;
        self.fixed_weight_unit = fixed_weight_unit;
        self
    }

    /// Currency expression when the data carries one: a mapped currency
    /// column, the schema-level fixed value chosen at import, or the currency
    /// embedded in the value column's own name ("Value USD").
    pub(crate) fn currency_expr(&self) -> Option<String> {
        self.text(SemanticField::Currency)
            .or_else(|| self.fixed_currency.clone().map(sql_literal))
            .or_else(|| self.embedded_currency().map(sql_literal))
    }

    /// Weight-unit expression: a mapped unit column, the schema-level fixed
    /// value chosen at import, or the unit embedded in the weight column's
    /// own name ("Net kg").
    pub(crate) fn weight_unit_expr(&self) -> Option<String> {
        self.text(SemanticField::WeightUnit)
            .or_else(|| self.fixed_weight_unit.clone().map(sql_literal))
            .or_else(|| self.embedded_weight_unit().map(sql_literal))
    }

    pub(crate) fn raw_column(&self, name: &str) -> String {
        format!("{}.{name}", self.row_alias)
    }

    fn sources_for(&self, field: SemanticField) -> Vec<Source> {
        let mut sources = Vec::new();
        if let Some(shape) = &self.shape {
            for column in shape.columns.iter().filter(|c| c.semantic == Some(field)) {
                let source = match &column.storage {
                    ColumnStorage::SchemaColumn(name) => Source::Schema(name.clone()),
                    ColumnStorage::SourceJson => Source::Extra(column.header.clone()),
                };
                if !sources.contains(&source) {
                    sources.push(source);
                }
            }
        }
        if sources.is_empty()
            && let Some(name) = column_for_semantic(field)
        {
            sources.push(Source::Schema(name.to_string()));
        }
        sources
    }

    pub(crate) fn is_schema_backed(&self, field: SemanticField) -> bool {
        let sources = self.sources_for(field);
        !sources.is_empty()
            && sources
                .iter()
                .all(|source| matches!(source, Source::Schema(_)))
    }

    /// Raw text expression for a semantic field. This is used by filters that
    /// need prefix/equality behavior over the user's mapped source column.
    pub(crate) fn text(&self, field: SemanticField) -> Option<String> {
        let expressions = self
            .sources_for(field)
            .into_iter()
            .map(|source| match source {
                Source::Schema(name) => format!("{}.{name}", self.row_alias),
                Source::Extra(header) => format!(
                    "extra_value({}.extra, '{}')",
                    self.row_alias,
                    escape(&header)
                ),
            })
            .collect::<Vec<_>>();
        coalesce_text(expressions)
    }

    /// Numeric (`REAL`/`NULL`) expression for a semantic, or `None` if the shape
    /// has no column for it.
    pub(crate) fn number(&self, field: SemanticField) -> Option<String> {
        // Totals and quantities read a lone three-digit tail ("1.250") as a
        // thousands group; weights and per-unit prices keep it as decimals.
        let parse_fn = match field {
            SemanticField::Value | SemanticField::Quantity => "num_value_grouped",
            _ => "num_value",
        };
        let expressions = self
            .sources_for(field)
            .into_iter()
            .map(|source| match source {
                Source::Schema(name) => derived::num_column_for(&name)
                    .map(|column| format!("{}.{column}", self.row_alias))
                    .unwrap_or_else(|| format!("{parse_fn}({}.{name})", self.row_alias)),
                Source::Extra(header) => {
                    format!(
                        "{parse_fn}(extra_value({}.extra, '{}'))",
                        self.row_alias,
                        escape(&header)
                    )
                }
            })
            .collect::<Vec<_>>();
        coalesce_number(expressions)
    }

    /// Cleaned grouping-label expression for a semantic.
    pub(crate) fn label(&self, field: SemanticField) -> Option<String> {
        let expressions = self
            .sources_for(field)
            .into_iter()
            .map(|source| match source {
                Source::Schema(name) => derived::label_column_for(&name)
                    .map(|column| format!("{}.{column}", self.row_alias))
                    .unwrap_or_else(|| format!("label_value({}.{name})", self.row_alias)),
                Source::Extra(header) => {
                    format!(
                        "label_value(extra_value({}.extra, '{}'))",
                        self.row_alias,
                        escape(&header)
                    )
                }
            })
            .collect::<Vec<_>>();
        coalesce_text(expressions)
    }

    /// Normalized country-key expression for a semantic.
    pub(crate) fn country_key(&self, field: SemanticField) -> Option<String> {
        let expressions = self
            .sources_for(field)
            .into_iter()
            .map(|source| match source {
                Source::Schema(name) => derived::key_column_for(&name)
                    .map(|column| format!("{}.{column}", self.row_alias))
                    .unwrap_or_else(|| format!("country_key({}.{name})", self.row_alias)),
                Source::Extra(header) => {
                    format!(
                        "country_key(extra_value({}.extra, '{}'))",
                        self.row_alias,
                        escape(&header)
                    )
                }
            })
            .collect::<Vec<_>>();
        coalesce_text(expressions)
    }

    /// Month-key expression (`YYYY-MM`) for a date semantic.
    pub(crate) fn month(&self, field: SemanticField) -> Option<String> {
        let expressions = self
            .sources_for(field)
            .into_iter()
            .map(|source| match source {
                Source::Schema(name) => derived::month_column_for(&name)
                    .map(|column| format!("{}.{column}", self.row_alias))
                    .unwrap_or_else(|| format!("month_key({}.{name})", self.row_alias)),
                Source::Extra(header) => {
                    format!(
                        "month_key(extra_value({}.extra, '{}'))",
                        self.row_alias,
                        escape(&header)
                    )
                }
            })
            .collect::<Vec<_>>();
        coalesce_text(expressions)
    }

    pub(crate) fn measures(&self) -> AnalyticsMeasureSql {
        let value = self
            .number(SemanticField::Value)
            .unwrap_or_else(|| "NULL".to_string());
        let net_weight = self
            .number(SemanticField::NetWeight)
            .unwrap_or_else(|| "NULL".to_string());
        let gross_weight = self
            .number(SemanticField::GrossWeight)
            .unwrap_or_else(|| "NULL".to_string());
        let currency_raw = self.currency_expr().unwrap_or_else(|| "''".to_string());
        let weight_unit_raw = self.weight_unit_expr().unwrap_or_else(|| "''".to_string());
        let currency_key = currency_key_sql(&currency_raw);
        let weight_unit_key = weight_unit_key_sql(&weight_unit_raw);
        let weight_factor_to_kg = format!(
            "CASE {weight_unit_key}
                WHEN 'kg' THEN 1.0
                WHEN 'g' THEN 0.001
                WHEN 'tonne' THEN 1000.0
                WHEN 'lb' THEN 0.45359237
             END"
        );
        let net_weight_kg = format!(
            "CASE WHEN {net_weight} IS NOT NULL AND ({weight_factor_to_kg}) IS NOT NULL
                THEN {net_weight} * ({weight_factor_to_kg}) END"
        );
        let gross_weight_kg = format!(
            "CASE WHEN {gross_weight} IS NOT NULL AND ({weight_factor_to_kg}) IS NOT NULL
                THEN {gross_weight} * ({weight_factor_to_kg}) END"
        );
        AnalyticsMeasureSql {
            value,
            currency_key,
            net_weight,
            gross_weight,
            weight_unit_key,
            weight_factor_to_kg,
            net_weight_kg,
            gross_weight_kg,
        }
    }

    /// User-visible headers of the columns carrying a semantic, falling back
    /// to the canonical customs header. Hints must read what the user saw
    /// ("Value USD"), not the physical storage name.
    fn semantic_headers(&self, field: SemanticField) -> Vec<String> {
        if let Some(shape) = &self.shape {
            let headers: Vec<String> = shape
                .columns
                .iter()
                .filter(|column| column.semantic == Some(field))
                .map(|column| column.header.clone())
                .collect();
            if !headers.is_empty() {
                return headers;
            }
        }
        column_for_semantic(field)
            .map(|name| vec![crate::schema::header_for(name).to_string()])
            .unwrap_or_default()
    }

    fn embedded_currency(&self) -> Option<String> {
        if !self.sources_for(SemanticField::Currency).is_empty() {
            return None;
        }
        common_hint(
            self.semantic_headers(SemanticField::Value)
                .iter()
                .map(|header| currency_hint(header)),
        )
    }

    fn embedded_weight_unit(&self) -> Option<String> {
        if !self.sources_for(SemanticField::WeightUnit).is_empty() {
            return None;
        }
        let headers = [SemanticField::NetWeight, SemanticField::GrossWeight]
            .into_iter()
            .flat_map(|field| self.semantic_headers(field))
            .collect::<Vec<_>>();
        common_hint(headers.iter().map(|header| weight_hint(header)))
    }
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Source::Schema(name) | Source::Extra(name) => name,
        }
    }
}

fn common_hint(hints: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut hints = hints.into_iter();
    let first = hints.next()??;
    if hints.all(|hint| hint.as_deref() == Some(first.as_str())) {
        Some(first)
    } else {
        None
    }
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn currency_hint(value: &str) -> Option<String> {
    const CODES: [&str; 12] = [
        "usd", "eur", "uah", "gbp", "cny", "jpy", "chf", "pln", "cad", "aud", "sek", "nok",
    ];
    normalized_tokens(value)
        .into_iter()
        .find(|token| CODES.contains(&token.as_str()))
        .map(|token| token.to_ascii_uppercase())
}

fn weight_hint(value: &str) -> Option<String> {
    let tokens = normalized_tokens(value);
    if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "kg" | "kgs" | "кг"))
    {
        Some("kg".to_string())
    } else if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "gram" | "grams" | "gm" | "гр"))
    {
        Some("g".to_string())
    } else if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "ton" | "tons" | "tonne" | "tonnes" | "metricton" | "mt"
        )
    }) {
        Some("tonne".to_string())
    } else if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "lb" | "lbs" | "pound" | "pounds"))
    {
        Some("lb".to_string())
    } else {
        None
    }
}

fn currency_key_sql(raw: &str) -> String {
    let key = format!("text_key({raw})");
    format!(
        "CASE {key}
            WHEN 'usd' THEN 'USD' WHEN 'us dollar' THEN 'USD' WHEN 'us dollars' THEN 'USD'
            WHEN 'us$' THEN 'USD'
            WHEN 'eur' THEN 'EUR' WHEN 'euro' THEN 'EUR' WHEN 'euros' THEN 'EUR'
            WHEN '€' THEN 'EUR'
            WHEN 'uah' THEN 'UAH' WHEN 'hryvnia' THEN 'UAH' WHEN '₴' THEN 'UAH'
            WHEN 'gbp' THEN 'GBP' WHEN 'pound sterling' THEN 'GBP' WHEN '£' THEN 'GBP'
            WHEN 'cny' THEN 'CNY' WHEN 'rmb' THEN 'CNY' WHEN 'yuan' THEN 'CNY'
            ELSE CASE
                WHEN {key} = '' THEN '{UNKNOWN_CURRENCY_KEY}'
                WHEN LENGTH({key}) = 3 AND {key} GLOB '[a-z][a-z][a-z]' THEN UPPER({key})
                ELSE '{UNKNOWN_CURRENCY_KEY}:' || UPPER({key})
            END
         END"
    )
}

fn weight_unit_key_sql(raw: &str) -> String {
    let key = format!("text_key({raw})");
    format!(
        "CASE {key}
            WHEN 'kg' THEN 'kg' WHEN 'kgs' THEN 'kg'
            WHEN 'kilogram' THEN 'kg' WHEN 'kilograms' THEN 'kg'
            WHEN 'кг' THEN 'kg'
            WHEN 'g' THEN 'g' WHEN 'gr' THEN 'g' WHEN 'gram' THEN 'g' WHEN 'grams' THEN 'g'
            WHEN 'г' THEN 'g' WHEN 'гр' THEN 'g'
            WHEN 't' THEN 'tonne' WHEN 'ton' THEN 'tonne' WHEN 'tons' THEN 'tonne'
            WHEN 'tonne' THEN 'tonne' WHEN 'tonnes' THEN 'tonne'
            WHEN 'metric ton' THEN 'tonne' WHEN 'mt' THEN 'tonne' WHEN 'т' THEN 'tonne'
            WHEN 'lb' THEN 'lb' WHEN 'lbs' THEN 'lb' WHEN 'pound' THEN 'lb'
            WHEN 'pounds' THEN 'lb'
            ELSE CASE WHEN {key} = '' THEN '{UNKNOWN_UNIT_KEY}'
                ELSE '{UNKNOWN_UNIT_KEY}:' || {key} END
         END"
    )
}

fn sql_literal(value: String) -> String {
    format!("'{}'", escape(&value))
}

fn coalesce_number(expressions: Vec<String>) -> Option<String> {
    match expressions.as_slice() {
        [] => None,
        [expr] => Some(expr.clone()),
        _ => Some(format!("COALESCE({})", expressions.join(", "))),
    }
}

fn coalesce_text(expressions: Vec<String>) -> Option<String> {
    match expressions.as_slice() {
        [] => None,
        [expr] => Some(expr.clone()),
        _ => Some(format!(
            "COALESCE({}, '')",
            expressions
                .into_iter()
                .map(|expr| format!("NULLIF({expr}, '')"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Escapes a header for inlining as a single-quoted SQL string literal. Headers
/// come from imported files (not from query-time user input) and are escaped
/// here so a quote in a header cannot break the expression.
fn escape(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::AnalyticsColumns;
    use crate::domain::table::{
        ColumnRole, ColumnStorage, SemanticField, SourceColumn, TableShape,
    };

    fn column(
        id: &str,
        header: &str,
        semantic: SemanticField,
        storage: ColumnStorage,
    ) -> SourceColumn {
        SourceColumn {
            id: id.to_string(),
            header: header.to_string(),
            source_index: 0,
            role: ColumnRole::Text,
            semantic: Some(semantic),
            storage,
        }
    }

    #[test]
    fn customs_semantics_resolve_to_typed_columns() {
        let shape = TableShape {
            columns: vec![
                column(
                    "value",
                    "ФВ вал.контр",
                    SemanticField::Value,
                    ColumnStorage::SchemaColumn("currency_control_value".to_string()),
                ),
                column(
                    "sender",
                    "Відправник",
                    SemanticField::Sender,
                    ColumnStorage::SchemaColumn("sender".to_string()),
                ),
                column(
                    "origin",
                    "Кр.пох.",
                    SemanticField::OriginCountry,
                    ColumnStorage::SchemaColumn("origin_country".to_string()),
                ),
            ],
        };
        let cols = AnalyticsColumns::for_alias(Some(shape), "r");
        assert_eq!(
            cols.number(SemanticField::Value).as_deref(),
            Some("r.value_num")
        );
        assert_eq!(
            cols.label(SemanticField::Sender).as_deref(),
            Some("r.sender_label")
        );
        assert_eq!(
            cols.country_key(SemanticField::OriginCountry).as_deref(),
            Some("r.origin_key")
        );
        assert_eq!(cols.month(SemanticField::Date).as_deref(), Some("r.month"));
    }

    #[test]
    fn missing_shape_falls_back_to_customs_profile() {
        let cols = AnalyticsColumns::for_alias(None, "r");
        assert_eq!(
            cols.number(SemanticField::Value).as_deref(),
            Some("r.value_num")
        );
        assert_eq!(
            cols.label(SemanticField::Recipient).as_deref(),
            Some("r.recipient_label")
        );
        assert_eq!(cols.month(SemanticField::Date).as_deref(), Some("r.month"));
    }

    #[test]
    fn user_assigned_extra_column_resolves_to_json_expression() {
        let shape = TableShape {
            columns: vec![
                column(
                    "price_eur",
                    "Price EUR",
                    SemanticField::Value,
                    ColumnStorage::SourceJson,
                ),
                column(
                    "ship_from",
                    "Ship From",
                    SemanticField::OriginCountry,
                    ColumnStorage::SourceJson,
                ),
                column(
                    "order_date",
                    "Order Date",
                    SemanticField::Date,
                    ColumnStorage::SourceJson,
                ),
            ],
        };
        let cols = AnalyticsColumns::for_alias(Some(shape), "r");
        assert_eq!(
            cols.number(SemanticField::Value).as_deref(),
            Some("num_value_grouped(extra_value(r.extra, 'Price EUR'))")
        );
        assert_eq!(
            cols.country_key(SemanticField::OriginCountry).as_deref(),
            Some("country_key(extra_value(r.extra, 'Ship From'))")
        );
        assert_eq!(
            cols.month(SemanticField::Date).as_deref(),
            Some("month_key(extra_value(r.extra, 'Order Date'))")
        );
    }

    #[test]
    fn header_quote_is_escaped() {
        let shape = TableShape {
            columns: vec![column(
                "x",
                "O'Hara value",
                SemanticField::Value,
                ColumnStorage::SourceJson,
            )],
        };
        let cols = AnalyticsColumns::for_alias(Some(shape), "r");
        assert_eq!(
            cols.number(SemanticField::Value).as_deref(),
            Some("num_value_grouped(extra_value(r.extra, 'O''Hara value'))")
        );
    }

    #[test]
    fn mixed_schema_and_extra_semantics_coalesce_values() {
        let shape = TableShape {
            columns: vec![
                column(
                    "value",
                    "Profile value",
                    SemanticField::Value,
                    ColumnStorage::SchemaColumn("currency_control_value".to_string()),
                ),
                column(
                    "generic_value",
                    "Generic value",
                    SemanticField::Value,
                    ColumnStorage::SourceJson,
                ),
                column(
                    "recipient",
                    "Profile recipient",
                    SemanticField::Recipient,
                    ColumnStorage::SchemaColumn("recipient".to_string()),
                ),
                column(
                    "generic_recipient",
                    "Generic recipient",
                    SemanticField::Recipient,
                    ColumnStorage::SourceJson,
                ),
            ],
        };
        let cols = AnalyticsColumns::for_alias(Some(shape), "r");

        assert_eq!(
            cols.number(SemanticField::Value).as_deref(),
            Some("COALESCE(r.value_num, num_value_grouped(extra_value(r.extra, 'Generic value')))")
        );
        assert_eq!(
            cols.label(SemanticField::Recipient).as_deref(),
            Some(
                "COALESCE(NULLIF(r.recipient_label, ''), NULLIF(label_value(extra_value(r.extra, 'Generic recipient')), ''), '')"
            )
        );
        assert!(!cols.is_schema_backed(SemanticField::Recipient));
    }
}
