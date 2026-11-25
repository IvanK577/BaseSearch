use std::collections::HashMap;
use std::sync::LazyLock;

use crate::domain::table::SemanticField;
use crate::schema::COLUMNS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AliasMatch {
    pub semantic: SemanticField,
    pub needs_sample_confirmation: bool,
}

static ALIASES: LazyLock<HashMap<String, AliasMatch>> = LazyLock::new(|| {
    let mut aliases = HashMap::new();
    for (semantic, weak, values) in ALIAS_GROUPS {
        for value in *values {
            let key = normalize_header(value);
            let candidate = AliasMatch {
                semantic: *semantic,
                needs_sample_confirmation: *weak,
            };
            match aliases.insert(key.clone(), candidate) {
                Some(previous) if previous.semantic != *semantic => {
                    panic!("ambiguous semantic header alias: {key}")
                }
                _ => {}
            }
        }
    }
    aliases
});

static CANONICAL_ALIASES: LazyLock<HashMap<String, Option<usize>>> = LazyLock::new(|| {
    let mut aliases = HashMap::new();
    for (index, column) in COLUMNS.iter().enumerate() {
        for value in [column.name, column.header] {
            insert_canonical_alias(&mut aliases, value, index);
        }
    }
    for (target, values) in CANONICAL_ALIAS_GROUPS {
        let target = COLUMNS
            .iter()
            .position(|column| column.name == *target)
            .expect("canonical import alias target must exist");
        for value in *values {
            insert_canonical_alias(&mut aliases, value, target);
        }
    }
    aliases
});

fn insert_canonical_alias(
    aliases: &mut HashMap<String, Option<usize>>,
    value: &str,
    target: usize,
) {
    let key = normalize_header(value);
    if key.is_empty() {
        return;
    }
    aliases
        .entry(key)
        .and_modify(|existing| {
            if *existing != Some(target) {
                *existing = None;
            }
        })
        .or_insert(Some(target));
}

pub(super) fn canonical_column(header: &str) -> Option<usize> {
    let trimmed = header.trim();
    if let Some(index) = COLUMNS
        .iter()
        .position(|column| column.header.trim() == trimmed)
    {
        return Some(index);
    }
    CANONICAL_ALIASES
        .get(&normalize_header(trimmed))
        .copied()
        .flatten()
}

pub(super) fn match_header(header: &str) -> Option<AliasMatch> {
    let key = normalize_header(header);
    if let Some(alias) = ALIASES.get(&key) {
        return Some(*alias);
    }

    // Export tools sometimes append a qualifier or contain a typo after the
    // stable semantic phrase. Prefixes are deliberately long to avoid mapping
    // generic words such as "number" or "code".
    let semantic = if key.starts_with("номердекларац")
        || key.starts_with("declarationnumber")
        || key.starts_with("declarationno")
    {
        SemanticField::DeclarationNumber
    } else if key.starts_with("датадокумент")
        || key.starts_with("датаоформлен")
        || key.starts_with("documentdate")
    {
        SemanticField::Date
    } else if key.starts_with("кодтовар")
        || key.starts_with("productcode")
        || key.starts_with("commoditycode")
    {
        SemanticField::ProductCode
    } else if key.starts_with("опистовар")
        || key.starts_with("описаниетовар")
        || key.starts_with("productdescription")
        || key.starts_with("goodsdescription")
    {
        SemanticField::Description
    } else if key.starts_with("фактурнаварт")
        || key.starts_with("фактурнаястоим")
        || key.starts_with("invoicevalue")
    {
        SemanticField::Value
    } else {
        return None;
    };
    Some(AliasMatch {
        semantic,
        needs_sample_confirmation: false,
    })
}

