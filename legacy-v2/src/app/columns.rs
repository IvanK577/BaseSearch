use crate::schema::column_glossary;
use crate::search::{FieldInfo, FieldKind, FieldRef};

/// Visual cell type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CellKind {
    /// Primary text, such as descriptions and companies.
    Normal,
    /// Secondary text, such as dates, countries, and organization codes.
    Weak,
    /// Product code: monospace and accented.
    Code,
    /// Numbers: monospace and right-aligned.
    Number,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ColumnPreset {
    Basic,
    Companies,
    Goods,
    Prices,
    Logistics,
}

/// Result column width and visual style.
fn col_spec(name: &str) -> (f32, CellKind) {
    match name {
        "clearance_time" => (130.0, CellKind::Weak),
        "customs_office" => (190.0, CellKind::Weak),
        "declaration_type" => (72.0, CellKind::Weak),
        "declaration_date" => (88.0, CellKind::Weak),
        "declaration_number" => (150.0, CellKind::Weak),
        "sender" => (195.0, CellKind::Normal),
        "recipient" => (195.0, CellKind::Normal),
        "item_number" => (58.0, CellKind::Number),
        "description" => (440.0, CellKind::Normal),
        "product_code" => (104.0, CellKind::Code),
        "edrpou" => (88.0, CellKind::Weak),
        "trade_country" | "dispatch_country" | "origin_country" => (76.0, CellKind::Weak),
        "delivery_terms" => (92.0, CellKind::Weak),
        "delivery_place" => (140.0, CellKind::Weak),
        "quantity" => (76.0, CellKind::Number),
        "unit" => (72.0, CellKind::Weak),
        "gross_kg"
        | "net_kg"
        | "declaration_weight"
        | "currency_control_value"
        | "rfv_usd_kg"
        | "unit_weight"
        | "weight_difference"
        | "rmv_net_usd_kg"
        | "rmv_usd_extra_unit"
        | "rmv_gross_usd_kg"
        | "min_base_usd_kg"
        | "min_base_difference"
        | "preferential"
        | "full_rate" => (112.0, CellKind::Number),
        "contract" => (150.0, CellKind::Weak),
        "trademark" => (110.0, CellKind::Weak),
        "source_file" => (140.0, CellKind::Weak),
        _ => (110.0, CellKind::Normal),
    }
}

pub(super) fn field_col_spec(field: &FieldInfo) -> (f32, CellKind) {
    match &field.source {
        FieldRef::Column(name) => col_spec(name),
        // Source-schema fields render by their kind, like extra columns.
        FieldRef::Extra(_) | FieldRef::SourceField(_) => match field.kind {
            FieldKind::Number => (116.0, CellKind::Number),
            FieldKind::Code => (120.0, CellKind::Code),
            FieldKind::Date | FieldKind::Country => (110.0, CellKind::Weak),
            FieldKind::Year => (72.0, CellKind::Weak),
            FieldKind::Text => (160.0, CellKind::Normal),
        },
    }
}

pub(super) fn field_glossary(field: &FieldInfo) -> Option<&'static str> {
    match &field.source {
        FieldRef::Column(name) => column_glossary(name),
        FieldRef::Extra(_) | FieldRef::SourceField(_) => None,
    }
}

fn field_column_name(field: &FieldInfo) -> Option<&str> {
    match &field.source {
        FieldRef::Column(name) => Some(name.as_str()),
        FieldRef::Extra(_) | FieldRef::SourceField(_) => None,
    }
}

pub(super) fn column_group(field: &FieldInfo) -> &'static str {
    match &field.source {
        FieldRef::Extra(_) | FieldRef::SourceField(_) => "Source columns",
        FieldRef::Column(name) => {
            match name.as_str() {
                "declaration_number" | "declaration_date" | "declaration_type"
                | "clearance_time" | "customs_office" | "source_file" => "Documents",
                "sender" | "recipient" | "edrpou" => "Companies",
                "product_code" | "description" | "trademark" | "item_number" | "quantity"
                | "unit" => "Goods",
                "gross_kg" | "net_kg" | "declaration_weight" | "unit_weight"
                | "weight_difference" => "Weights",
                "currency_control_value"
                | "rfv_usd_kg"
                | "rmv_net_usd_kg"
                | "rmv_usd_extra_unit"
                | "rmv_gross_usd_kg"
                | "min_base_usd_kg"
                | "min_base_difference"
                | "preferential"
                | "full_rate"
                | "field_3001"
                | "field_3002"
                | "field_9610" => "Prices and payments",
                "trade_country" | "dispatch_country" | "origin_country" | "delivery_terms"
                | "delivery_place" => "Countries and delivery",
                _ => "Profile details",
            }
        }
    }
}

