use serde::{Deserialize, Serialize};

/// Domain-neutral role inferred from a source column.
///
/// These roles describe how a column can be searched, filtered, aggregated, or
/// displayed. They are not tied to one document layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnRole {
    Text,
    Number,
    Date,
    Year,
    Country,
    Code,
    Identifier,
    Money,
    Weight,
}

/// Optional semantic meaning attached by a profile.
///
/// A public Base Search import must preserve every source column even when no
/// semantic field is known. Conservative header inference and profiles can add
/// these hints for better analytics, but the raw columns remain the source of
/// truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SemanticField {
    Date,
    DeclarationNumber,
    CompanyCode,
    Sender,
    Recipient,
    ProductCode,
    Description,
    Trademark,
    Country,
    OriginCountry,
    DispatchCountry,
    TradeCountry,
    Quantity,
    NetWeight,
    GrossWeight,
    Value,
    Currency,
    WeightUnit,
}

/// The two meanings a person may pin for a whole table instead of a column.
///
/// They are exactly the measures analytics refuses to combine without: money is
/// never added across currencies, weights never across units. A source that
/// states neither leaves analytics unable to label a total — the customs
/// profile states neither, since its value column holds the invoice amount and
/// the currency code appears nowhere in the file — so a person has to be able
/// to say it once and have every reading follow.
pub const FIXABLE_SEMANTICS: [SemanticField; 2] =
    [SemanticField::Currency, SemanticField::WeightUnit];

/// Longest fixed value accepted. Currency codes and unit names are short; the
/// bound exists so a pasted paragraph cannot become a grouping label.
pub const MAX_FIXED_VALUE_CHARS: usize = 32;

/// Checks one pinned value and returns it trimmed.
///
/// Import and the post-import editor both resolve through here, so a value the
/// importer would have refused cannot arrive later through the other door.
pub fn validate_fixed_value(semantic: SemanticField, value: &str) -> Result<String, String> {
    if !FIXABLE_SEMANTICS.contains(&semantic) {
        return Err(format!(
            "Fixed values are not supported for {semantic:?}; use Currency or WeightUnit."
        ));
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("Fixed {semantic:?} value must not be empty."));
    }
    if value.chars().count() > MAX_FIXED_VALUE_CHARS {
        return Err(format!(
            "Fixed {semantic:?} value must be at most {MAX_FIXED_VALUE_CHARS} characters."
        ));
    }
    Ok(value.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceColumn {
    pub id: String,
    pub header: String,
    pub source_index: usize,
    pub role: ColumnRole,
    pub semantic: Option<SemanticField>,
    #[serde(default)]
    pub storage: ColumnStorage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColumnStorage {
    #[default]
    SourceJson,
    SchemaColumn(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableShape {
    pub columns: Vec<SourceColumn>,
}

/// Durable identity and storage contract for one column in one source schema.
///
/// `field_id` is safe to expose in saved queries. It is never used as a SQL
/// identifier; canonical storage names are validated against the built-in
/// schema before SQL is generated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSchemaField {
    pub field_id: String,
    pub schema_id: i64,
    pub source_index: usize,
    pub raw_header: String,
    pub header: String,
    pub normalized_header: String,
    pub role: ColumnRole,
    pub semantic: Option<SemanticField>,
    pub storage: ColumnStorage,
}

/// One stable interpretation of an ordered source table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSchema {
    pub id: i64,
    pub public_id: String,
    pub fingerprint: String,
    pub fingerprint_version: u32,
    pub fixed_currency: Option<String>,
    pub fixed_weight_unit: Option<String>,
    pub columns: Vec<SourceSchemaField>,
}

/// Provenance for one successfully imported file sheet or delimited table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSource {
    pub id: i64,
    pub public_id: String,
    pub schema_id: i64,
    pub schema_public_id: String,
    pub source_file: String,
    pub table_name: String,
    pub import_fingerprint: String,
    pub imported_at: String,
}

impl TableShape {
    pub fn from_headers(headers: impl IntoIterator<Item = String>) -> Self {
        let mut seen = std::collections::HashMap::<String, usize>::new();
        let columns = headers
            .into_iter()
            .enumerate()
            .map(|(source_index, header)| {
                let mut header = normalize_header_for_display(&header, source_index);
                let base_id = stable_column_id(&header, source_index);
                let count = seen.entry(base_id.clone()).or_insert(0);
                let id = if *count == 0 {
                    base_id
                } else {
                    header = format!("{header} ({})", *count + 1);
                    format!("{base_id}_{}", *count + 1)
                };
                *count += 1;
                let role = infer_role(&header);
                let semantic = infer_semantic(&id, role);
                SourceColumn {
                    id,
                    header,
                    source_index,
                    role,
                    semantic,
                    storage: ColumnStorage::SourceJson,
                }
            })
            .collect();
        Self { columns }
    }

    pub fn with_semantics(
        mut self,
        semantics: impl IntoIterator<Item = (String, SemanticField)>,
    ) -> Self {
        let semantics = semantics
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        for column in &mut self.columns {
            if let Some(semantic) = semantics.get(&column.id).copied() {
                column.semantic = Some(semantic);
            }
        }
        self
    }
}

fn normalize_header_for_display(header: &str, source_index: usize) -> String {
    let trimmed = header.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        format!("Column {}", source_index + 1)
    } else {
        trimmed
    }
}

