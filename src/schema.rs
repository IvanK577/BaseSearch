//! Compatibility profile column definitions and database mapping.
//!
//! The public product should treat this as one optional semantic profile, not
//! as the universal data model. New generic-document work belongs in
//! `crate::domain`.

use crate::domain::table::SemanticField;

pub struct ColumnDef {
    /// Source Excel column header as it appears in the compatibility profile.
    pub header: &'static str,
    /// SQLite column name.
    pub name: &'static str,
}

pub const COLUMNS: [ColumnDef; 41] = [
    ColumnDef {
        header: "Час оформлення",
        name: "clearance_time",
    },
    ColumnDef {
        header: "Назва ПМО",
        name: "customs_office",
    },
    ColumnDef {
        header: "Тип",
        name: "declaration_type",
    },
    ColumnDef {
        header: "Номер МД",
        name: "declaration_number",
    },
    ColumnDef {
        header: "Дата",
        name: "declaration_date",
    },
    ColumnDef {
        header: "Відправник",
        name: "sender",
    },
    ColumnDef {
        header: "ЕДРПОУ",
        name: "edrpou",
    },
    ColumnDef {
        header: "Одержувач",
        name: "recipient",
    },
    ColumnDef {
        header: "№",
        name: "item_number",
    },
    ColumnDef {
        header: "Код товару",
        name: "product_code",
    },
    ColumnDef {
        header: "Опис товару",
        name: "description",
    },
    ColumnDef {
        header: "Кр.торг.",
        name: "trade_country",
    },
    ColumnDef {
        header: "Кр.відпр.",
        name: "dispatch_country",
    },
    ColumnDef {
        header: "Кр.пох.",
        name: "origin_country",
    },
    ColumnDef {
        header: "Умови пост.",
        name: "delivery_terms",
    },
    ColumnDef {
        header: "Місце пост",
        name: "delivery_place",
    },
    ColumnDef {
        header: "К-ть",
        name: "quantity",
    },
    ColumnDef {
        header: "Один.вим.",
        name: "unit",
    },
    ColumnDef {
        header: "Брутто, кг.",
        name: "gross_kg",
    },
    ColumnDef {
        header: "Нетто, кг.",
        name: "net_kg",
    },
    ColumnDef {
        header: "Вага по МД",
        name: "declaration_weight",
    },
    ColumnDef {
        header: "ФВ вал.контр",
        name: "currency_control_value",
    },
    ColumnDef {
        header: "Особ.перем.",
        name: "movement_feature",
    },
    ColumnDef {
        header: "43",
        name: "field_43",
    },
    ColumnDef {
        header: "43_01",
        name: "field_43_01",
    },
    ColumnDef {
        header: "РФВ Дол/кг.",
        name: "rfv_usd_kg",
    },
    ColumnDef {
        header: "Вага.один.",
        name: "unit_weight",
    },
    ColumnDef {
        header: "Вага різн.",
        name: "weight_difference",
    },
    ColumnDef {
        header: "Контракт",
        name: "contract",
    },
    ColumnDef {
        header: "3001",
        name: "field_3001",
    },
    ColumnDef {
        header: "3002",
        name: "field_3002",
    },
    ColumnDef {
        header: "9610",
        name: "field_9610",
    },
    ColumnDef {
        header: "Торг.марк.",
        name: "trademark",
    },
    ColumnDef {
        header: "РМВ Нетто Дол/кг.",
        name: "rmv_net_usd_kg",
    },
    ColumnDef {
        header: "РМВ Дол/дод.од.",
        name: "rmv_usd_extra_unit",
    },
    ColumnDef {
        header: "РМВ Брутто Дол/кг",
        name: "rmv_gross_usd_kg",
    },
    ColumnDef {
        header: "Призн.Зед",
        name: "zed_purpose",
    },
    ColumnDef {
        header: "Мін.База Дол/кг.",
        name: "min_base_usd_kg",
    },
    ColumnDef {
        header: "Різн.мін.база",
        name: "min_base_difference",
    },
    ColumnDef {
        header: "пільгова",
        name: "preferential",
    },
    ColumnDef {
        header: "повна",
        name: "full_rate",
    },
];