pub(super) fn normalize_header(value: &str) -> String {
    value
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const ALIAS_GROUPS: &[(SemanticField, bool, &[&str])] = &[
    (
        SemanticField::Date,
        false,
        &[
            "date",
            "order date",
            "document date",
            "declaration date",
            "clearance date",
            "invoice date",
            "дата",
            "дата документа",
            "дата декларації",
            "дата оформлення",
            "дата декларации",
            "дата оформления",
            "дата мд",
        ],
    ),
    (
        SemanticField::DeclarationNumber,
        false,
        &[
            "declaration",
            "declaration no",
            "declaration number",
            "declaration id",
            "document no",
            "document number",
            "invoice no",
            "invoice number",
            "номер декларації",
            "номер декларации",
            "номер мд",
            "номер вмд",
            "номер гтд",
            "номер документа",
        ],
    ),
    (
        SemanticField::CompanyCode,
        false,
        &[
            "company code",
            "company id",
            "recipient code",
            "recipient id",
            "buyer code",
            "buyer id",
            "importer code",
            "importer id",
            "edrpou",
            "єдрпоу",
            "едрпоу",
            "код єдрпоу",
            "код едрпоу",
            "код компанії",
            "код компании",
            "код фірми отримувача",
            "код фирмы получателя",
        ],
    ),
    (
        SemanticField::Sender,
        false,
        &[
            "sender",
            "sender name",
            "supplier",
            "supplier name",
            "exporter",
            "exporter name",
            "shipper",
            "shipper name",
            "seller",
            "seller name",
            "відправник",
            "відпправник",
            "назва відправника",
            "назва фірми відправника",
            "отправитель",
            "наименование отправителя",
            "название фирмы отправителя",
        ],
    ),
    (
        SemanticField::Recipient,
        false,
        &[
            "recipient",
            "recipient name",
            "receiver",
            "receiver name",
            "buyer",
            "buyer name",
            "customer",
            "customer name",
            "importer",
            "importer name",
            "consignee",
            "consignee name",
            "одержувач",
            "отримувач",
            "покупець",
            "назва покупця",
            "назва отримувача",
            "назва фірми отримувача",
            "получатель",
            "покупатель",
            "наименование получателя",
            "название фирмы получателя",
        ],
    ),
    (
        SemanticField::ProductCode,
        false,
        &[
            "sku",
            "article",
            "article no",
            "product code",
            "item code",
            "goods code",
            "commodity code",
            "hs",
            "hs code",
            "hscode",
            "uktzed",
            "ukt zed",
            "uktzed code",
            "tnved",
            "tnved code",
            "артикул",
            "артикул товару",
            "код товару",
            "код уктзед",
            "код товар",
            "код товара",
            "код тнвэд",
        ],
    ),
    (
        SemanticField::Description,
        false,
        &[
            "description",
            "product description",
            "product name",
            "item description",
            "item name",
            "goods description",
            "goods name",
            "commodity description",
            "опис",
            "опис товару",
            "опис позиції",
            "найменування товару",
            "назва товару",
            "описание",
            "описание товара",
            "наименование товара",
            "название товара",
        ],
    ),
    (
        SemanticField::Trademark,
        false,
        &[
            "brand",
            "brand name",
            "trademark",
            "trade mark",
            "manufacturer brand",
            "бренд",
            "торгова марка",
            "торг марка",
            "торговая марка",
        ],
    ),
    (
        SemanticField::Country,
        true,
        &["country", "країна", "страна"],
    ),
    (
        SemanticField::OriginCountry,
        false,
        &[
            "origin",
            "origin country",
            "country of origin",
            "country origin",
            "країна походження",
            "кр походж",
            "страна происхождения",
        ],
    ),
    (
        SemanticField::DispatchCountry,
        false,
        &[
            "dispatch country",
            "country of dispatch",
            "ship from",
            "shipping country",
            "departure country",
            "source country",
            "країна відправлення",
            "кр відпр",
            "страна отправления",
        ],
    ),
    (
        SemanticField::TradeCountry,
        false,
        &[
            "trade country",
            "trading country",
            "country of trade",
            "seller country",
            "торгуюча країна",
            "країна торгівлі",
            "кр торг",
            "торгующая страна",
            "страна торговли",
        ],
    ),
    (
        SemanticField::Quantity,
        true,
        &[
            "quantity",
            "qty",
            "count",
            "unit count",
            "кількість",
            "к ть",
            "количество",
        ],
    ),
    (
        SemanticField::NetWeight,
        false,
        &[
            "net kg",
            "net weight",
            "net weight kg",
            "weight net",
            "weight net kg",
            "вага нетто",
            "вага нетто кг",
            "нетто кг",
            "вес нетто",
            "вес нетто кг",
        ],
    ),
    (
        SemanticField::GrossWeight,
        false,
        &[
            "gross kg",
            "gross weight",
            "gross weight kg",
            "weight gross",
            "weight gross kg",
            "вага брутто",
            "вага брутто кг",
            "брутто кг",
            "вес брутто",
            "вес брутто кг",
        ],
    ),
    (
        SemanticField::Value,
        true,
        &[
            "value",
            "value usd",
            "amount",
            "amount usd",
            "total value",
            "total amount",
            "invoice value",
            "customs value",
            "сума",
            "сума рахунку",
            "загальна сума",
            "вартість",
            "фактурна вартість",
            "сумма",
            "сумма счета",
            "общая сумма",
            "стоимость",
            "фактурная стоимость",
        ],
    ),
    (
        SemanticField::Currency,
        false,
        &[
            "currency",
            "currency code",
            "value currency",
            "amount currency",
            "ccy",
            "iso currency",
            "валюта",
            "код валюти",
            "валюта суми",
            "код валюты",
            "валюта суммы",
        ],
    ),
    (
        SemanticField::WeightUnit,
        false,
        &[
            "weight unit",
            "net weight unit",
            "mass unit",
            "weight uom",
            "mass uom",
            "одиниця ваги",
            "одиниця маси",
            "единица веса",
            "единица массы",
        ],
    ),
];

const CANONICAL_ALIAS_GROUPS: &[(&str, &[&str])] = &[(
    "declaration_type",
    &["declaration type", "тип декларації", "тип декларации"],
)];
