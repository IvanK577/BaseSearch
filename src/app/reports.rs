use super::analytics_groups::section_title;
use super::format::{
    currency_breakdown, fmt_compact, fmt_decimal, fmt_money_compact, fmt_money_exact,
    fmt_money_per_kg,
};
use super::month_chart::{MonthMetric, months_chart};
use super::price_view::price_metric_title;
use super::ui_text::{query_summary, trunc_label};
use super::widgets::kpi_tile;
use crate::db::{Analytics, AnalyticsGroupRow, AnalyticsSection, Query};
use crate::i18n::{Lang, group_digits, tr};

pub(super) fn analytics_report_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Звіт",
        _ => "Report",
    }
}

pub(super) fn report_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Звіт по поточному запиту",
        _ => "Report for the current query",
    }
}

pub(super) fn report_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Короткий підсумок для роботи: головні цифри, компанії, товари, країни і ціни. HTML-звіт можна зберегти як PDF через друк у браузері."
        }
        _ => {
            "A clean working summary: headline numbers, companies, goods, countries, and prices. The HTML report can be saved as PDF from the system print dialog."
        }
    }
}

pub(super) fn report_copy_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Копіювати звіт",
        _ => "Copy report",
    }
}

pub(super) fn report_export_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Експорт HTML/PDF",
        _ => "Export HTML/PDF",
    }
}

pub(super) fn report_markdown(analytics: &Analytics, query: &Query, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str("# Base Search Report\n\n");
    out.push_str(&format!("Query: {}\n\n", query_summary(query, tr(lang))));
    out.push_str("## Summary\n");
    out.push_str(&format!(
        "- Rows: {}\n",
        group_digits(analytics.overview.row_count)
    ));
    out.push_str(&format!(
        "- Declarations: {}\n",
        group_digits(analytics.overview.declaration_count)
    ));
    out.push_str(&format!(
        "- Total value: {}\n",
        fmt_money_exact(&analytics.overview.measures, lang)
    ));
    // A report is read away from the program, so "several currencies" with
    // nothing beside it cannot be resolved by hovering anything.
    if let Some(breakdown) = currency_breakdown(&analytics.overview.measures, lang) {
        out.push_str(&format!("- {}: {breakdown}\n", tr(lang).currency_breakdown));
    }
    out.push_str(&format!(
        "- Net weight: {:.3} kg\n",
        analytics.overview.total_net_kg
    ));
    out.push_str(&format!(
        "- Average value/kg: {}\n\n",
        fmt_money_per_kg(&analytics.overview.measures, lang)
    ));
    append_report_sections(&mut out, "Companies", &analytics.company_sections, lang);
    append_report_sections(&mut out, "Goods", &analytics.product_sections, lang);
    append_report_sections(&mut out, "Countries", &analytics.country_sections, lang);
    out
}

pub(super) fn report_html(analytics: &Analytics, query: &Query, lang: Lang) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<h1>Base Search Report</h1><p class=\"query\">{}</p>",
        esc_html(&query_summary(query, tr(lang)))
    ));
    body.push_str("<section class=\"kpis\">");
    for (label, value) in [
        (
            tr(lang).rows_label,
            group_digits(analytics.overview.row_count),
        ),
        (
            tr(lang).declarations_label,
            group_digits(analytics.overview.declaration_count),
        ),
        (
            tr(lang).total_value,
            fmt_money_exact(&analytics.overview.measures, lang),
        ),
        (
            tr(lang).net_weight,
            format!("{:.3} kg", analytics.overview.total_net_kg),
        ),
        (
            tr(lang).avg_value_kg,
            fmt_money_per_kg(&analytics.overview.measures, lang),
        ),
        (
            tr(lang).unique_edrpou,
            group_digits(analytics.overview.distinct_edrpou),
        ),
    ] {
        body.push_str(&format!(
            "<article><span>{}</span><strong>{}</strong></article>",
            esc_html(label),
            esc_html(&value)
        ));
    }
    if let Some(breakdown) = currency_breakdown(&analytics.overview.measures, lang) {
        body.push_str(&format!(
            "<article><span>{}</span><strong>{}</strong></article>",
            esc_html(tr(lang).currency_breakdown),
            esc_html(&breakdown)
        ));
    }
    body.push_str("</section>");
    append_html_sections(
        &mut body,
        tr(lang).companies_section,
        &analytics.company_sections,
        lang,
    );
    append_html_sections(
        &mut body,
        tr(lang).products_section,
        &analytics.product_sections,
        lang,
    );
    append_html_sections(
        &mut body,
        tr(lang).countries_section,
        &analytics.country_sections,
        lang,
    );
    append_html_prices(&mut body, analytics, lang);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Base Search Report</title>{}</head><body>{}</body></html>",
        report_css(),
        body
    )
}

