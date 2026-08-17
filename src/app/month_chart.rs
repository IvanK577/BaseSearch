use crate::db::AnalyticsMonthRow;
use crate::i18n::{Lang, group_digits, tr};

use super::ACCENT;
use super::format::{fmt_compact, fmt_decimal, fmt_money_exact, fmt_money_per_kg, short_month};

/// Metric displayed in the monthly dynamics chart.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MonthMetric {
    #[default]
    Value,
    Rows,
    NetWeight,
    /// Monthly average price: value / net weight.
    AvgPrice,
}

impl MonthMetric {
    fn of(self, row: &AnalyticsMonthRow) -> f64 {
        match self {
            MonthMetric::Value => row.total_value_usd,
            MonthMetric::Rows => row.rows as f64,
            MonthMetric::NetWeight => row.total_net_kg,
            MonthMetric::AvgPrice => {
                if row.total_net_kg > 0.0 {
                    row.total_value_usd / row.total_net_kg
                } else {
                    0.0
                }
            }
        }
    }

    fn index(self) -> u8 {
        match self {
            MonthMetric::Value => 0,
            MonthMetric::Rows => 1,
            MonthMetric::NetWeight => 2,
            MonthMetric::AvgPrice => 3,
        }
    }
}

/// Bar chart of monthly dynamics. Bars are drawn with the painter;
/// hovering a bar shows the full numbers for that month.
pub(super) fn months_chart(
    ui: &mut egui::Ui,
    months: &[AnalyticsMonthRow],
    metric: MonthMetric,
    lang: Lang,
) {
    let height = 190.0;
    let width = ui.available_width().max(320.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let visuals = ui.visuals();
    let rounding = egui::CornerRadius::same(5);
    ui.painter().rect(
        rect,
        rounding,
        visuals.faint_bg_color,
        visuals.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );

    let max_value = months
        .iter()
        .map(|month| metric.of(month))
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    let label_h = 18.0;
    let pad = 10.0;
    let plot = egui::Rect::from_min_max(
        egui::pos2(rect.left() + pad, rect.top() + pad),
        egui::pos2(rect.right() - pad, rect.bottom() - pad - label_h),
    );

    let grid_font = egui::FontId::new(10.5, egui::FontFamily::Monospace);
    let grid_color = visuals.weak_text_color().gamma_multiply(0.5);
    for step in 1..=3 {
        let frac = step as f32 / 4.0;
        let y = plot.bottom() - plot.height() * frac;
        ui.painter().hline(
            plot.x_range(),
            y,
            egui::Stroke::new(0.5_f32, grid_color.gamma_multiply(0.6)),
        );
        ui.painter().text(
            egui::pos2(plot.left(), y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            fmt_compact(max_value * frac as f64),
            grid_font.clone(),
            grid_color,
        );
    }

    let n = months.len().max(1);
    let slot = plot.width() / n as f32;
    let bar_w = (slot * 0.72).clamp(3.0, 64.0);
    let hover_x = response.hover_pos().map(|pos| pos.x);
    let mut hovered: Option<usize> = None;
    let mut hovered_bar: Option<egui::Rect> = None;

    let bar_color = if visuals.dark_mode {
        egui::Color32::from_rgb(80, 140, 255)
    } else {
        ACCENT
    };
    let month_font = egui::FontId::new(10.5, egui::FontFamily::Monospace);
    let value_font = egui::FontId::new(10.5, egui::FontFamily::Monospace);
    let label_every = ((42.0 / slot).ceil() as usize).max(1);

    for (i, month) in months.iter().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        let value = metric.of(month);
        let is_hovered = hover_x
            .map(|x| (x - cx).abs() <= slot / 2.0)
            .unwrap_or(false);
        let hover_t = ui.ctx().animate_bool_with_time(
            egui::Id::new(("month_chart_bar", i, metric.index())),
            is_hovered,
            0.12,
        );
        let bar_h = (plot.height() * (value / max_value) as f32 * (1.0 + hover_t * 0.035))
            .max(1.5)
            .min(plot.height());
        let lift = hover_t * 2.0;
        let bar = egui::Rect::from_min_max(
            egui::pos2(cx - bar_w / 2.0, plot.bottom() - bar_h - lift),
            egui::pos2(cx + bar_w / 2.0, plot.bottom()),
        );
        if is_hovered {
            hovered = Some(i);
            hovered_bar = Some(bar);
        }
        let color = bar_color.gamma_multiply(0.58 + hover_t * 0.42);
        ui.painter().rect_filled(
            bar,
            egui::CornerRadius::same(2 + (hover_t * 2.0) as u8),
            color,
        );
        if i % label_every == 0 {
            ui.painter().text(
                egui::pos2(cx, rect.bottom() - 4.0),
                egui::Align2::CENTER_BOTTOM,
                short_month(&month.month),
                month_font.clone(),
                visuals.weak_text_color(),
            );
        }
        if slot >= 46.0 && value > 0.0 {
            ui.painter().text(
                egui::pos2(cx, bar.top() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                fmt_compact(value),
                value_font.clone(),
                visuals.weak_text_color(),
            );
        }
    }

    if let (Some(i), Some(bar)) = (hovered, hovered_bar) {
        let month = &months[i];
        draw_month_popup(ui, rect, bar, month, metric, lang);
    }
}

fn draw_month_popup(
    ui: &mut egui::Ui,
    chart_rect: egui::Rect,
    bar: egui::Rect,
    month: &AnalyticsMonthRow,
    metric: MonthMetric,
    lang: Lang,
) {
    let visuals = ui.visuals();
    let popup_w = 226.0;
    let popup_h = 112.0;
    let x = (bar.center().x - popup_w / 2.0)
        .clamp(chart_rect.left() + 8.0, chart_rect.right() - popup_w - 8.0);
    let y = (bar.top() - popup_h - 10.0).max(chart_rect.top() + 8.0);
    let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(popup_w, popup_h));
    let fill = if visuals.dark_mode {
        egui::Color32::from_rgb(32, 38, 48)
    } else {
        egui::Color32::from_rgb(255, 255, 255)
    };
    let stroke = egui::Stroke::new(
        1.0_f32,
        if visuals.dark_mode {
            egui::Color32::from_rgb(84, 112, 160)
        } else {
            egui::Color32::from_rgb(188, 203, 230)
        },
    );
    let shadow = rect.translate(egui::vec2(0.0, 2.0));
    ui.painter().rect_filled(
        shadow,
        egui::CornerRadius::same(7),
        egui::Color32::from_black_alpha(if visuals.dark_mode { 70 } else { 26 }),
    );
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(7), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(7),
        stroke,
        egui::StrokeKind::Inside,
    );
    let arrow_x = bar
        .center()
        .x
        .clamp(rect.left() + 16.0, rect.right() - 16.0);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(arrow_x - 6.0, rect.bottom() - 1.0),
            egui::pos2(arrow_x + 6.0, rect.bottom() - 1.0),
            egui::pos2(arrow_x, rect.bottom() + 7.0),
        ],
        fill,
        stroke,
    ));

    let t = tr(lang);
    let lines = [
        month.month.clone(),
        format!(
            "{}: {}",
            month_metric_label(metric, lang),
            month_metric_value(metric, month, lang)
        ),
        format!("{}: {}", t.chart_rows, group_digits(month.rows)),
        format!(
            "{}: {}",
            t.chart_declarations,
            group_digits(month.declarations)
        ),
        format!(
            "{}: {}",
            t.chart_value,
            fmt_money_exact(&month.measures, lang)
        ),
        format!(
            "{}: {} kg  |  {}: {}",
            t.chart_net_weight,
            fmt_decimal(month.total_net_kg, 0),
            t.metric_price,
            fmt_money_per_kg(&month.measures, lang)
        ),
    ];
    for (idx, line) in lines.iter().enumerate() {
        let color = if idx == 0 {
            visuals.text_color()
        } else {
            visuals.weak_text_color()
        };
        let font = if idx == 0 {
            egui::FontId::new(13.0, egui::FontFamily::Proportional)
        } else {
            egui::FontId::new(11.5, egui::FontFamily::Proportional)
        };
        ui.painter().text(
            egui::pos2(rect.left() + 10.0, rect.top() + 9.0 + idx as f32 * 16.0),
            egui::Align2::LEFT_TOP,
            line,
            font,
            color,
        );
    }
}