pub(super) fn field_matches_filter(field: &FieldInfo, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let label = field.label.to_lowercase();
    let id = field.id.to_lowercase();
    let group = column_group(field).to_lowercase();
    let glossary = field_glossary(field).unwrap_or("").to_lowercase();
    label.contains(needle)
        || id.contains(needle)
        || group.contains(needle)
        || glossary.contains(needle)
}

pub(super) fn column_in_preset(field: &FieldInfo, preset: ColumnPreset) -> bool {
    let Some(name) = field_column_name(field) else {
        return matches!(preset, ColumnPreset::Basic);
    };
    match preset {
        ColumnPreset::Basic => matches!(
            name,
            "declaration_date"
                | "declaration_number"
                | "sender"
                | "recipient"
                | "edrpou"
                | "product_code"
                | "description"
                | "currency_control_value"
                | "net_kg"
                | "trademark"
                | "origin_country"
                | "source_file"
        ),
        ColumnPreset::Companies => matches!(
            name,
            "declaration_date"
                | "declaration_number"
                | "sender"
                | "recipient"
                | "edrpou"
                | "trade_country"
                | "dispatch_country"
                | "origin_country"
                | "source_file"
        ),
        ColumnPreset::Goods => matches!(
            name,
            "declaration_date"
                | "product_code"
                | "description"
                | "trademark"
                | "item_number"
                | "quantity"
                | "unit"
                | "net_kg"
                | "gross_kg"
                | "source_file"
        ),
        ColumnPreset::Prices => matches!(
            name,
            "declaration_date"
                | "recipient"
                | "edrpou"
                | "product_code"
                | "currency_control_value"
                | "net_kg"
                | "gross_kg"
                | "rfv_usd_kg"
                | "rmv_net_usd_kg"
                | "rmv_usd_extra_unit"
                | "rmv_gross_usd_kg"
                | "min_base_usd_kg"
                | "min_base_difference"
                | "field_3001"
                | "field_3002"
                | "field_9610"
        ),
        ColumnPreset::Logistics => matches!(
            name,
            "declaration_date"
                | "declaration_number"
                | "sender"
                | "recipient"
                | "trade_country"
                | "dispatch_country"
                | "origin_country"
                | "delivery_terms"
                | "delivery_place"
                | "gross_kg"
                | "net_kg"
                | "declaration_weight"
                | "source_file"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{CellKind, ColumnPreset, column_group, column_in_preset, field_col_spec};
    use crate::search::{FieldInfo, FieldKind, FieldRef, operators_for_kind};

    fn column(id: &str, label: &str, kind: FieldKind) -> FieldInfo {
        FieldInfo {
            id: id.to_string(),
            label: label.to_string(),
            source: FieldRef::Column(id.to_string()),
            kind,
            operators: operators_for_kind(kind).to_vec(),
        }
    }

    #[test]
    fn basic_preset_keeps_working_columns() {
        let description = column("description", "Description", FieldKind::Text);
        let price = column("currency_control_value", "Value", FieldKind::Number);
        let contract = column("contract", "Contract", FieldKind::Text);

        assert!(column_in_preset(&description, ColumnPreset::Basic));
        assert!(column_in_preset(&price, ColumnPreset::Basic));
        assert!(!column_in_preset(&contract, ColumnPreset::Basic));
    }

    #[test]
    fn columns_have_groups_and_visual_kinds() {
        let code = column("product_code", "Product code", FieldKind::Code);
        let value = column("currency_control_value", "Value", FieldKind::Number);

        assert_eq!(column_group(&code), "Goods");
        assert_eq!(column_group(&value), "Prices and payments");
        assert!(matches!(field_col_spec(&code).1, CellKind::Code));
        assert!(matches!(field_col_spec(&value).1, CellKind::Number));
    }
}
