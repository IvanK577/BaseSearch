use super::ACCENT;
use super::format::{
    fmt_compact, fmt_decimal, fmt_money_compact, fmt_money_per_kg, value_per_document,
};
use crate::db::AnalyticsOverview;
use crate::i18n::{Lang, group_digits, tr};

/// One stat card: title, headline figure, two supporting lines, hover help.
pub(super) type StoryCard = (&'static str, String, String, String, &'static str);

/// The text of the four overview cards, separated from drawing them so the
/// numbers can be asserted without an egui context. Every money string here
/// resolves through the currency-bucketed measures; reading the raw
/// `total_value_usd` at any of these sites is what made the desktop print a
/// sum across currencies that the browser refuses to show.
pub(super) fn overview_story_card_data(overview: &AnalyticsOverview, lang: Lang) -> [StoryCard; 4] {
    let rows_per_decl = safe_ratio(overview.row_count as f64, overview.declaration_count as f64);
    let value_per_decl = value_per_document(&overview.measures, overview.declaration_count);
    let net_per_decl = safe_ratio(overview.total_net_kg, overview.declaration_count as f64);
    let country_total = overview
        .distinct_origin_countries
        .max(overview.distinct_dispatch_countries)
        .max(overview.distinct_trade_countries);
    [
        (
            overview_scale_title(lang),
            fmt_money_compact(&overview.measures, lang),
            overview_weight_line(overview, lang),
            format!(
                "{}: {}",
                tr(lang).avg_value_kg,
                fmt_money_per_kg(&overview.measures, lang)
            ),
            overview_scale_help(lang),
        ),
        (
            overview_documents_title(lang),
            group_digits(overview.declaration_count),
            overview_rows_line(overview.row_count, lang),
            overview_declaration_density_line(rows_per_decl, value_per_decl, lang),
            overview_documents_help(lang),
        ),
        (
            overview_participants_title(lang),
            group_digits(overview.distinct_edrpou),
            overview_participants_line(overview, lang),
            overview_net_per_declaration_line(net_per_decl, lang),
            overview_participants_help(lang),
        ),
        (
            overview_goods_title(lang),
            group_digits(overview.distinct_product_codes),
            overview_trademarks_line(overview.distinct_trademarks, lang),
            overview_countries_line(country_total, lang),
            overview_goods_help(lang),
        ),
    ]
}

pub(super) fn overview_story_cards(ui: &mut egui::Ui, overview: &AnalyticsOverview, lang: Lang) {
    let cards = overview_story_card_data(overview, lang);

    let gap = 10.0;
    let avail = ui.available_width();
    let per_row = if avail >= 1040.0 {
        4
    } else if avail >= 660.0 {
        2
    } else {
        1
    };
    let card_w = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32).max(250.0);
    for chunk in cards.chunks(per_row) {
        ui.horizontal(|ui| {
            for (idx, (title, value, line1, line2, help)) in chunk.iter().enumerate() {
                if idx > 0 {
                    ui.add_space(gap);
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(card_w, 98.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| overview_story_card(ui, title, value, line1, line2, help),
                );
            }
        });
        ui.add_space(8.0);
    }
}

fn overview_story_card(
    ui: &mut egui::Ui,
    title: &str,
    value: &str,
    line1: &str,
    line2: &str,
    help: &str,
) {
    let response = egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(title).weak().small());
            ui.add_space(3.0);
            ui.label(egui::RichText::new(value).strong().monospace().size(20.0));
            ui.add_space(3.0);
            ui.label(egui::RichText::new(line1).small());
            ui.label(egui::RichText::new(line2).weak().small());
        })
        .response;
    let strip = egui::Rect::from_min_max(
        response.rect.left_top() + egui::vec2(1.0, 7.0),
        egui::pos2(response.rect.left() + 4.0, response.rect.bottom() - 7.0),
    );
    ui.painter().rect_filled(
        strip,
        egui::CornerRadius::same(3),
        ACCENT.gamma_multiply(0.85),
    );
    response.on_hover_text(help);
}

fn safe_ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

fn overview_weight_line(overview: &AnalyticsOverview, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!(
            "{} кг нетто | {} кг брутто",
            fmt_compact(overview.total_net_kg),
            fmt_compact(overview.total_gross_kg)
        ),
        _ => format!(
            "{} kg net | {} kg gross",
            fmt_compact(overview.total_net_kg),
            fmt_compact(overview.total_gross_kg)
        ),
    }
}

fn overview_rows_line(rows: u64, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!("{} товарних рядків", group_digits(rows)),
        _ => format!("{} goods rows", group_digits(rows)),
    }
}

fn overview_declaration_density_line(
    rows_per_decl: f64,
    value_per_decl: Option<f64>,
    lang: Lang,
) -> String {
    // `None` means the rows span several currencies, so the value half of this
    // line is dropped rather than filled with a sum of unlike money.
    let value = match value_per_decl {
        Some(value) => fmt_compact(value),
        None => tr(lang).mixed_currencies.to_string(),
    };
    match lang {
        Lang::Ua => format!(
            "{}: {} рядків | {} сума",
            overview_per_declaration_label(lang),
            fmt_decimal(rows_per_decl, 1),
            value
        ),
        _ => format!(
            "{}: {} rows | {} value",
            overview_per_declaration_label(lang),
            fmt_decimal(rows_per_decl, 1),
            value
        ),
    }
}