fn month_metric_label(metric: MonthMetric, lang: Lang) -> &'static str {
    let t = tr(lang);
    match metric {
        MonthMetric::Value => t.metric_value,
        MonthMetric::Rows => t.metric_rows,
        MonthMetric::NetWeight => t.metric_weight,
        MonthMetric::AvgPrice => t.metric_price,
    }
}

/// The hovered month's figure as text.
///
/// Bar heights stay on the raw sums — a bar needs a scalar — but the numbers a
/// person reads come from the month's own currency bucket, so a month is either
/// labelled with its currency or reported as mixed. It is never a bare number
/// standing for two currencies added together.
fn month_metric_value(metric: MonthMetric, month: &AnalyticsMonthRow, lang: Lang) -> String {
    match metric {
        MonthMetric::Rows => group_digits(month.rows),
        MonthMetric::AvgPrice => fmt_money_per_kg(&month.measures, lang),
        MonthMetric::NetWeight => format!("{} kg", fmt_decimal(month.total_net_kg, 0)),
        MonthMetric::Value => fmt_money_exact(&month.measures, lang),
    }
}

#[cfg(test)]
mod tests {
    use super::{MonthMetric, month_metric_value};
    use crate::db::{
        AnalyticsCurrencyTotal, AnalyticsMeasures, AnalyticsMonthRow, AnalyticsValuePerWeight,
    };
    use crate::i18n::Lang;