/// Index of the date column in COLUMNS.
pub const DATE_COL: usize = 4;

/// Files are not imported without these columns.
pub const REQUIRED_HEADERS: [&str; 7] = [
    "Номер МД",
    "Дата",
    "Відправник",
    "ЕДРПОУ",
    "Одержувач",
    "Код товару",
    "Опис товару",
];

/// Columns included in the full-text search index.
/// Countries are included so country filters can be accelerated through FTS.
pub const SEARCH_COLUMNS: [&str; 9] = [
    "description",
    "sender",
    "recipient",
    "trademark",
    "declaration_number",
    "product_code",
    "trade_country",
    "dispatch_country",
    "origin_country",
];

/// Legacy compatibility-profile result columns by database name. Newer UI/export
/// paths prefer the persisted table shape when it is available.
pub const RESULT_COLUMNS: [&str; 42] = [
    "clearance_time",
    "customs_office",
    "declaration_type",
    "declaration_number",
    "declaration_date",
    "sender",
    "edrpou",
    "recipient",
    "item_number",
    "product_code",
    "description",
    "trade_country",
    "dispatch_country",
    "origin_country",
    "delivery_terms",
    "delivery_place",
    "quantity",
    "unit",
    "gross_kg",
    "net_kg",
    "declaration_weight",
    "currency_control_value",
    "movement_feature",
    "field_43",
    "field_43_01",
    "rfv_usd_kg",
    "unit_weight",
    "weight_difference",
    "contract",
    "field_3001",
    "field_3002",
    "field_9610",
    "trademark",
    "rmv_net_usd_kg",
    "rmv_usd_extra_unit",
    "rmv_gross_usd_kg",
    "zed_purpose",
    "min_base_usd_kg",
    "min_base_difference",
    "preferential",
    "full_rate",
    "source_file",
];

pub fn col_index(name: &str) -> Option<usize> {
    COLUMNS.iter().position(|c| c.name == name)
}

/// The single source of truth linking this compatibility profile's columns to
/// the domain-neutral [`SemanticField`] roles. The import field semantics, the
/// table shape, and the materialized analytics columns ([`crate::storage::derived`])
/// all resolve through this one table, so the profile is defined in one place.
pub const SEMANTIC_COLUMNS: &[(SemanticField, &str)] = &[
    (SemanticField::Date, "declaration_date"),
    (SemanticField::DeclarationNumber, "declaration_number"),
    (SemanticField::Sender, "sender"),
    (SemanticField::CompanyCode, "edrpou"),
    (SemanticField::Recipient, "recipient"),
    (SemanticField::ProductCode, "product_code"),
    (SemanticField::Description, "description"),
    (SemanticField::TradeCountry, "trade_country"),
    (SemanticField::DispatchCountry, "dispatch_country"),
    (SemanticField::OriginCountry, "origin_country"),
    (SemanticField::Quantity, "quantity"),
    (SemanticField::GrossWeight, "gross_kg"),
    (SemanticField::NetWeight, "net_kg"),
    (SemanticField::Value, "currency_control_value"),
    (SemanticField::Trademark, "trademark"),
];

/// Semantic role carried by a profile column, if any.
pub fn semantic_for_column(name: &str) -> Option<SemanticField> {
    SEMANTIC_COLUMNS
        .iter()
        .find(|(_, column)| *column == name)
        .map(|(field, _)| *field)
}

/// Profile column that carries a semantic role, if any.
pub fn column_for_semantic(field: SemanticField) -> Option<&'static str> {
    SEMANTIC_COLUMNS
        .iter()
        .find(|(candidate, _)| *candidate == field)
        .map(|(_, column)| *column)
}

