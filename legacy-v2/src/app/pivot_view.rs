use super::ACCENT;
use super::format::fmt_compact;
use crate::db::{AnalyticsFilterAction, PivotDim, PivotMetric, PivotResult, pivot_filter_action};
use crate::i18n::{Lang, group_digits, tr};
use egui_extras::{Column, TableBuilder};

fn pivot_dim_label(dim: PivotDim, lang: Lang) -> &'static str {
    let t = tr(lang);
    match dim {
        PivotDim::Recipient => t.recipient,
        PivotDim::Sender => t.sender,
        PivotDim::Edrpou => t.edrpou,
        PivotDim::ProductCode => t.product_code,
        PivotDim::Trademark => t.trademark,
        PivotDim::OriginCountry => t.origin_country,
        PivotDim::DispatchCountry => t.dispatch_country,
        PivotDim::TradeCountry => t.trade_country,
        PivotDim::Month => t.month,
        PivotDim::Year => t.year,
    }
}

const PIVOT_DIMS: [PivotDim; 10] = [
    PivotDim::Recipient,
    PivotDim::Sender,
    PivotDim::Edrpou,
    PivotDim::ProductCode,
    PivotDim::Trademark,
    PivotDim::OriginCountry,
    PivotDim::DispatchCountry,
    PivotDim::TradeCountry,
    PivotDim::Month,
    PivotDim::Year,
];

pub(super) fn pivot_dim_combo(
    ui: &mut egui::Ui,
    id: &str,
    current: PivotDim,
    lang: Lang,
    out: &mut Option<PivotDim>,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(pivot_dim_label(current, lang))
        .show_ui(ui, |ui| {
            for dim in PIVOT_DIMS {
                if ui
                    .selectable_label(dim == current, pivot_dim_label(dim, lang))
                    .clicked()
                    && dim != current
                {
                    *out = Some(dim);
                }
            }
        });
}

/// Heatmap-style cross-tab. Row/column labels are clickable to drill into
/// the Results table; cell shading shows relative size within the matrix.
pub(super) fn pivot_table_ui(
    ui: &mut egui::Ui,
    pivot: &PivotResult,
    row_dim: PivotDim,
    col_dim: PivotDim,
    metric: PivotMetric,
    lang: Lang,
) -> Option<AnalyticsFilterAction> {
    let mut action: Option<AnalyticsFilterAction> = None;
    let max_cell = pivot
        .cells
        .iter()
        .flat_map(|r| r.iter())
        .copied()
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    let accent = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(80, 140, 255)
    } else {
        ACCENT
    };
    let total_label = tr(lang).total;

    egui::ScrollArea::both().show(ui, |ui| {
        let mut builder = TableBuilder::new(ui)
            .striped(false)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(190.0).at_least(120.0).clip(true));
        for _ in &pivot.col_labels {
            builder = builder.column(Column::initial(84.0).at_least(56.0));
        }
        builder = builder.column(Column::initial(92.0).at_least(64.0));
        builder
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong(pivot_dim_label(row_dim, lang));
                });
                for (ci, label) in pivot.col_labels.iter().enumerate() {
                    header.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_others =
                                pivot.cols_truncated && ci + 1 == pivot.col_labels.len();
                            let label_text = egui::RichText::new(label.clone()).strong();
                            if is_others || !pivot.col_filterable {
                                ui.label(label_text);
                            } else if let Some(next) = pivot_filter_action(col_dim, label.clone()) {
                                let response = ui
                                    .add(egui::Label::new(label_text).sense(egui::Sense::click()))
                                    .on_hover_text(pivot_click_hint(lang));
                                if response.clicked() {
                                    action = Some(next);
                                }
                            } else {
                                ui.label(label_text);
                            }
                        });
                    });
                }
                header.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.strong(total_label);
                    });
                });
            })
            .body(|mut body| {
                for (ri, row_label) in pivot.row_labels.iter().enumerate() {
                    body.row(22.0, |mut row| {
                        row.col(|ui| {
                            let resp = ui.add(
                                egui::Label::new(row_label)
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            );
                            let is_others =
                                pivot.rows_truncated && ri + 1 == pivot.row_labels.len();
                            if pivot.row_filterable
                                && !is_others
                                && let Some(next) = pivot_filter_action(row_dim, row_label.clone())
                            {
                                let resp = resp.on_hover_text(pivot_click_hint(lang));
                                if resp.clicked() {
                                    action = Some(next);
                                }
                            }
                        });
                        for ci in 0..pivot.col_labels.len() {
                            let v = pivot.cells[ri][ci];
                            row.col(|ui| {
                                paint_pivot_cell(ui, v, max_cell, accent, metric);
                            });
                        }
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(pivot_fmt(
                                            pivot.row_totals[ri],
                                            metric,
                                        ))
                                        .monospace()
                                        .strong(),
                                    );
                                },
                            );
                        });
                    });
                }
                body.row(22.0, |mut row| {
                    row.col(|ui| {
                        ui.strong(total_label);
                    });
                    for ci in 0..pivot.col_labels.len() {
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(pivot_fmt(
                                            pivot.col_totals[ci],
                                            metric,
                                        ))
                                        .monospace()
                                        .strong(),
                                    );
                                },
                            );
                        });
                    }
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(pivot_fmt(pivot.grand_total, metric))
                                    .monospace()
                                    .strong(),
                            );
                        });
                    });
                });
            });
    });
    action
}

fn paint_pivot_cell(
    ui: &mut egui::Ui,
    value: f64,
    max_cell: f64,
    accent: egui::Color32,
    metric: PivotMetric,
) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
    if value > 0.0 {
        let intensity = (value / max_cell).clamp(0.0, 1.0) as f32;
        let alpha = (18.0 + intensity * 150.0) as u8;
        let fill = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha);
        ui.painter()
            .rect_filled(rect.shrink(1.0), egui::CornerRadius::same(2), fill);
        let text_color = ui.visuals().text_color();
        ui.painter().text(
            egui::pos2(rect.right() - 4.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            pivot_fmt(value, metric),
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            text_color,
        );
    }
}

fn pivot_fmt(value: f64, metric: PivotMetric) -> String {
    match metric {
        PivotMetric::Rows => group_digits(value as u64),
        _ => fmt_compact(value),
    }
}

fn pivot_click_hint(lang: Lang) -> &'static str {
    tr(lang).pivot_click_hint
}

/// Pivot matrix as TSV, ready to paste into Excel.
pub(super) fn pivot_tsv(
    pivot: &PivotResult,
    row_dim: PivotDim,
    _col_dim: PivotDim,
    lang: Lang,
) -> String {
    let total_label = tr(lang).total;
    let mut out = String::new();
    out.push_str(pivot_dim_label(row_dim, lang));
    for c in &pivot.col_labels {
        out.push('\t');
        out.push_str(c);
    }
    out.push('\t');
    out.push_str(total_label);
    for (ri, rl) in pivot.row_labels.iter().enumerate() {
        out.push('\n');
        out.push_str(rl);
        for ci in 0..pivot.col_labels.len() {
            out.push('\t');
            out.push_str(&format!("{:.2}", pivot.cells[ri][ci]));
        }
        out.push('\t');
        out.push_str(&format!("{:.2}", pivot.row_totals[ri]));
    }
    out.push('\n');
    out.push_str(total_label);
    for ci in 0..pivot.col_labels.len() {
        out.push('\t');
        out.push_str(&format!("{:.2}", pivot.col_totals[ci]));
    }
    out.push('\t');
    out.push_str(&format!("{:.2}", pivot.grand_total));
    out
}
