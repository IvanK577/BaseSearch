use super::format::{
    fmt_compact, fmt_decimal, fmt_money_compact, fmt_money_exact, fmt_money_per_kg,
};
use super::ui_text::query_summary;
use crate::db::{Analytics, Query};
use crate::i18n::{Lang, group_digits, tr};

pub(super) fn analytics_compare_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Порівняння",
        _ => "Compare",
    }
}

pub(super) fn compare_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Порівняйте поточний запит з іншим товаром, компанією або роком. Фільтри зліва зберігаються, якщо не змінити текст чи рік."
        }
        _ => {
            "Compare the current query with another product, company, or year. Current filters are reused unless you override text or year."
        }
    }
}

pub(super) fn compare_text_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Порівняти з:",
        _ => "Compare with:",
    }
}

pub(super) fn compare_previous_year_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Попередній рік",
        _ => "Previous year",
    }
}

pub(super) fn compare_run_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Порівняти",
        _ => "Compare",
    }
}

pub(super) fn compare_empty(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Вкажіть текст або рік для порівняння і натисніть «Порівняти».",
        _ => "Enter a text or year to compare with, then click Compare.",
    }
}

pub(super) fn compare_ui(
    ui: &mut egui::Ui,
    left: &Analytics,
    right: &Analytics,
    left_query: &Query,
    right_query: &Query,
    lang: Lang,
) {
    ui.columns(2, |cols| {
        compare_side_card(
            &mut cols[0],
            query_summary(left_query, tr(lang)),
            left,
            lang,
        );
        compare_side_card(
            &mut cols[1],
            query_summary(right_query, tr(lang)),
            right,
            lang,
        );
    });
    ui.add_space(10.0);
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(compare_delta_title(lang)).strong());
            ui.add_space(4.0);
            egui::Grid::new("compare_delta_grid")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    compare_metric_row(
                        ui,
                        tr(lang).rows_label,
                        left.overview.row_count as f64,
                        right.overview.row_count as f64,
                        0,
                    );
                    compare_metric_row(
                        ui,
                        tr(lang).declarations_label,
                        left.overview.declaration_count as f64,
                        right.overview.declaration_count as f64,
                        0,
                    );
                    let (lm, rm) = (&left.overview.measures, &right.overview.measures);
                    compare_money_row(
                        ui,
                        tr(lang).total_value,
                        (&fmt_money_exact(lm, lang), lm.single_currency_total()),
                        (&fmt_money_exact(rm, lang), rm.single_currency_total()),
                        lang,
                    );
                    compare_metric_row(
                        ui,
                        tr(lang).net_weight,
                        left.overview.total_net_kg,
                        right.overview.total_net_kg,
                        2,
                    );
                    compare_money_row(
                        ui,
                        tr(lang).avg_value_kg,
                        (&fmt_money_per_kg(lm, lang), lm.single_currency_per_net_kg()),
                        (&fmt_money_per_kg(rm, lang), rm.single_currency_per_net_kg()),
                        lang,
                    );
                    compare_metric_row(
                        ui,
                        tr(lang).unique_edrpou,
                        left.overview.distinct_edrpou as f64,
                        right.overview.distinct_edrpou as f64,
                        0,
                    );
                });
        });
}

fn compare_side_card(ui: &mut egui::Ui, title: String, analytics: &Analytics, lang: Lang) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).strong());
            ui.add_space(4.0);
            ui.label(format!(
                "{}: {}",
                tr(lang).rows_label,
                group_digits(analytics.overview.row_count)
            ));
            ui.label(format!(
                "{}: {}",
                tr(lang).total_value,
                fmt_money_compact(&analytics.overview.measures, lang)
            ));
            ui.label(format!(
                "{}: {} kg",
                tr(lang).net_weight,
                fmt_compact(analytics.overview.total_net_kg)
            ));
            ui.label(format!(
                "{}: {}",
                tr(lang).avg_value_kg,
                fmt_money_per_kg(&analytics.overview.measures, lang)
            ));
        });
}

