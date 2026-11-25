//! Single source of truth for the typed and normalized columns that are
//! materialized once at import time.
//!
//! Analytics aggregates over millions of rows. Parsing localized numbers and
//! normalizing labels/countries with per-row SQL functions turns every
//! aggregation into a full scan of user functions. Instead, each value is
//! normalized once on insert and stored in a dedicated column, so analytics can
//! `SUM`/`GROUP BY`/`COUNT(DISTINCT)` plain typed columns.
//!
//! The DDL, the import insert, the migration backfill, and the analytics queries
//! all derive from [`DERIVED`], so adding or changing a materialized column is a
//! one-line change here.

use rusqlite::types::Value;

use crate::storage::normalize::{
    NumberStyle, clean_label_value, month_key, normalize_country_key, parse_number_styled,
};

/// How a derived column is computed from its source schema column.
#[derive(Clone, Copy)]
pub(crate) enum Derivation {
    /// Localized number to `REAL`, or `NULL` when the text is not numeric.
    /// The style resolves the "1.250" thousands-vs-decimals ambiguity per
    /// column: weights keep three-decimal readings, totals and quantities
    /// read a lone three-digit tail as a thousands group.
    Number(NumberStyle),
    /// Cleaned grouping label; placeholder values ("0", "n/a", ...) become "".
    Label,
    /// Normalized ISO country code with synonyms merged; placeholders become "".
    Country,
    /// "YYYY-MM" from an ISO date, or "" when the value is not a date.
    Month,
}

pub(crate) struct DerivedColumn {
    /// Stored column name in `records`.
    pub(crate) name: &'static str,
    /// SQLite column type.
    pub(crate) sql_type: &'static str,
    /// Source schema column (a name in [`crate::schema::COLUMNS`]).
    pub(crate) source: &'static str,
    pub(crate) derivation: Derivation,
}

const fn num(name: &'static str, source: &'static str) -> DerivedColumn {
    DerivedColumn {
        name,
        sql_type: "REAL",
        source,
        derivation: Derivation::Number(NumberStyle::PreferDecimal),
    }
}

const fn num_grouped(name: &'static str, source: &'static str) -> DerivedColumn {
    DerivedColumn {
        name,
        sql_type: "REAL",
        source,
        derivation: Derivation::Number(NumberStyle::PreferGrouped),
    }
}

const fn label(name: &'static str, source: &'static str) -> DerivedColumn {
    DerivedColumn {
        name,
        sql_type: "TEXT",
        source,
        derivation: Derivation::Label,
    }
}

const fn country(name: &'static str, source: &'static str) -> DerivedColumn {
    DerivedColumn {
        name,
        sql_type: "TEXT",
        source,
        derivation: Derivation::Country,
    }
}

/// Columns materialized at import time. Order is stable; insert binds them in
/// this order after the raw schema columns.
pub(crate) const DERIVED: &[DerivedColumn] = &[
    num_grouped("value_num", "currency_control_value"),
    num("net_kg_num", "net_kg"),
    num("gross_kg_num", "gross_kg"),
    num_grouped("quantity_num", "quantity"),
    num("rfv_num", "rfv_usd_kg"),
    num("rmv_net_num", "rmv_net_usd_kg"),
    num("rmv_extra_num", "rmv_usd_extra_unit"),
    num("rmv_gross_num", "rmv_gross_usd_kg"),
    num("min_base_num", "min_base_usd_kg"),
    label("sender_label", "sender"),
    label("recipient_label", "recipient"),
    label("edrpou_label", "edrpou"),
    label("trademark_label", "trademark"),
    country("origin_key", "origin_country"),
    country("dispatch_key", "dispatch_country"),
    country("trade_key", "trade_country"),
    DerivedColumn {
        name: "month",
        sql_type: "TEXT",
        source: "declaration_date",
        derivation: Derivation::Month,
    },
];

/// Computes the stored value of a derived column from its source text.
pub(crate) fn compute(derivation: Derivation, source_value: &str) -> Value {
    match derivation {
        Derivation::Number(style) => match parse_number_styled(source_value, style) {
            Some(number) if number.is_finite() => Value::Real(number),
            _ => Value::Null,
        },
        Derivation::Label => Value::Text(clean_label_value(source_value)),
        Derivation::Country => Value::Text(normalize_country_key(source_value)),
        Derivation::Month => Value::Text(month_of(source_value)),
    }
}

/// Materialized numeric column for a source schema column, if one exists.
pub(crate) fn num_column_for(source: &str) -> Option<&'static str> {
    derived_name_for(source, Derivation::Number(NumberStyle::PreferDecimal))
}

/// The number style a source schema column is materialized with, so that
/// query-time parsing (search filters) matches the stored value exactly.
pub(crate) fn number_style_for(source: &str) -> Option<NumberStyle> {
    DERIVED.iter().find_map(|column| match column.derivation {
        Derivation::Number(style) if column.source == source => Some(style),
        _ => None,
    })
}

/// Materialized cleaned-label column for a source schema column, if one exists.
pub(crate) fn label_column_for(source: &str) -> Option<&'static str> {
    derived_name_for(source, Derivation::Label)
}

/// Materialized country-key column for a source schema column, if one exists.
pub(crate) fn key_column_for(source: &str) -> Option<&'static str> {
    derived_name_for(source, Derivation::Country)
}

