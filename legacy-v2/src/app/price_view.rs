use super::format::fmt_decimal;
use super::ui_text::{
    price_average_help, price_count_help, price_median_help, price_range_help, price_weighted_help,
};
use crate::db::{AnalyticsPriceMetric, PriceMetricKind};
use crate::i18n::{Lang, group_digits, tr};

pub(super) fn price_table(ui: &mut egui::Ui, metrics: &[AnalyticsPriceMetric], lang: Lang) {
    egui::Grid::new("analytics_price_metrics")
        .num_columns(6)
        .striped(true)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new(price_header_metric(lang)).weak());
            ui.label(egui::RichText::new(price_header_avg(lang)).weak())
                .on_hover_text(price_average_help(lang));
            ui.label(egui::RichText::new(price_header_weighted(lang)).weak())
                .on_hover_text(price_weighted_help(lang));
            ui.label(egui::RichText::new(price_header_median(lang)).weak())
                .on_hover_text(price_median_help(lang));
            ui.label(egui::RichText::new("P25\u{2013}P75").weak())
                .on_hover_text(price_range_help(lang));
            ui.label(egui::RichText::new(price_header_count(lang)).weak())
                .on_hover_text(price_count_help(lang));
            ui.end_row();
            for metric in metrics {
                if metric.count == 0 {
                    continue;
                }
                ui.label(price_metric_title(metric.kind, lang));
                ui.label(egui::RichText::new(fmt_decimal(metric.average, 3)).monospace());
                ui.label(egui::RichText::new(fmt_decimal(metric.weighted_average, 3)).monospace());
                ui.label(egui::RichText::new(fmt_decimal(metric.median, 3)).monospace());
                ui.label(
                    egui::RichText::new(format!(
                        "{} \u{2013} {}",
                        fmt_decimal(metric.p25, 3),
                        fmt_decimal(metric.p75, 3)
                    ))
                    .monospace(),
                );
                ui.label(egui::RichText::new(group_digits(metric.count)).monospace());
                ui.end_row();
            }
        });
}

fn price_header_median(lang: Lang) -> &'static str {
    tr(lang).median
}

fn price_header_weighted(lang: Lang) -> &'static str {
    tr(lang).weighted_avg
}

pub(super) fn price_metric_title(kind: PriceMetricKind, lang: Lang) -> &'static str {
    let t = tr(lang);
    match kind {
        PriceMetricKind::ValuePerNetKg => t.pm_value_per_net_kg,
        PriceMetricKind::RfvUsdKg => t.pm_rfv,
        PriceMetricKind::RmvNetUsdKg => t.pm_rmv_net,
        PriceMetricKind::RmvUsdExtraUnit => t.pm_rmv_extra_unit,
        PriceMetricKind::RmvGrossUsdKg => t.pm_rmv_gross,
        PriceMetricKind::MinBaseUsdKg => t.pm_min_base,
    }
}

fn price_header_metric(lang: Lang) -> &'static str {
    tr(lang).price_header_metric
}

fn price_header_avg(lang: Lang) -> &'static str {
    tr(lang).price_header_avg
}

fn price_header_count(lang: Lang) -> &'static str {
    tr(lang).price_header_count
}