/// Historical header-based column id ("Value USD" -> "value_usd"). The source
/// schema registry reuses this exact algorithm so its compatibility shape keeps
/// the ids the rest of the app (and saved semantics) were built around.
pub(crate) fn stable_column_id(header: &str, source_index: usize) -> String {
    let mut out = String::new();
    let mut last_sep = false;
    for ch in header.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_sep = false;
        } else if !out.is_empty() && !last_sep {
            out.push('_');
            last_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        format!("column_{}", source_index + 1)
    } else {
        out
    }
}

fn infer_role(header: &str) -> ColumnRole {
    let normalized = stable_column_id(header, 0);
    let lower = normalized.as_str();
    if lower.contains("date") || lower.contains("дата") {
        ColumnRole::Date
    } else if lower == "year" || lower.contains("рік") || lower.contains("год") {
        ColumnRole::Year
    } else if lower.contains("country") || lower.contains("краї") || lower.contains("стра")
    {
        ColumnRole::Country
    } else if lower.contains("code") || lower.contains("код") || lower.contains("sku") {
        ColumnRole::Code
    } else if lower.contains("id") || lower.contains("number") || lower.contains("номер") {
        ColumnRole::Identifier
    } else if lower.contains("price")
        || lower.contains("value")
        || lower.contains("amount")
        || lower.contains("варт")
        || lower.contains("сум")
    {
        ColumnRole::Money
    } else if lower.contains("weight") || lower.contains("вага") || lower.contains("kg") {
        ColumnRole::Weight
    } else if lower.contains("qty") || lower.contains("quantity") || lower.contains("кільк") {
        ColumnRole::Number
    } else {
        ColumnRole::Text
    }
}