pub(super) fn report_ui(ui: &mut egui::Ui, analytics: &Analytics, query: &Query, lang: Lang) {
    ui.label(
        egui::RichText::new(query_summary(query, tr(lang)))
            .weak()
            .small(),
    );
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        kpi_tile(
            ui,
            tr(lang).rows_label,
            group_digits(analytics.overview.row_count),
            tr(lang).rows_help,
        );
        kpi_tile(
            ui,
            tr(lang).declarations_label,
            group_digits(analytics.overview.declaration_count),
            tr(lang).declarations_help,
        );
        kpi_tile(
            ui,
            tr(lang).total_value,
            fmt_money_compact(&analytics.overview.measures, lang),
            tr(lang).total_value_help,
        );
        kpi_tile(
            ui,
            tr(lang).net_weight,
            format!("{} kg", fmt_compact(analytics.overview.total_net_kg)),
            tr(lang).net_weight_help,
        );
        kpi_tile(
            ui,
            tr(lang).avg_value_kg,
            fmt_money_per_kg(&analytics.overview.measures, lang),
            tr(lang).avg_value_kg_help,
        );
        kpi_tile(
            ui,
            tr(lang).unique_edrpou,
            group_digits(analytics.overview.distinct_edrpou),
            tr(lang).unique_edrpou,
        );
    });
    ui.add_space(12.0);

    if !analytics.months.is_empty() {
        ui.label(egui::RichText::new(tr(lang).months_section).strong());
        months_chart(ui, &analytics.months, MonthMetric::Value, lang);
        ui.add_space(12.0);
    }

    ui.columns(2, |cols| {
        report_section(
            &mut cols[0],
            tr(lang).companies_section,
            &analytics.company_sections,
            lang,
        );
        report_section(
            &mut cols[1],
            tr(lang).products_section,
            &analytics.product_sections,
            lang,
        );
    });
    ui.add_space(10.0);
    ui.columns(2, |cols| {
        report_section(
            &mut cols[0],
            tr(lang).countries_section,
            &analytics.country_sections,
            lang,
        );
        report_prices(&mut cols[1], analytics, lang);
    });
}

fn report_section(ui: &mut egui::Ui, title: &str, sections: &[AnalyticsSection], lang: Lang) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).strong());
            ui.add_space(4.0);
            for section in sections.iter().filter(|s| !s.rows.is_empty()).take(2) {
                ui.label(
                    egui::RichText::new(section_title(section.kind, lang))
                        .weak()
                        .small(),
                );
                for row in section.rows.iter().take(5) {
                    report_group_row(ui, row, lang);
                }
                ui.add_space(4.0);
            }
        });
}

fn report_group_row(ui: &mut egui::Ui, row: &AnalyticsGroupRow, lang: Lang) {
    ui.horizontal(|ui| {
        ui.label(trunc_label(&row.label, 38));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {}%",
                    fmt_money_compact(&row.measures, lang),
                    fmt_decimal(row.share_percent, 1)
                ))
                .monospace(),
            );
        });
    });
}

fn report_prices(ui: &mut egui::Ui, analytics: &Analytics, lang: Lang) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(tr(lang).prices_section).strong());
            ui.add_space(4.0);
            for metric in analytics
                .price_sections
                .iter()
                .filter(|m| m.count > 0)
                .take(5)
            {
                ui.horizontal(|ui| {
                    ui.label(price_metric_title(metric.kind, lang));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}: {}",
                                tr(lang).median,
                                fmt_decimal(metric.median, 2)
                            ))
                            .monospace(),
                        );
                    });
                });
            }
        });
}

fn append_html_sections(out: &mut String, title: &str, sections: &[AnalyticsSection], lang: Lang) {
    out.push_str(&format!("<section><h2>{}</h2>", esc_html(title)));
    for section in sections.iter().filter(|s| !s.rows.is_empty()).take(3) {
        out.push_str(&format!(
            "<h3>{}</h3><table><thead><tr><th>Name</th><th>Value</th><th>Net kg</th><th>Rows</th><th>Share</th></tr></thead><tbody>",
            esc_html(section_title(section.kind, lang))
        ));
        for row in section.rows.iter().take(10) {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{:.3}</td><td>{}</td><td>{:.1}%</td></tr>",
                esc_html(&row.label),
                esc_html(&fmt_money_exact(&row.measures, lang)),
                row.total_net_kg,
                row.rows,
                row.share_percent
            ));
        }
        out.push_str("</tbody></table>");
    }
    out.push_str("</section>");
}

fn append_html_prices(out: &mut String, analytics: &Analytics, lang: Lang) {
    out.push_str(&format!(
        "<section><h2>{}</h2><table><thead><tr><th>Metric</th><th>Average</th><th>Weighted</th><th>Median</th><th>P25-P75</th><th>Rows</th></tr></thead><tbody>",
        esc_html(tr(lang).prices_section)
    ));
    for metric in analytics
        .price_sections
        .iter()
        .filter(|m| m.count > 0)
        .take(8)
    {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3} - {:.3}</td><td>{}</td></tr>",
            esc_html(price_metric_title(metric.kind, lang)),
            metric.average,
            metric.weighted_average,
            metric.median,
            metric.p25,
            metric.p75,
            metric.count
        ));
    }
    out.push_str("</tbody></table></section>");
}