fn compare_delta_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Різниця",
        _ => "Difference",
    }
}

fn compare_metric_row(ui: &mut egui::Ui, label: &str, left: f64, right: f64, decimals: usize) {
    ui.label(label);
    ui.label(egui::RichText::new(format_metric(left, decimals)).monospace());
    ui.label(egui::RichText::new(format_metric(right, decimals)).monospace());
    let delta = right - left;
    let pct = if left.abs() > f64::EPSILON {
        delta / left * 100.0
    } else {
        0.0
    };
    let text = if left.abs() > f64::EPSILON {
        format!("{} ({:+.1}%)", format_metric(delta, decimals), pct)
    } else {
        format_metric(delta, decimals)
    };
    ui.label(egui::RichText::new(text).monospace().strong());
    ui.end_row();
}

/// A money row of the difference grid.
///
/// Two totals can be subtracted only when both sides are a single, identical
/// currency. Otherwise the two figures are still shown — that is the point of
/// the comparison — but the difference column says the currencies do not line
/// up rather than printing a subtraction that stands for nothing.
fn compare_money_row(
    ui: &mut egui::Ui,
    label: &str,
    left: (&str, Option<(f64, &str)>),
    right: (&str, Option<(f64, &str)>),
    lang: Lang,
) {
    ui.label(label);
    ui.label(egui::RichText::new(left.0).monospace());
    ui.label(egui::RichText::new(right.0).monospace());
    ui.label(
        egui::RichText::new(money_difference(left.1, right.1, lang))
            .monospace()
            .strong(),
    );
    ui.end_row();
}

fn money_difference(left: Option<(f64, &str)>, right: Option<(f64, &str)>, lang: Lang) -> String {
    let (Some((before, left_code)), Some((after, right_code))) = (left, right) else {
        return tr(lang).mixed_currencies.to_string();
    };
    if left_code != right_code {
        return tr(lang).mixed_currencies.to_string();
    }
    let delta = after - before;
    let shown = match left_code {
        "" => fmt_decimal(delta, 2),
        code => format!("{} {code}", fmt_decimal(delta, 2)),
    };
    if before.abs() > f64::EPSILON {
        format!("{shown} ({:+.1}%)", delta / before * 100.0)
    } else {
        shown
    }
}

fn format_metric(value: f64, decimals: usize) -> String {
    if decimals == 0 {
        let rounded = value.round();
        if rounded < 0.0 {
            format!("-{}", group_digits((-rounded) as u64))
        } else {
            group_digits(rounded as u64)
        }
    } else {
        fmt_decimal(value, decimals)
    }
}

#[cfg(test)]
mod tests {
    use super::money_difference;
    use crate::i18n::Lang;

    #[test]
    fn two_totals_in_the_same_currency_subtract() {
        assert_eq!(
            money_difference(Some((1000.0, "USD")), Some((1250.0, "USD")), Lang::En),
            "250 USD (+25.0%)"
        );
    }

    /// The whole point of the comparison is the difference column, so it is
    /// exactly where a subtraction across currencies would do the most damage:
    /// a number with a percentage next to it reads as a finding.
    #[test]
    fn a_dollar_total_is_never_subtracted_from_a_euro_one() {
        assert_eq!(
            money_difference(Some((1000.0, "USD")), Some((1250.0, "EUR")), Lang::En),
            "Several currencies"
        );
    }

    #[test]
    fn a_side_that_spans_currencies_has_no_difference_to_report() {
        assert_eq!(
            money_difference(None, Some((1250.0, "EUR")), Lang::En),
            "Several currencies"
        );
        assert_eq!(
            money_difference(Some((1000.0, "USD")), None, Lang::En),
            "Several currencies"
        );
    }

    /// Both sides unlabelled is the ordinary case for a source that never
    /// stated a currency, and it has always subtracted. It still does.
    #[test]
    fn two_unlabelled_totals_still_subtract() {
        assert_eq!(
            money_difference(Some((100.0, "")), Some((80.0, "")), Lang::En),
            "-20 (-20.0%)"
        );
    }
}
