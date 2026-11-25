use super::AnalyticsView;
use crate::db::{AnalyticsSectionKind, Filters, PivotDim, PivotMetric, Query};
use crate::i18n::{Lang, Tr, fmt};
use crate::search::{
    ConditionOp, ConditionValue, FieldInfo, LogicOp, QueryExpr, default_field_catalog, field_label,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GuidedQuestionSection {
    Product,
    Company,
    Market,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GuidedQuestionKind {
    ProductCompanies,
    ProductAllCompanies,
    ProductGoods,
    ProductCountries,
    ProductPrices,
    ProductTimeline,
    ProductCompaniesByMonth,
    CompanyProfile,
    CompanyGoods,
    CompanySuppliers,
    CompanyCountries,
    CompanyTimeline,
    CompanyGoodsByMonth,
    MarketCompanies,
    MarketGoods,
    MarketCountries,
    MarketPrices,
}

pub(super) enum GuidedQuestionAction {
    Analytics(AnalyticsView),
    Explore(AnalyticsSectionKind),
    Pivot(PivotDim, PivotDim, PivotMetric),
    Profile(String),
}

pub(super) fn trunc_label(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('\u{2026}');
    }
    out
}

pub(super) fn condition_op_label(op: ConditionOp, t: &Tr) -> &'static str {
    match op {
        ConditionOp::Contains => t.op_contains,
        ConditionOp::Equals => t.op_equals,
        ConditionOp::StartsWith => t.op_starts_with,
        ConditionOp::IsAnyOf => t.op_is_any_of,
        ConditionOp::Range => t.op_range,
        ConditionOp::IsEmpty => t.op_is_empty,
        ConditionOp::IsNotEmpty => t.op_is_not_empty,
    }
}

pub(super) fn condition_value_label(value: &ConditionValue) -> String {
    match value {
        ConditionValue::None => String::new(),
        ConditionValue::Single(value) => value.clone(),
        ConditionValue::List(values) => values.join(", "),
        ConditionValue::Range { from, to } => {
            format!(
                "{}..{}",
                from.as_deref().unwrap_or_default(),
                to.as_deref().unwrap_or_default()
            )
        }
    }
}

pub(super) fn logic_op_label(op: LogicOp, t: &Tr) -> &'static str {
    match op {
        LogicOp::And => t.v2_match_all,
        LogicOp::Or => t.v2_match_any,
    }
}

pub(super) fn group_label_for_ui(op: LogicOp, t: &Tr) -> String {
    format!("{}: {}", t.v2_group, logic_op_label(op, t))
}

pub(super) fn expr_label_for_ui(expr: &QueryExpr, catalog: &[FieldInfo], t: &Tr) -> String {
    match expr {
        QueryExpr::Group(group) => {
            let mut text = group_label_for_ui(group.op, t);
            if group.negated {
                text = format!("{}: {text}", t.v2_excluding);
            }
            text
        }
        QueryExpr::Condition(condition) => {
            let value = condition_value_label(&condition.value);
            let mut text = if value.trim().is_empty() {
                format!(
                    "{} {}",
                    field_label(&condition.field, catalog),
                    condition_op_label(condition.op, t)
                )
            } else {
                format!(
                    "{} {} {}",
                    field_label(&condition.field, catalog),
                    condition_op_label(condition.op, t),
                    value
                )
            };
            if condition.negated {
                text = format!("{}: {text}", t.v2_excluding);
            }
            text
        }
    }
}

pub(super) fn query_summary(query: &Query, t: &Tr) -> String {
    if query.is_empty() {
        return t.enter_query_hint.to_string();
    }
    let f = &query.filters;
    let mut parts = Vec::new();
    if !query.text.trim().is_empty() {
        parts.push(query.text.trim().to_string());
    }
    for (label, value) in [
        (t.year, &f.year),
        (t.product_code, &f.product_code),
        (t.trademark, &f.trademark),
        (t.description, &f.description),
        (t.sender, &f.sender),
        (t.recipient, &f.recipient),
        (t.edrpou, &f.edrpou),
        (t.trade_country, &f.trade_country),
        (t.dispatch_country, &f.dispatch_country),
        (t.origin_country, &f.origin_country),
    ] {
        let value = value.trim();
        if !value.is_empty() {
            parts.push(format!("{label}: {value}"));
        }
    }
    if let Some(advanced) = &query.advanced
        && !advanced.is_empty()
    {
        let label = expr_label_for_ui(advanced, &default_field_catalog(), t);
        parts.push(fmt(t.v2_query_summary, &[&label]));
    }
    parts.join(" · ")
}