/// Materialized month column for a source schema date column, if one exists.
pub(crate) fn month_column_for(source: &str) -> Option<&'static str> {
    derived_name_for(source, Derivation::Month)
}

fn derived_name_for(source: &str, derivation: Derivation) -> Option<&'static str> {
    DERIVED
        .iter()
        .find(|column| {
            column.source == source
                && std::mem::discriminant(&column.derivation) == std::mem::discriminant(&derivation)
        })
        .map(|column| column.name)
}

/// `"name TYPE"` fragments for the `records` table definition.
pub(crate) fn ddl_definitions() -> Vec<String> {
    DERIVED
        .iter()
        .map(|column| format!("{} {}", column.name, column.sql_type))
        .collect()
}

/// `"name, name, ..."` for an INSERT column list.
pub(crate) fn insert_column_list() -> String {
    DERIVED
        .iter()
        .map(|column| column.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `"name = <expr>, ..."` backfill assignments computed from the raw columns
/// through the registered SQL functions, so existing rows match import-time
/// materialization exactly (used once during schema migration).
pub(crate) fn backfill_assignments() -> String {
    DERIVED
        .iter()
        .map(|column| format!("{} = {}", column.name, backfill_expr(column)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn backfill_expr(column: &DerivedColumn) -> String {
    match column.derivation {
        Derivation::Number(NumberStyle::PreferDecimal) => {
            format!("num_value({})", column.source)
        }
        Derivation::Number(NumberStyle::PreferGrouped) => {
            format!("num_value_grouped({})", column.source)
        }
        Derivation::Label => format!("label_value({})", column.source),
        Derivation::Country => format!("country_key({})", column.source),
        // Use the same month_key UDF the import path uses, so a backfilled
        // legacy row and a freshly imported row derive the identical month
        // for every date shape (an ISO-shaped but invalid value like
        // "1234-56-78" yields "" in both, not a bogus "1234-56").
        Derivation::Month => format!("month_key({})", column.source),
    }
}

fn month_of(date: &str) -> String {
    month_key(date)
}

#[cfg(test)]
mod tests {
    use super::{DERIVED, Derivation, compute, month_of};
    use crate::schema::col_index;
    use crate::storage::normalize::NumberStyle;
    use rusqlite::types::Value;

    #[test]
    fn every_derived_source_is_a_schema_column() {
        for column in DERIVED {
            assert!(
                col_index(column.source).is_some(),
                "derived column {} has unknown source {}",
                column.name,
                column.source
            );
        }
    }

    #[test]
    fn derived_names_are_unique_and_not_schema_columns() {
        let mut seen = std::collections::HashSet::new();
        for column in DERIVED {
            assert!(
                seen.insert(column.name),
                "duplicate derived {}",
                column.name
            );
            assert!(
                col_index(column.name).is_none(),
                "derived {} collides with a schema column",
                column.name
            );
        }
    }

    #[test]
    fn semantic_derived_columns_match_schema_profile() {
        use crate::domain::table::SemanticField;
        use crate::schema::column_for_semantic;
        // A derived column that materializes a semantic field must read that
        // field's profile column, so the schema profile, the import semantics,
        // and these materialized columns cannot drift apart.
        let pairs = [
            ("value_num", SemanticField::Value),
            ("net_kg_num", SemanticField::NetWeight),
            ("gross_kg_num", SemanticField::GrossWeight),
            ("quantity_num", SemanticField::Quantity),
            ("sender_label", SemanticField::Sender),
            ("recipient_label", SemanticField::Recipient),
            ("edrpou_label", SemanticField::CompanyCode),
            ("trademark_label", SemanticField::Trademark),
            ("origin_key", SemanticField::OriginCountry),
            ("dispatch_key", SemanticField::DispatchCountry),
            ("trade_key", SemanticField::TradeCountry),
            ("month", SemanticField::Date),
        ];
        for (name, field) in pairs {
            let column = DERIVED
                .iter()
                .find(|column| column.name == name)
                .expect("derived column exists");
            assert_eq!(
                Some(column.source),
                column_for_semantic(field),
                "derived {name} must read the {field:?} profile column"
            );
        }
    }

    #[test]
    fn compute_matches_normalization_rules() {
        assert_eq!(
            compute(Derivation::Number(NumberStyle::PreferDecimal), "1 200,50"),
            Value::Real(1200.50)
        );
        assert_eq!(
            compute(
                Derivation::Number(NumberStyle::PreferDecimal),
                "not a number"
            ),
            Value::Null
        );
        // Grouped style (totals/quantities): a lone three-digit tail is a
        // thousands group; decimal style (weights) keeps three decimals.
        assert_eq!(
            compute(Derivation::Number(NumberStyle::PreferGrouped), "1,250"),
            Value::Real(1250.0)
        );
        assert_eq!(
            compute(Derivation::Number(NumberStyle::PreferDecimal), "1.250"),
            Value::Real(1.25)
        );
        assert_eq!(
            compute(Derivation::Number(NumberStyle::PreferGrouped), "1.23E+08"),
            Value::Real(123_000_000.0)
        );
        assert_eq!(
            compute(Derivation::Label, "  0 "),
            Value::Text(String::new())
        );
        assert_eq!(
            compute(Derivation::Country, "КИТАЙ"),
            Value::Text("CN".to_string())
        );
        assert_eq!(month_of("2024-03-15"), "2024-03");
        assert_eq!(month_of("not a date"), "");
    }
}