    fn month(buckets: Vec<(&str, f64)>, raw_sum: f64) -> AnalyticsMonthRow {
        let currency_totals: Vec<AnalyticsCurrencyTotal> = buckets
            .iter()
            .map(|(currency, total)| AnalyticsCurrencyTotal {
                currency: (*currency).to_string(),
                known: true,
                valued_rows: 1,
                total_value: *total,
            })
            .collect();
        AnalyticsMonthRow {
            month: "2024-03".to_string(),
            rows: 2,
            // Still the bar's height, and still never printed as text.
            total_value_usd: raw_sum,
            total_net_kg: 100.0,
            measures: AnalyticsMeasures {
                value_per_net_weight: currency_totals
                    .iter()
                    .map(|total| AnalyticsValuePerWeight {
                        currency: total.currency.clone(),
                        normalized_weight_unit: "kg".to_string(),
                        source_weight_units: vec!["kg".to_string()],
                        paired_rows: 1,
                        total_value: total.total_value,
                        total_weight: 100.0,
                        value_per_weight: Some(total.total_value / 100.0),
                    })
                    .collect(),
                currency_totals,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_month_in_one_currency_is_read_out_with_it() {
        let march = month(vec![("EUR", 900.0)], 900.0);
        assert_eq!(
            month_metric_value(MonthMetric::Value, &march, Lang::En),
            "900 EUR"
        );
        assert_eq!(
            month_metric_value(MonthMetric::AvgPrice, &march, Lang::En),
            "9 EUR"
        );
    }

    /// Hovering a bar is how a person checks a number they are about to use.
    /// The bar can only have one height, but the figure under the cursor must
    /// not claim that height means one amount of money.
    #[test]
    fn a_month_holding_two_currencies_names_neither() {
        let march = month(vec![("USD", 600.0), ("EUR", 300.0)], 900.0);
        for metric in [MonthMetric::Value, MonthMetric::AvgPrice] {
            let shown = month_metric_value(metric, &march, Lang::En);
            assert_eq!(shown, "Several currencies");
        }
        assert_eq!(
            month_metric_value(MonthMetric::NetWeight, &march, Lang::En),
            "100 kg",
            "weight is not money and keeps its number"
        );
    }
}