pub(super) fn recent_queries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Недавні запити",
        _ => "Recent searches",
    }
}

pub(super) fn saved_queries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Збережені запити",
        _ => "Saved searches",
    }
}

pub(super) fn save_current_query_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Зберегти поточний запит",
        _ => "Save current search",
    }
}

pub(super) fn empty_recent_queries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Недавніх запитів ще немає",
        _ => "No recent searches yet",
    }
}

pub(super) fn empty_saved_queries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Збережених запитів ще немає",
        _ => "No saved searches yet",
    }
}

pub(super) fn clear_recent_queries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Очистити історію",
        _ => "Clear history",
    }
}

pub(super) fn guided_questions_label(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Questions",
        Lang::Ua => "Питання",
        Lang::De => "Fragen",
        Lang::Es => "Preguntas",
        Lang::Fr => "Questions",
        Lang::Pl => "Pytania",
        Lang::Pt => "Perguntas",
        Lang::Ro => "Întrebări",
        Lang::Hu => "Kérdések",
        Lang::Bg => "Въпроси",
        Lang::Zh => "问题",
    }
}

pub(super) fn guided_questions_hover(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Smart shortcuts for the current product, company, or filtered slice.",
        Lang::Ua => "Розумні сценарії для поточного товару, компанії або фільтра.",
        Lang::De => "Intelligente Wege für Ware, Firma oder aktuellen Filter.",
        Lang::Es => "Atajos inteligentes para el producto, empresa o filtro actual.",
        Lang::Fr => "Raccourcis intelligents pour le produit, l'entreprise ou le filtre actuel.",
        Lang::Pl => "Inteligentne skróty dla towaru, firmy albo bieżącego filtra.",
        Lang::Pt => "Atalhos inteligentes para produto, empresa ou filtro atual.",
        Lang::Ro => "Scurtături inteligente pentru produs, companie sau filtrul curent.",
        Lang::Hu => "Okos útvonalak az aktuális termékhez, céghez vagy szűrőhöz.",
        Lang::Bg => "Умни преки пътища за текущия продукт, фирма или филтър.",
        Lang::Zh => "当前商品、公司或筛选范围的智能分析入口。",
    }
}

pub(super) fn guided_questions_empty(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Enter a product, company, code, year, or country first.",
        Lang::Ua => "Спочатку введіть товар, компанію, код, рік або країну.",
        Lang::De => "Geben Sie zuerst Ware, Firma, Code, Jahr oder Land ein.",
        Lang::Es => "Primero introduzca producto, empresa, código, año o país.",
        Lang::Fr => "Saisissez d'abord un produit, une entreprise, un code, une année ou un pays.",
        Lang::Pl => "Najpierw wpisz towar, firmę, kod, rok albo kraj.",
        Lang::Pt => "Digite primeiro produto, empresa, código, ano ou país.",
        Lang::Ro => "Introduceți mai întâi produs, companie, cod, an sau țară.",
        Lang::Hu => "Először adjon meg terméket, céget, kódot, évet vagy országot.",
        Lang::Bg => "Първо въведете продукт, фирма, код, година или държава.",
        Lang::Zh => "请先输入商品、公司、编码、年份或国家。",
    }
}