/// Expanded meaning for abbreviated source columns, shown as a hover hint on
/// result-table headers. RV means calculated value; FRV means invoice
/// calculated value.
pub fn column_glossary(name: &str) -> Option<&'static str> {
    Some(match name {
        "clearance_time" => "Час оформлення — час реєстрації або завершення митного оформлення.",
        "customs_office" => "Назва ПМО — підрозділ митного оформлення / митний пост.",
        "declaration_type" => "Тип — тип митної декларації або митного режиму.",
        "declaration_number" => "Номер МД — номер митної декларації.",
        "declaration_date" => "Дата — дата оформлення митної декларації.",
        "sender" => "Відправник — компанія або особа, що відправила товар.",
        "edrpou" => "ЄДРПОУ — код української компанії-одержувача або учасника операції.",
        "recipient" => "Одержувач — компанія або особа, що отримувала товар.",
        "item_number" => "№ — номер товарної позиції всередині декларації.",
        "product_code" => "Код товару — код УКТЗЕД / HS для класифікації товару.",
        "description" => "Опис товару — текстовий опис товарної позиції з джерела.",
        "trade_country" => "Кр.торг. — країна торгівлі або країна контракту.",
        "dispatch_country" => "Кр.відпр. — країна, з якої товар був відправлений.",
        "origin_country" => "Кр.пох. — країна походження товару.",
        "delivery_terms" => "Умови пост. — умови поставки, зазвичай Incoterms.",
        "delivery_place" => "Місце пост — місце поставки за умовами контракту.",
        "quantity" => "К-ть — кількість товару у вказаній одиниці виміру.",
        "unit" => "Один.вим. — одиниця виміру кількості товару.",
        "gross_kg" => "Брутто, кг. — вага товару з упаковкою.",
        "net_kg" => "Нетто, кг. — вага товару без упаковки.",
        "declaration_weight" => {
            "Вага по МД — вага, зазначена або розрахована по митній декларації."
        }
        "movement_feature" => {
            "Особ.перем. — особливість переміщення товару, якщо вона є в джерелі."
        }
        "field_43" => "43 — графа 43 митної декларації; метод визначення митної вартості.",
        "field_43_01" => "43_01 — додаткове або уточнювальне значення для графи 43.",
        "unit_weight" => "Вага.один. — вага однієї одиниці товару.",
        "weight_difference" => "Вага різн. — різниця ваги або контрольне відхилення ваги.",
        "contract" => "Контракт — номер, дата або ознака зовнішньоекономічного контракту.",
        "field_3001" => "3001 — мито, грн., якщо це поле є в початковому митному реєстрі.",
        "field_3002" => "3002 — акциз, грн., якщо це поле є в початковому митному реєстрі.",
        "field_9610" => "9610 — ПДВ, грн., якщо це поле є в початковому митному реєстрі.",
        "trademark" => "Торг.марк. — торгова марка або бренд товару.",
        "zed_purpose" => "Призн.Зед — ознака або призначення зовнішньоекономічної операції.",
        "min_base_difference" => "Різн.мін.база — різниця відносно мінімальної розрахункової бази.",
        "preferential" => "пільгова — пільгова ставка або сума мита, якщо є в джерелі.",
        "full_rate" => "повна — повна ставка або сума мита, якщо є в джерелі.",
        "source_file" => "Файл — Excel-файл, з якого була імпортована ця строка.",
        "currency_control_value" => "ФВ вал.контр — фактурна вартість у валюті контракту",
        "rfv_usd_kg" => "РФВ — розрахункова фактурна вартість (ФРВ), дол/кг",
        "rmv_net_usd_kg" => "РМВ нетто — розрахункова митна вартість за нетто, дол/кг",
        "rmv_usd_extra_unit" => "РМВ — розрахункова митна вартість, дол за дод. одиницю",
        "rmv_gross_usd_kg" => "РМВ брутто — розрахункова митна вартість за брутто, дол/кг",
        "min_base_usd_kg" => "Мін.База — мінімальна розрахункова вартість (РВ), дол/кг",
        _ => return None,
    })
}

/// Source-file header for a database column name; source_file maps to file.
pub fn header_for(name: &str) -> &'static str {
    if name == "source_file" {
        return "File";
    }
    COLUMNS
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.header)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::header_for;

    #[test]
    fn source_file_uses_the_neutral_api_label() {
        assert_eq!(header_for("source_file"), "File");
    }
}
