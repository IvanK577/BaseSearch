use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Date,
    Company,
    Product,
    Value,
    Currency,
    Weight,
    WeightUnit,
}

impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Company => "company",
            Self::Product => "product",
            Self::Value => "value",
            Self::Currency => "currency",
            Self::Weight => "weight",
            Self::WeightUnit => "weightUnit",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "date" => Some(Self::Date),
            "company" => Some(Self::Company),
            "product" => Some(Self::Product),
            "value" => Some(Self::Value),
            "currency" => Some(Self::Currency),
            "weight" => Some(Self::Weight),
            "weightUnit" => Some(Self::WeightUnit),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingColumn {
    pub name: String,
    pub role: Option<Role>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSource {
    pub file_name: String,
    pub sheet: String,
    pub fingerprint: String,
    pub columns: Vec<IncomingColumn>,
    pub fixed_currency: Option<String>,
    pub fixed_weight_unit: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub id: String,
    pub label: String,
    pub role: Option<Role>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: i64,
    pub file_name: String,
    pub sheet: String,
    pub rows: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub revision: i64,
    pub row_count: u64,
    pub columns: Vec<Column>,
    pub sources: Vec<Source>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: i64,
    pub source_name: String,
    pub sheet: String,
    pub row_number: Option<u64>,
    pub values: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_columns: Vec<Column>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QuerySpec {
    pub text: String,
    pub filters: Vec<QueryFilter>,
    pub sort: Option<QuerySort>,
    pub revision: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilter {
    pub field: String,
    pub op: FilterOp,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterOp {
    Eq,
    Contains,
    Prefix,
    From,
    To,
    Empty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySort {
    pub field: String,
    pub direction: SortDirection,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub revision: i64,
    pub total_rows: u64,
    pub rows: Vec<Record>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dimension {
    Month,
    Company,
    Product,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub total_rows: u64,
    pub currency_totals: Vec<CurrencyTotal>,
    pub net_weight_kg: Option<String>,
    pub unknown_value_rows: u64,
    pub unknown_unit_rows: u64,
    pub total_groups: u64,
    pub groups: Vec<SummaryGroup>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyTotal {
    pub currency: Option<String>,
    pub amount: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGroup {
    pub key: String,
    pub rows: u64,
    pub share_percent: f64,
    pub change_percent: Option<f64>,
    pub currency_totals: Vec<CurrencyTotal>,
    pub net_weight_kg: Option<String>,
    pub filter: QueryFilter,
}