pub(super) fn guided_section_title(section: GuidedQuestionSection, lang: Lang) -> &'static str {
    match section {
        GuidedQuestionSection::Product => match lang {
            Lang::En => "For this product or search",
            Lang::Ua => "Для цього товару або запиту",
            Lang::De => "Zu dieser Ware oder Suche",
            Lang::Es => "Para este producto o búsqueda",
            Lang::Fr => "Pour ce produit ou cette recherche",
            Lang::Pl => "Dla tego towaru lub wyszukiwania",
            Lang::Pt => "Para este produto ou busca",
            Lang::Ro => "Pentru acest produs sau căutare",
            Lang::Hu => "Ehhez a termékhez vagy kereséshez",
            Lang::Bg => "За този продукт или търсене",
            Lang::Zh => "针对当前商品或搜索",
        },
        GuidedQuestionSection::Company => match lang {
            Lang::En => "For this company",
            Lang::Ua => "Для цієї компанії",
            Lang::De => "Zu dieser Firma",
            Lang::Es => "Para esta empresa",
            Lang::Fr => "Pour cette entreprise",
            Lang::Pl => "Dla tej firmy",
            Lang::Pt => "Para esta empresa",
            Lang::Ro => "Pentru această companie",
            Lang::Hu => "Ehhez a céghez",
            Lang::Bg => "За тази фирма",
            Lang::Zh => "针对当前公司",
        },
        GuidedQuestionSection::Market => match lang {
            Lang::En => "For the current slice",
            Lang::Ua => "Для поточної вибірки",
            Lang::De => "Zum aktuellen Ausschnitt",
            Lang::Es => "Para la selección actual",
            Lang::Fr => "Pour le périmètre actuel",
            Lang::Pl => "Dla bieżącego zakresu",
            Lang::Pt => "Para o recorte atual",
            Lang::Ro => "Pentru selecția curentă",
            Lang::Hu => "Az aktuális szűréshez",
            Lang::Bg => "За текущата извадка",
            Lang::Zh => "针对当前筛选范围",
        },
    }
}