fn infer_semantic(id: &str, role: ColumnRole) -> Option<SemanticField> {
    match id {
        "date" | "order_date" | "document_date" | "declaration_date" | "clearance_date" => {
            Some(SemanticField::Date)
        }
        "declaration" | "declaration_no" | "declaration_number" | "declaration_id" | "invoice"
        | "invoice_no" | "invoice_number" | "document_no" | "document_number" => {
            Some(SemanticField::DeclarationNumber)
        }
        "sender" | "sender_name" | "supplier" | "supplier_name" | "exporter" | "exporter_name"
        | "shipper" | "shipper_name" | "seller" | "seller_name" => Some(SemanticField::Sender),
        "recipient" | "recipient_name" | "receiver" | "receiver_name" | "buyer" | "buyer_name"
        | "customer" | "customer_name" | "importer" | "importer_name" | "consignee"
        | "consignee_name" => Some(SemanticField::Recipient),
        "edrpou" | "company_code" | "company_id" | "recipient_code" | "recipient_id"
        | "buyer_code" | "buyer_id" | "importer_code" | "importer_id" => {
            Some(SemanticField::CompanyCode)
        }
        "product_code" | "goods_code" | "commodity_code" | "item_code" | "sku" | "hs_code"
        | "hscode" | "hs" | "uktzed" | "ukt_zed" | "uktzed_code" | "tnved" | "tnved_code" => {
            Some(SemanticField::ProductCode)
        }
        "description"
        | "product_description"
        | "goods_description"
        | "item_description"
        | "commodity_description"
        | "product_name"
        | "goods_name"
        | "item_name" => Some(SemanticField::Description),
        "trademark" | "trade_mark" | "brand" | "brand_name" | "mark" | "manufacturer_brand" => {
            Some(SemanticField::Trademark)
        }
        "country" => Some(SemanticField::Country),
        "origin" | "origin_country" | "country_origin" | "country_of_origin" => {
            Some(SemanticField::OriginCountry)
        }
        "dispatch_country"
        | "country_dispatch"
        | "country_of_dispatch"
        | "ship_from"
        | "shipping_country"
        | "departure_country"
        | "source_country" => Some(SemanticField::DispatchCountry),
        "trade_country" | "trading_country" | "country_of_trade" | "seller_country" => {
            Some(SemanticField::TradeCountry)
        }
        "quantity" | "qty" | "count" | "units" | "unit_count" => Some(SemanticField::Quantity),
        "net_kg" | "net_weight" | "net_weight_kg" | "weight_net" | "weight_net_kg" => {
            Some(SemanticField::NetWeight)
        }
        "gross_kg" | "gross_weight" | "gross_weight_kg" | "weight_gross" | "weight_gross_kg" => {
            Some(SemanticField::GrossWeight)
        }
        "value" | "value_usd" | "amount" | "amount_usd" | "total_value" | "total_value_usd"
        | "invoice_value" | "invoice_value_usd" | "customs_value" | "customs_value_usd" => {
            Some(SemanticField::Value)
        }
        "currency" | "currency_code" | "value_currency" | "amount_currency" | "ccy"
        | "iso_currency" => Some(SemanticField::Currency),
        "weight_unit" | "net_weight_unit" | "mass_unit" | "weight_uom" | "mass_uom" => {
            Some(SemanticField::WeightUnit)
        }
        _ if role == ColumnRole::Year => Some(SemanticField::Date),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnRole, SemanticField, TableShape};

    #[test]
    fn table_shape_keeps_every_source_column_first_class() {
        let shape = TableShape::from_headers([
            "SKU".to_string(),
            "Price EUR".to_string(),
            "Origin country".to_string(),
            "SKU".to_string(),
            "".to_string(),
        ]);

        assert_eq!(shape.columns.len(), 5);
        assert_eq!(shape.columns[0].id, "sku");
        assert_eq!(shape.columns[1].role, ColumnRole::Money);
        assert_eq!(shape.columns[2].role, ColumnRole::Country);
        assert_eq!(shape.columns[3].id, "sku_2");
        assert_eq!(shape.columns[4].header, "Column 5");
    }

    #[test]
    fn common_business_headers_get_conservative_semantics() {
        let shape = TableShape::from_headers([
            "Brand".to_string(),
            "Recipient".to_string(),
            "Product code".to_string(),
            "Value USD".to_string(),
            "Net kg".to_string(),
        ]);

        let semantics = shape
            .columns
            .iter()
            .map(|column| (column.id.as_str(), column.semantic))
            .collect::<Vec<_>>();
        assert_eq!(semantics[0], ("brand", Some(SemanticField::Trademark)));
        assert_eq!(semantics[1], ("recipient", Some(SemanticField::Recipient)));
        assert_eq!(
            semantics[2],
            ("product_code", Some(SemanticField::ProductCode))
        );
        assert_eq!(semantics[3], ("value_usd", Some(SemanticField::Value)));
        assert_eq!(semantics[4], ("net_kg", Some(SemanticField::NetWeight)));
    }
}