fn report_css() -> &'static str {
    "<style>
      :root { color-scheme: light; font-family: Segoe UI, Arial, sans-serif; color: #1b2430; }
      body { margin: 36px; background: #fff; font-size: 13px; line-height: 1.45; }
      h1 { margin: 0 0 4px; font-size: 26px; }
      h2 { margin: 26px 0 8px; font-size: 18px; border-bottom: 1px solid #d7dde5; padding-bottom: 4px; }
      h3 { margin: 16px 0 6px; font-size: 14px; color: #34404e; }
      .query { margin: 0 0 18px; color: #6a7682; }
      .kpis { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; margin: 18px 0 20px; }
      .kpis article { border: 1px solid #d7dde5; border-radius: 6px; padding: 10px 12px; }
      .kpis span { display: block; color: #6a7682; font-size: 11px; }
      .kpis strong { display: block; margin-top: 4px; font-size: 18px; font-family: Consolas, monospace; }
      table { width: 100%; border-collapse: collapse; margin-bottom: 8px; }
      th, td { border-bottom: 1px solid #e4e8ee; padding: 6px 7px; text-align: left; vertical-align: top; }
      th { background: #f3f6f9; color: #34404e; font-size: 11px; text-transform: uppercase; }
      td:not(:first-child), th:not(:first-child) { text-align: right; font-family: Consolas, monospace; }
      @media print { body { margin: 18mm; } .kpis { grid-template-columns: repeat(3, 1fr); } h2 { break-after: avoid; } table { break-inside: avoid; } }
    </style>"
}

fn esc_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn append_report_sections(
    out: &mut String,
    title: &str,
    sections: &[AnalyticsSection],
    lang: Lang,
) {
    out.push_str(&format!("## {title}\n"));
    for section in sections.iter().filter(|s| !s.rows.is_empty()).take(3) {
        out.push_str(&format!("### {:?}\n", section.kind));
        for row in section.rows.iter().take(10) {
            out.push_str(&format!(
                "- {}: value {}, net {:.3} kg, rows {}, share {:.1}%\n",
                row.label,
                fmt_money_exact(&row.measures, lang),
                row.total_net_kg,
                row.rows,
                row.share_percent
            ));
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{report_html, report_markdown};
    use crate::db::{
        Analytics, AnalyticsCurrencyTotal, AnalyticsGroupRow, AnalyticsMeasures, AnalyticsOverview,
        AnalyticsSection, AnalyticsSectionKind, Query,
    };
    use crate::i18n::Lang;

    fn measures(buckets: Vec<(&str, f64)>) -> AnalyticsMeasures {
        AnalyticsMeasures {
            currency_totals: buckets
                .iter()
                .map(|(currency, total)| AnalyticsCurrencyTotal {
                    currency: (*currency).to_string(),
                    known: true,
                    valued_rows: 1,
                    total_value: *total,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// One company in dollars, one in euros, and a workspace total that is
    /// neither. The exported report is the artefact that leaves the program and
    /// gets forwarded, so an unlabelled number in it is the hardest to catch.
    fn mixed_analytics() -> Analytics {
        Analytics {
            overview: AnalyticsOverview {
                row_count: 4,
                total_value_usd: 1500.0,
                measures: measures(vec![("USD", 1000.0), ("EUR", 500.0)]),
                ..Default::default()
            },
            company_sections: vec![AnalyticsSection {
                kind: AnalyticsSectionKind::Senders,
                rows: vec![
                    AnalyticsGroupRow {
                        label: "SHENZHEN TECH".to_string(),
                        rows: 2,
                        total_value_usd: 1000.0,
                        measures: measures(vec![("USD", 1000.0)]),
                        ..Default::default()
                    },
                    AnalyticsGroupRow {
                        label: "HAMBURG HANDEL".to_string(),
                        rows: 2,
                        total_value_usd: 500.0,
                        measures: measures(vec![("EUR", 500.0)]),
                        ..Default::default()
                    },
                ],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn the_markdown_report_labels_every_company_with_its_own_currency() {
        let report = report_markdown(&mixed_analytics(), &Query::default(), Lang::En);
        assert!(report.contains("1\u{202F}000 USD"), "{report}");
        assert!(report.contains("500 EUR"), "{report}");
        assert!(
            report.contains("Total value: Several currencies"),
            "the workspace total spans two and must say so: {report}"
        );
        assert!(
            report.contains("By currency: 1000 USD  ·  500 EUR"),
            "and then say which two, since the reader cannot hover a file: {report}"
        );
        assert!(
            !report.contains("1500") && !report.contains("1\u{202F}500"),
            "the cross-currency sum reached the report: {report}"
        );
    }

    #[test]
    fn the_html_report_does_the_same() {
        let report = report_html(&mixed_analytics(), &Query::default(), Lang::En);
        assert!(report.contains("1\u{202F}000 USD"), "{report}");
        assert!(report.contains("500 EUR"), "{report}");
        assert!(
            report.contains("1000 USD  ·  500 EUR"),
            "the breakdown belongs in the exported file too: {report}"
        );
        assert!(
            !report.contains(">1500<") && !report.contains("1500.00"),
            "the cross-currency sum reached the report: {report}"
        );
    }
}