pub(super) fn guided_question_title(kind: GuidedQuestionKind, lang: Lang) -> &'static str {
    match kind {
        GuidedQuestionKind::ProductCompanies => match lang {
            Lang::En => "Who received or imported it?",
            Lang::Ua => "Хто отримував або ввозив це?",
            Lang::De => "Wer hat es erhalten oder importiert?",
            Lang::Es => "¿Quién lo recibió o importó?",
            Lang::Fr => "Qui l'a reçu ou importé ?",
            Lang::Pl => "Kto to odbierał lub importował?",
            Lang::Pt => "Quem recebeu ou importou isso?",
            Lang::Ro => "Cine a primit sau importat?",
            Lang::Hu => "Ki kapta vagy importálta?",
            Lang::Bg => "Кой го е получавал или внасял?",
            Lang::Zh => "谁接收或进口了它？",
        },
        GuidedQuestionKind::ProductAllCompanies => match lang {
            Lang::En => "Show every company and identifier",
            Lang::Ua => "Показати всі компанії та ЄДРПОУ",
            Lang::De => "Alle Firmen und EDRPOU anzeigen",
            Lang::Es => "Mostrar todas las empresas y EDRPOU",
            Lang::Fr => "Afficher toutes les entreprises et EDRPOU",
            Lang::Pl => "Pokaż wszystkie firmy i EDRPOU",
            Lang::Pt => "Mostrar todas as empresas e EDRPOU",
            Lang::Ro => "Arată toate companiile și EDRPOU",
            Lang::Hu => "Összes cég és EDRPOU megjelenítése",
            Lang::Bg => "Покажи всички фирми и ЕДРПОУ",
            Lang::Zh => "显示所有公司和EDRPOU",
        },
        GuidedQuestionKind::ProductGoods => match lang {
            Lang::En => "Which product codes and brands are inside?",
            Lang::Ua => "Які коди товару та бренди всередині?",
            Lang::De => "Welche Warencodes und Marken stecken darin?",
            Lang::Es => "¿Qué códigos y marcas contiene?",
            Lang::Fr => "Quels codes produit et marques contient-il ?",
            Lang::Pl => "Jakie kody towaru i marki są w środku?",
            Lang::Pt => "Quais códigos de produto e marcas aparecem?",
            Lang::Ro => "Ce coduri de produs și mărci conține?",
            Lang::Hu => "Milyen termékkódok és márkák vannak benne?",
            Lang::Bg => "Какви кодове и марки има вътре?",
            Lang::Zh => "包含哪些商品编码和品牌？",
        },
        GuidedQuestionKind::ProductCountries => match lang {
            Lang::En => "From which countries and routes?",
            Lang::Ua => "З яких країн і маршрутів?",
            Lang::De => "Aus welchen Ländern und Routen?",
            Lang::Es => "¿Desde qué países y rutas?",
            Lang::Fr => "Depuis quels pays et routes ?",
            Lang::Pl => "Z jakich krajów i tras?",
            Lang::Pt => "De quais países e rotas?",
            Lang::Ro => "Din ce țări și rute?",
            Lang::Hu => "Mely országokból és útvonalakon?",
            Lang::Bg => "От кои държави и маршрути?",
            Lang::Zh => "来自哪些国家和路线？",
        },
        GuidedQuestionKind::ProductPrices => match lang {
            Lang::En => "What is the price and $/kg picture?",
            Lang::Ua => "Яка ціна та картина $/кг?",
            Lang::De => "Wie sieht Preis und $/kg aus?",
            Lang::Es => "¿Cómo se ven precio y $/kg?",
            Lang::Fr => "Quelle est la situation prix et $/kg ?",
            Lang::Pl => "Jak wygląda cena i $/kg?",
            Lang::Pt => "Como estão preço e $/kg?",
            Lang::Ro => "Cum arată prețul și $/kg?",
            Lang::Hu => "Milyen az ár és $/kg képe?",
            Lang::Bg => "Как изглеждат цена и $/кг?",
            Lang::Zh => "价格和$/公斤情况如何？",
        },
        GuidedQuestionKind::ProductTimeline => match lang {
            Lang::En => "How did value and weight change by month?",
            Lang::Ua => "Як змінювались вартість і вага по місяцях?",
            Lang::De => "Wie änderten sich Wert und Gewicht je Monat?",
            Lang::Es => "¿Cómo cambiaron valor y peso por mes?",
            Lang::Fr => "Comment valeur et poids ont-ils changé par mois ?",
            Lang::Pl => "Jak zmieniały się wartość i waga miesięcznie?",
            Lang::Pt => "Como valor e peso mudaram por mês?",
            Lang::Ro => "Cum s-au schimbat valoarea și greutatea lunar?",
            Lang::Hu => "Hogyan változott az érték és a súly havonta?",
            Lang::Bg => "Как се променяха стойност и тегло по месеци?",
            Lang::Zh => "金额和重量按月如何变化？",
        },
        GuidedQuestionKind::ProductCompaniesByMonth => match lang {
            Lang::En => "Compare companies by month",
            Lang::Ua => "Порівняти компанії по місяцях",
            Lang::De => "Firmen nach Monaten vergleichen",
            Lang::Es => "Comparar empresas por mes",
            Lang::Fr => "Comparer les entreprises par mois",
            Lang::Pl => "Porównaj firmy według miesięcy",
            Lang::Pt => "Comparar empresas por mês",
            Lang::Ro => "Compară companiile pe luni",
            Lang::Hu => "Cégek összehasonlítása hónaponként",
            Lang::Bg => "Сравни фирмите по месеци",
            Lang::Zh => "按月比较公司",
        },
        GuidedQuestionKind::CompanyProfile => match lang {
            Lang::En => "Open the full company dossier",
            Lang::Ua => "Відкрити повне досьє компанії",
            Lang::De => "Vollständiges Firmendossier öffnen",
            Lang::Es => "Abrir el expediente completo de la empresa",
            Lang::Fr => "Ouvrir le dossier complet de l'entreprise",
            Lang::Pl => "Otwórz pełny profil firmy",
            Lang::Pt => "Abrir o dossiê completo da empresa",
            Lang::Ro => "Deschide dosarul complet al companiei",
            Lang::Hu => "Teljes cégdosszié megnyitása",
            Lang::Bg => "Отвори пълното досие на фирмата",
            Lang::Zh => "打开完整公司档案",
        },
        GuidedQuestionKind::CompanyGoods => match lang {
            Lang::En => "What did this company move?",
            Lang::Ua => "Що переміщувала ця компанія?",
            Lang::De => "Welche Waren bewegte diese Firma?",
            Lang::Es => "¿Qué movió esta empresa?",
            Lang::Fr => "Quelles marchandises cette entreprise a-t-elle traitées ?",
            Lang::Pl => "Co przewoziła ta firma?",
            Lang::Pt => "O que esta empresa movimentou?",
            Lang::Ro => "Ce a transportat această companie?",
            Lang::Hu => "Mit mozgatott ez a cég?",
            Lang::Bg => "Какво е превозвала тази фирма?",
            Lang::Zh => "这家公司运输了什么？",
        },
        GuidedQuestionKind::CompanySuppliers => match lang {
            Lang::En => "Who supplied this company?",
            Lang::Ua => "Хто постачав цій компанії?",
            Lang::De => "Wer belieferte diese Firma?",
            Lang::Es => "¿Quién abasteció a esta empresa?",
            Lang::Fr => "Qui a fourni cette entreprise ?",
            Lang::Pl => "Kto dostarczał tej firmie?",
            Lang::Pt => "Quem forneceu para esta empresa?",
            Lang::Ro => "Cine a furnizat această companie?",
            Lang::Hu => "Ki szállított ennek a cégnek?",
            Lang::Bg => "Кой е доставял на тази фирма?",
            Lang::Zh => "谁给这家公司供货？",
        },
        GuidedQuestionKind::CompanyCountries => match lang {
            Lang::En => "Which countries did it work with?",
            Lang::Ua => "З якими країнами вона працювала?",
            Lang::De => "Mit welchen Ländern arbeitete sie?",
            Lang::Es => "¿Con qué países trabajó?",
            Lang::Fr => "Avec quels pays a-t-elle travaillé ?",
            Lang::Pl => "Z jakimi krajami współpracowała?",
            Lang::Pt => "Com quais países trabalhou?",
            Lang::Ro => "Cu ce țări a lucrat?",
            Lang::Hu => "Mely országokkal dolgozott?",
            Lang::Bg => "С кои държави е работила?",
            Lang::Zh => "它与哪些国家往来？",
        },
        GuidedQuestionKind::CompanyTimeline => match lang {
            Lang::En => "How did this company change by month?",
            Lang::Ua => "Як ця компанія змінювалась по місяцях?",
            Lang::De => "Wie veränderte sich diese Firma je Monat?",
            Lang::Es => "¿Cómo cambió esta empresa por mes?",
            Lang::Fr => "Comment cette entreprise a-t-elle évolué par mois ?",
            Lang::Pl => "Jak firma zmieniała się miesięcznie?",
            Lang::Pt => "Como esta empresa mudou por mês?",
            Lang::Ro => "Cum s-a schimbat compania pe luni?",
            Lang::Hu => "Hogyan változott a cég havonta?",
            Lang::Bg => "Как се променяше фирмата по месеци?",
            Lang::Zh => "这家公司按月如何变化？",
        },
        GuidedQuestionKind::CompanyGoodsByMonth => match lang {
            Lang::En => "Compare product codes by month",
            Lang::Ua => "Порівняти коди товару по місяцях",
            Lang::De => "Warencodes nach Monaten vergleichen",
            Lang::Es => "Comparar códigos de producto por mes",
            Lang::Fr => "Comparer les codes produit par mois",
            Lang::Pl => "Porównaj kody towarów według miesięcy",
            Lang::Pt => "Comparar códigos de produto por mês",
            Lang::Ro => "Compară codurile de produs pe luni",
            Lang::Hu => "Termékkódok összehasonlítása hónaponként",
            Lang::Bg => "Сравни кодовете по месеци",
            Lang::Zh => "按月比较商品编码",
        },
        GuidedQuestionKind::MarketCompanies => match lang {
            Lang::En => "Who are the biggest companies here?",
            Lang::Ua => "Хто найбільші компанії у вибірці?",
            Lang::De => "Wer sind hier die größten Firmen?",
            Lang::Es => "¿Cuáles son las empresas más grandes aquí?",
            Lang::Fr => "Quelles sont les plus grandes entreprises ici ?",
            Lang::Pl => "Które firmy są tu największe?",
            Lang::Pt => "Quais são as maiores empresas aqui?",
            Lang::Ro => "Care sunt cele mai mari companii aici?",
            Lang::Hu => "Melyek itt a legnagyobb cégek?",
            Lang::Bg => "Кои са най-големите фирми тук?",
            Lang::Zh => "这里最大的公司是谁？",
        },
        GuidedQuestionKind::MarketGoods => match lang {
            Lang::En => "Which goods dominate this slice?",
            Lang::Ua => "Які товари домінують у вибірці?",
            Lang::De => "Welche Waren dominieren diesen Ausschnitt?",
            Lang::Es => "¿Qué mercancías dominan esta selección?",
            Lang::Fr => "Quelles marchandises dominent ce périmètre ?",
            Lang::Pl => "Jakie towary dominują w tym zakresie?",
            Lang::Pt => "Quais mercadorias dominam este recorte?",
            Lang::Ro => "Ce mărfuri domină această selecție?",
            Lang::Hu => "Mely áruk dominálnak ebben a szűrésben?",
            Lang::Bg => "Кои стоки доминират в тази извадка?",
            Lang::Zh => "这个范围内哪些商品占主导？",
        },
        GuidedQuestionKind::MarketCountries => match lang {
            Lang::En => "Which countries and routes dominate?",
            Lang::Ua => "Які країни та маршрути домінують?",
            Lang::De => "Welche Länder und Routen dominieren?",
            Lang::Es => "¿Qué países y rutas dominan?",
            Lang::Fr => "Quels pays et routes dominent ?",
            Lang::Pl => "Jakie kraje i trasy dominują?",
            Lang::Pt => "Quais países e rotas dominam?",
            Lang::Ro => "Ce țări și rute domină?",
            Lang::Hu => "Mely országok és útvonalak dominálnak?",
            Lang::Bg => "Кои държави и маршрути доминират?",
            Lang::Zh => "哪些国家和路线占主导？",
        },
        GuidedQuestionKind::MarketPrices => match lang {
            Lang::En => "Are prices normal in this slice?",
            Lang::Ua => "Чи нормальні ціни в цій вибірці?",
            Lang::De => "Sind die Preise in diesem Ausschnitt normal?",
            Lang::Es => "¿Son normales los precios en esta selección?",
            Lang::Fr => "Les prix sont-ils normaux dans ce périmètre ?",
            Lang::Pl => "Czy ceny w tym zakresie są normalne?",
            Lang::Pt => "Os preços são normais neste recorte?",
            Lang::Ro => "Sunt prețurile normale în această selecție?",
            Lang::Hu => "Normálisak az árak ebben a szűrésben?",
            Lang::Bg => "Нормални ли са цените в тази извадка?",
            Lang::Zh => "这个范围内价格是否正常？",
        },
    }
}