fn overview_participants_line(overview: &AnalyticsOverview, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!(
            "{} одержувачів | {} відправників",
            group_digits(overview.distinct_recipients),
            group_digits(overview.distinct_senders)
        ),
        _ => format!(
            "{} recipients | {} senders",
            group_digits(overview.distinct_recipients),
            group_digits(overview.distinct_senders)
        ),
    }
}

fn overview_net_per_declaration_line(net_per_decl: f64, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!("Нетто на декларацію: {} кг", fmt_compact(net_per_decl)),
        _ => format!("Net per declaration: {} kg", fmt_compact(net_per_decl)),
    }
}

fn overview_trademarks_line(trademarks: u64, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!("{} торгових марок", group_digits(trademarks)),
        _ => format!("{} trademarks", group_digits(trademarks)),
    }
}

fn overview_countries_line(countries: u64, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!("{} країн", group_digits(countries)),
        _ => format!("{} countries", group_digits(countries)),
    }
}

fn overview_scale_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Масштаб запиту",
        _ => "Query scale",
    }
}

fn overview_scale_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Головна відповідь про обсяг: сума, вага та середня вартість за кг для поточного запиту."
        }
        _ => "Headline scale: value, weight, and average value per kg for the current query.",
    }
}

fn overview_documents_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Документи",
        _ => "Documents",
    }
}

fn overview_documents_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Пояснює базу розрахунку: скільки декларацій і товарних рядків потрапили в аналітику."
        }
        _ => "Shows the calculation base: recognized documents and rows included in analytics.",
    }
}

fn overview_participants_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Учасники",
        _ => "Participants",
    }
}

fn overview_participants_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Скільки різних компаній, одержувачів і відправників знайдено у поточному запиті."
        }
        _ => "How many recognized companies and company roles appear in the current query.",
    }
}

fn overview_goods_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Товари і географія",
        _ => "Goods and geography",
    }
}

fn overview_goods_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Ширина товарної групи: коди товару, торгові марки та країни, які зустрічаються у запиті."
        }
        _ => {
            "Breadth of the product set: product/SKU codes, brands, and countries present in the query."
        }
    }
}

fn overview_per_declaration_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "На декларацію",
        _ => "Per document",
    }
}

pub(super) fn overview_senders_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Унікальні відправники у знайдених рядках.",
        _ => "Unique recognized source companies in the matched rows.",
    }
}

pub(super) fn overview_edrpou_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Унікальні коди ЄДРПОУ у знайдених рядках.",
        _ => "Unique recognized company identifiers in the matched rows.",
    }
}

pub(super) fn overview_gross_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Сумарна вага брутто у знайдених рядках.",
        _ => "Total gross weight across the matched rows.",
    }
}

pub(super) fn overview_quantity_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Сума поля кількості там, де воно заповнене числом.",
        _ => "Sum of the quantity field where it can be parsed as a number.",
    }
}

pub(super) fn overview_trademarks_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Кількість різних торгових марок у знайдених рядках.",
        _ => "Number of distinct trademarks in the matched rows.",
    }
}

pub(super) fn overview_origin_countries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Країни походження",
        _ => "Origin countries",
    }
}

pub(super) fn overview_dispatch_countries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Країни відправлення",
        _ => "Dispatch countries",
    }
}

pub(super) fn overview_dispatch_countries_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Кількість різних країн відправлення у знайдених рядках.",
        _ => "Number of distinct dispatch countries in the matched rows.",
    }
}

pub(super) fn overview_trade_countries_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Країни торгівлі",
        _ => "Trade countries",
    }
}

pub(super) fn overview_trade_countries_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Кількість різних країн торгівлі у знайдених рядках.",
        _ => "Number of distinct trade countries in the matched rows.",
    }
}

#[cfg(test)]
mod tests {
    use super::overview_story_card_data;
    use crate::db::{AnalyticsCurrencyTotal, AnalyticsMeasures, AnalyticsOverview};
    use crate::i18n::Lang;

    fn overview(totals: Vec<AnalyticsCurrencyTotal>, raw_sum: f64) -> AnalyticsOverview {
        AnalyticsOverview {
            row_count: 2,
            declaration_count: 2,
            // The scalar the cards used to read. It stays populated on purpose:
            // the point of the test is that nothing renders it.
            total_value_usd: raw_sum,
            measures: AnalyticsMeasures {
                currency_totals: totals,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn bucket(currency: &str, known: bool, total: f64) -> AnalyticsCurrencyTotal {
        AnalyticsCurrencyTotal {
            currency: currency.to_string(),
            known,
            valued_rows: 1,
            total_value: total,
        }
    }

    #[test]
    fn the_scale_card_carries_the_currency_it_is_denominated_in() {
        let cards = overview_story_card_data(
            &overview(vec![bucket("USD", true, 1500.0)], 1500.0),
            Lang::En,
        );
        assert_eq!(cards[0].1, "1500 USD");
    }

    #[test]
    fn the_scale_card_refuses_to_add_two_currencies() {
        let mixed = overview(
            vec![bucket("USD", true, 1000.0), bucket("EUR", true, 500.0)],
            1500.0,
        );
        let cards = overview_story_card_data(&mixed, Lang::En);
        assert_eq!(cards[0].1, "Several currencies");
        // The per-document line on the documents card is money too.
        assert!(
            cards[1].3.contains("Several currencies"),
            "value per document must not be an average across currencies: {}",
            cards[1].3
        );
        for card in &cards {
            assert!(
                !card.1.contains("1500") && !card.3.contains("1500"),
                "the cross-currency sum reached a card: {card:?}"
            );
        }
    }
}