pub(super) fn exact_edrpou_candidate(text: &str, filters: &Filters) -> Option<String> {
    let from_filter = filters.edrpou.trim();
    if !from_filter.is_empty() {
        return Some(from_filter.to_string());
    }
    let text = text.trim();
    if text.len() == 8 && text.chars().all(|c| c.is_ascii_digit()) {
        Some(text.to_string())
    } else {
        None
    }
}

pub(super) fn guided_questions_for(
    text: &str,
    filters: &Filters,
) -> Vec<(GuidedQuestionSection, GuidedQuestionKind)> {
    let mut out = Vec::new();
    let has_text = !text.trim().is_empty();
    let has_product = has_text
        || !filters.product_code.trim().is_empty()
        || !filters.trademark.trim().is_empty()
        || !filters.description.trim().is_empty();
    let has_company = exact_edrpou_candidate(text, filters).is_some()
        || !filters.recipient.trim().is_empty()
        || !filters.sender.trim().is_empty();
    let has_market = !filters.year.trim().is_empty()
        || !filters.origin_country.trim().is_empty()
        || !filters.dispatch_country.trim().is_empty()
        || !filters.trade_country.trim().is_empty();

    if has_product {
        out.extend([
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductCompanies,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductAllCompanies,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductGoods,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductCountries,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductPrices,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductTimeline,
            ),
            (
                GuidedQuestionSection::Product,
                GuidedQuestionKind::ProductCompaniesByMonth,
            ),
        ]);
    }
    if has_company {
        if exact_edrpou_candidate(text, filters).is_some() {
            out.push((
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanyProfile,
            ));
        }
        out.extend([
            (
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanyGoods,
            ),
            (
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanySuppliers,
            ),
            (
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanyCountries,
            ),
            (
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanyTimeline,
            ),
            (
                GuidedQuestionSection::Company,
                GuidedQuestionKind::CompanyGoodsByMonth,
            ),
        ]);
    }
    if has_market || (!has_product && !has_company) {
        out.extend([
            (
                GuidedQuestionSection::Market,
                GuidedQuestionKind::MarketCompanies,
            ),
            (
                GuidedQuestionSection::Market,
                GuidedQuestionKind::MarketGoods,
            ),
            (
                GuidedQuestionSection::Market,
                GuidedQuestionKind::MarketCountries,
            ),
            (
                GuidedQuestionSection::Market,
                GuidedQuestionKind::MarketPrices,
            ),
        ]);
    }
    out
}

pub(super) fn guided_question_action(
    kind: GuidedQuestionKind,
    text: &str,
    filters: &Filters,
) -> Option<GuidedQuestionAction> {
    Some(match kind {
        GuidedQuestionKind::ProductCompanies | GuidedQuestionKind::MarketCompanies => {
            GuidedQuestionAction::Analytics(AnalyticsView::Companies)
        }
        GuidedQuestionKind::ProductAllCompanies => {
            GuidedQuestionAction::Explore(AnalyticsSectionKind::Edrpou)
        }
        GuidedQuestionKind::ProductGoods
        | GuidedQuestionKind::CompanyGoods
        | GuidedQuestionKind::MarketGoods => {
            GuidedQuestionAction::Analytics(AnalyticsView::Products)
        }
        GuidedQuestionKind::ProductCountries
        | GuidedQuestionKind::CompanyCountries
        | GuidedQuestionKind::MarketCountries => {
            GuidedQuestionAction::Analytics(AnalyticsView::Countries)
        }
        GuidedQuestionKind::ProductPrices | GuidedQuestionKind::MarketPrices => {
            GuidedQuestionAction::Analytics(AnalyticsView::Prices)
        }
        GuidedQuestionKind::ProductTimeline | GuidedQuestionKind::CompanyTimeline => {
            GuidedQuestionAction::Analytics(AnalyticsView::Overview)
        }
        GuidedQuestionKind::ProductCompaniesByMonth => {
            GuidedQuestionAction::Pivot(PivotDim::Recipient, PivotDim::Month, PivotMetric::Value)
        }
        GuidedQuestionKind::CompanyProfile => {
            GuidedQuestionAction::Profile(exact_edrpou_candidate(text, filters)?)
        }
        GuidedQuestionKind::CompanySuppliers => {
            GuidedQuestionAction::Explore(AnalyticsSectionKind::Senders)
        }
        GuidedQuestionKind::CompanyGoodsByMonth => {
            GuidedQuestionAction::Pivot(PivotDim::ProductCode, PivotDim::Month, PivotMetric::Value)
        }
    })
}

pub(super) fn analytics_calc_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Як рахуються цифри",
        _ => "How the numbers are calculated",
    }
}

pub(super) fn analytics_calc_short_note(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Усі цифри рахуються за поточним запитом і фільтрами.",
        _ => "All numbers are calculated from the current search and filters.",
    }
}

pub(super) fn analytics_calc_lines(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Ua => &[
            "Рядки = знайдені товарні рядки, не унікальні декларації.",
            "Декларації = унікальні номери МД у поточній вибірці.",
            "Сума = сума поля «ФВ вал.контр», якщо воно заповнене у джерелі.",
            "$/кг = сума / нетто; якщо нетто порожнє або нульове, показник не рахується.",
            "У групах частка рахується від суми; якщо суми немає, використовується нетто, потім кількість рядків.",
            "Аналітика рахує унікальні рядки: дублікати, позначені як повтори, не подвоюють підсумки.",
        ],
        _ => &[
            "Rows = matching rows, not unique document identifiers.",
            "Document IDs = distinct recognized document numbers in the current result set.",
            "Value = SUM of the recognized value field when it is filled.",
            "$/kg = value / net kg; empty or zero net weight is skipped.",
            "Group share uses value first; if value is empty, it falls back to net weight, then row count.",
            "Analytics counts unique rows: duplicate rows flagged as repeats do not double totals.",
        ],
    }
}

pub(super) fn price_average_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Звичайне середнє за рядками з числовим значенням.",
        _ => "Simple average across rows with a numeric value.",
    }
}

pub(super) fn price_weighted_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Середнє, зважене за нетто кг: SUM(ціна * нетто) / SUM(нетто).",
        _ => "Net-kg weighted average: SUM(price * net kg) / SUM(net kg).",
    }
}

pub(super) fn price_median_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Медіана: половина значень нижче, половина вище.",
        _ => "Median: half the values are lower and half are higher.",
    }
}

pub(super) fn price_range_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "P25-P75: середній діапазон без крайніх 25% знизу і зверху.",
        _ => "P25-P75: middle range after excluding the lowest and highest quarters.",
    }
}

pub(super) fn price_count_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Кількість рядків, де цей показник можна прочитати як число.",
        _ => "Rows where this metric can be parsed as a number.",
    }
}
