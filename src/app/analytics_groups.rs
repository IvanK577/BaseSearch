use super::ACCENT;
use super::format::{
    currency_breakdown, fmt_compact, fmt_decimal, fmt_money_compact, fmt_money_exact,
    fmt_money_per_kg,
};
use super::ui_text::trunc_label;
use crate::db::{AnalyticsFilterAction, AnalyticsGroupRow, AnalyticsSection, AnalyticsSectionKind};
use crate::i18n::{Lang, fmt, group_digits, tr};
use egui_extras::{Column, TableBuilder};

pub(super) enum AnalyticsCardAction {
    Filter(AnalyticsFilterAction),
    Explore(AnalyticsSectionKind),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupSort {
    Label,
    Rows,
    Declarations,
    Companies,
    Value,
    NetKg,
    GrossKg,
    Quantity,
    Share,
    ValuePerKg,
}

pub(super) struct GroupExplorerState {
    pub(super) kind: AnalyticsSectionKind,
    pub(super) generation: u64,
    pub(super) loading: bool,
    pub(super) rows: Vec<AnalyticsGroupRow>,
    pub(super) label_filter: String,
    pub(super) sort: GroupSort,
    pub(super) descending: bool,
}

pub(super) enum GroupExplorerAction {
    Close,
    Filter(AnalyticsFilterAction),
}

pub(super) fn group_explorer_window(
    ctx: &egui::Context,
    explorer: &mut GroupExplorerState,
    lang: Lang,
    full_section_limit: u64,
) -> Option<GroupExplorerAction> {
    let t = tr(lang);
    let mut open = true;
    let mut close = false;
    let mut action = None;
    let title = group_explorer_title(explorer.kind, lang);
    egui::Window::new(title)
        .id(egui::Id::new(format!(
            "analytics_group_explorer_{:?}",
            explorer.kind
        )))
        .open(&mut open)
        .default_width(980.0)
        .default_height(620.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if explorer.loading {
                    ui.spinner();
                    ui.label(t.searching);
                } else {
                    ui.label(egui::RichText::new(group_explorer_count(
                        explorer.rows.len() as u64,
                        explorer.rows.len() as u64 >= full_section_limit,
                        lang,
                    )));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t.close).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(
                egui::RichText::new(group_explorer_hint(lang))
                    .weak()
                    .small(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::singleline(&mut explorer.label_filter)
                    .hint_text(group_search_hint(lang))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(6.0);

            if explorer.loading {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                });
                return;
            }
            if explorer.rows.is_empty() {
                ui.label(egui::RichText::new(t.nothing_found).weak());
                return;
            }

            let needle = explorer.label_filter.trim().to_lowercase();
            let mut visible_rows: Vec<&AnalyticsGroupRow> = explorer
                .rows
                .iter()
                .filter(|row| needle.is_empty() || row.label.to_lowercase().contains(&needle))
                .collect();
            sort_group_rows(&mut visible_rows, explorer.sort, explorer.descending);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(group_visible_count(
                        visible_rows.len() as u64,
                        explorer.rows.len() as u64,
                        explorer.rows.len() as u64 >= full_section_limit,
                        lang,
                    ))
                    .weak()
                    .small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(format!("\u{29C9} {}", copy_visible_label(lang)))
                        .on_hover_text(copy_table_hover(lang))
                        .clicked()
                    {
                        ui.ctx().copy_text(group_rows_tsv(&visible_rows, lang));
                    }
                });
            });
            ui.add_space(4.0);

            if visible_rows.is_empty() {
                ui.label(egui::RichText::new(t.nothing_found).weak());
            } else if let Some(next) = group_explorer_table(
                ui,
                &visible_rows,
                &mut explorer.sort,
                &mut explorer.descending,
                lang,
            ) {
                action = Some(GroupExplorerAction::Filter(next));
            }
        });

    if !open || close {
        Some(GroupExplorerAction::Close)
    } else {
        action
    }
}

/// Cards of one analytics scope, laid out side by side so the whole scope
/// fits on screen without endless scrolling.
pub(super) fn analytics_cards(
    ui: &mut egui::Ui,
    sections: &[AnalyticsSection],
    lang: Lang,
) -> Option<AnalyticsCardAction> {
    analytics_cards_with_options(ui, sections, lang, true)
}

pub(super) fn analytics_cards_with_options(
    ui: &mut egui::Ui,
    sections: &[AnalyticsSection],
    lang: Lang,
    allow_explore: bool,
) -> Option<AnalyticsCardAction> {
    let mut action = None;
    let sections: Vec<&AnalyticsSection> = sections.iter().filter(|s| !s.rows.is_empty()).collect();
    if sections.is_empty() {
        return None;
    }
    let gap = 10.0;
    let avail = ui.available_width();
    let per_row = if avail >= 960.0 {
        3.min(sections.len())
    } else if avail >= 640.0 {
        2.min(sections.len())
    } else {
        1
    };
    let card_w = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32).max(260.0);
    for chunk in sections.chunks(per_row) {
        ui.with_layout(
            egui::Layout::left_to_right(egui::Align::Min).with_main_align(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                for section in chunk {
                    ui.allocate_ui_with_layout(
                        egui::vec2(card_w, 10.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(card_w);
                            ui.set_max_width(card_w);
                            if let Some(next) = analytics_card(ui, section, lang, allow_explore) {
                                action = Some(next);
                            }
                        },
                    );
                }
            },
        );
        ui.add_space(gap);
    }
    action
}

/// Card rows as a TSV table that pastes directly into Excel.
fn section_tsv(section: &AnalyticsSection, lang: Lang) -> String {
    let rows: Vec<&AnalyticsGroupRow> = section.rows.iter().collect();
    group_rows_tsv(&rows, lang)
}

pub(super) fn group_rows_tsv(rows: &[&AnalyticsGroupRow], lang: Lang) -> String {
    let header = tr(lang).group_tsv_header;
    let mut out = String::from(header);
    for row in rows {
        out.push('\n');
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{}",
            row.label,
            row.rows,
            row.declarations,
            row.companies,
            fmt_money_exact(&row.measures, lang),
            row.total_net_kg,
            row.total_gross_kg,
            row.total_quantity,
            row.share_percent,
            fmt_money_per_kg(&row.measures, lang)
        ));
    }
    out
}

pub(super) fn copy_table_hover(lang: Lang) -> &'static str {
    tr(lang).copy_table_hover
}

fn all_rows_button(lang: Lang) -> &'static str {
    tr(lang).all_label
}

fn all_rows_hover(lang: Lang) -> &'static str {
    tr(lang).all_rows_hover
}

fn analytics_card(
    ui: &mut egui::Ui,
    section: &AnalyticsSection,
    lang: Lang,
    allow_explore: bool,
) -> Option<AnalyticsCardAction> {
    let mut action = None;
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(section_title(section.kind, lang)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("\u{29C9}")
                        .on_hover_text(copy_table_hover(lang))
                        .clicked()
                    {
                        ui.ctx().copy_text(section_tsv(section, lang));
                    }
                    if allow_explore
                        && ui
                            .small_button(all_rows_button(lang))
                            .on_hover_text(all_rows_hover(lang))
                            .clicked()
                    {
                        action = Some(AnalyticsCardAction::Explore(section.kind));
                    }
                });
            });
            ui.add_space(4.0);
            for row in &section.rows {
                if let Some(next) = compact_bar_row(ui, row, lang) {
                    action = Some(AnalyticsCardAction::Filter(next));
                }
            }
            let total_share: f64 = section.rows.iter().map(|r| r.share_percent).sum();
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(fmt(
                    top_share_pattern(lang),
                    &[
                        &section.rows.len().to_string(),
                        &fmt_decimal(total_share.min(100.0), 1),
                    ],
                ))
                .weak()
                .small(),
            );
        });
    action
}

/// One compact clickable row: label, share bar, value and percentage.
/// Full numbers are in the hover tooltip; click applies the filter.
fn compact_bar_row(
    ui: &mut egui::Ui,
    row: &AnalyticsGroupRow,
    lang: Lang,
) -> Option<AnalyticsFilterAction> {
    let width = ui.available_width();
    let height = 24.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let visuals = ui.visuals();
    let rounding = egui::CornerRadius::same(3);
    let hover_t = ui.ctx().animate_bool_with_time(
        egui::Id::new(("analytics_bar_row", &row.label)),
        response.hovered(),
        0.10,
    );
    if hover_t > 0.0 {
        ui.painter().rect_filled(
            rect,
            rounding,
            visuals.widgets.hovered.weak_bg_fill.gamma_multiply(hover_t),
        );
    }
    let share_width = (rect.width() * (row.share_percent as f32 / 100.0)).clamp(0.0, rect.width());
    let share_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - 3.0),
        egui::vec2(share_width, 3.0),
    );
    let bar_bg = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - 3.0),
        egui::vec2(rect.width(), 3.0),
    );
    let bar_color = if visuals.dark_mode {
        egui::Color32::from_rgb(80, 140, 255)
    } else {
        ACCENT
    };
    ui.painter()
        .rect_filled(bar_bg, rounding, visuals.faint_bg_color);
    ui.painter().rect_filled(
        share_rect,
        rounding,
        bar_color.gamma_multiply(0.72 + hover_t * 0.28),
    );

    let label_font = egui::FontId::new(12.5, egui::FontFamily::Proportional);
    let mono_font = egui::FontId::new(11.5, egui::FontFamily::Monospace);
    let right_text = format!(
        "{} · {}%",
        fmt_money_compact(&row.measures, lang),
        fmt_decimal(row.share_percent, 1)
    );
    let right_w = right_text.chars().count() as f32 * 7.0;
    ui.painter().text(
        egui::pos2(rect.left() + 2.0, rect.top() + 9.0),
        egui::Align2::LEFT_CENTER,
        trunc_label(
            &row.label,
            ((width - right_w - 12.0) / 6.8).max(8.0) as usize,
        ),
        label_font,
        visuals.text_color(),
    );
    ui.painter().text(
        egui::pos2(rect.right() - 2.0, rect.top() + 9.0),
        egui::Align2::RIGHT_CENTER,
        right_text,
        mono_font,
        visuals.weak_text_color(),
    );

    if response.hovered() {
        response.show_tooltip_ui(|ui| analytics_row_tooltip_ui(ui, row, lang));
    }
    if response.clicked() {
        row.filter_action.clone()
    } else {
        None
    }
}

fn analytics_row_tooltip_ui(ui: &mut egui::Ui, row: &AnalyticsGroupRow, lang: Lang) {
    ui.set_min_width(240.0);
    ui.strong(&row.label);
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new(row_counts_label(row, lang))
            .weak()
            .small(),
    );
    ui.separator();
    egui::Grid::new(("analytics_row_tip", &row.label))
        .num_columns(2)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            tooltip_metric(
                ui,
                tr(lang).total_value,
                fmt_money_exact(&row.measures, lang),
            );
            // The cell above can only say "several currencies". The hover is
            // where this row's actual money is, so it belongs here.
            if let Some(breakdown) = currency_breakdown(&row.measures, lang) {
                tooltip_metric(ui, tr(lang).currency_breakdown, breakdown);
            }
            tooltip_metric(
                ui,
                tr(lang).net_weight,
                format!("{} kg", fmt_decimal(row.total_net_kg, 3)),
            );
            tooltip_metric(
                ui,
                tr(lang).gross_weight,
                format!("{} kg", fmt_decimal(row.total_gross_kg, 3)),
            );
            tooltip_metric(ui, tr(lang).quantity, fmt_decimal(row.total_quantity, 3));
            tooltip_metric(
                ui,
                share_header(lang),
                format!("{}%", fmt_decimal(row.share_percent, 2)),
            );
            tooltip_metric(
                ui,
                tr(lang).avg_value_kg,
                fmt_money_per_kg(&row.measures, lang),
            );
        });
    ui.add_space(3.0);
    ui.label(egui::RichText::new(pivot_click_hint(lang)).weak().small());
}

fn tooltip_metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(egui::RichText::new(label).weak().small());
    ui.label(egui::RichText::new(value).monospace());
    ui.end_row();
}

pub(super) fn group_explorer_table(
    ui: &mut egui::Ui,
    rows: &[&AnalyticsGroupRow],
    sort: &mut GroupSort,
    descending: &mut bool,
    lang: Lang,
) -> Option<AnalyticsFilterAction> {
    let t = tr(lang);
    let mut action = None;
    egui::ScrollArea::horizontal()
        .id_salt("group_explorer_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(270.0).at_least(140.0).clip(true))
                .column(Column::initial(82.0).at_least(64.0))
                .column(Column::initial(104.0).at_least(78.0))
                .column(Column::initial(94.0).at_least(70.0))
                .column(Column::initial(110.0).at_least(86.0))
                .column(Column::initial(98.0).at_least(74.0))
                .column(Column::initial(98.0).at_least(74.0))
                .column(Column::initial(96.0).at_least(72.0))
                .column(Column::initial(82.0).at_least(62.0))
                .column(Column::initial(96.0).at_least(72.0))
                .header(24.0, |mut header| {
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            label_header(lang),
                            GroupSort::Label,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(ui, t.rows_label, GroupSort::Rows, sort, descending);
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            t.declarations_label,
                            GroupSort::Declarations,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            companies_header(lang),
                            GroupSort::Companies,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            t.total_value,
                            GroupSort::Value,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(ui, t.net_weight, GroupSort::NetKg, sort, descending);
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            t.gross_weight,
                            GroupSort::GrossKg,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            t.quantity,
                            GroupSort::Quantity,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            share_header(lang),
                            GroupSort::Share,
                            sort,
                            descending,
                        );
                    });
                    header.col(|ui| {
                        sortable_group_header(
                            ui,
                            t.avg_value_kg,
                            GroupSort::ValuePerKg,
                            sort,
                            descending,
                        );
                    });
                })
                .body(|body| {
                    body.rows(24.0, rows.len(), |mut table_row| {
                        let row = rows[table_row.index()];
                        let mut clicked = false;
                        table_row.col(|ui| {
                            clicked |= group_text_cell(ui, &row.label, row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |= group_numeric_cell(ui, group_digits(row.rows), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |=
                                group_numeric_cell(ui, group_digits(row.declarations), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |=
                                group_numeric_cell(ui, group_digits(row.companies), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |= group_numeric_cell(
                                ui,
                                fmt_money_compact(&row.measures, lang),
                                row,
                                lang,
                            );
                        });
                        table_row.col(|ui| {
                            clicked |=
                                group_numeric_cell(ui, fmt_compact(row.total_net_kg), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |=
                                group_numeric_cell(ui, fmt_compact(row.total_gross_kg), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |=
                                group_numeric_cell(ui, fmt_compact(row.total_quantity), row, lang);
                        });
                        table_row.col(|ui| {
                            clicked |= group_numeric_cell(
                                ui,
                                format!("{}%", fmt_decimal(row.share_percent, 1)),
                                row,
                                lang,
                            );
                        });
                        table_row.col(|ui| {
                            clicked |= group_numeric_cell(
                                ui,
                                fmt_money_per_kg(&row.measures, lang),
                                row,
                                lang,
                            );
                        });
                        if clicked {
                            action = row.filter_action.clone();
                        }
                    });
                });
        });
    action
}

fn sortable_group_header(
    ui: &mut egui::Ui,
    label: &str,
    column: GroupSort,
    current: &mut GroupSort,
    descending: &mut bool,
) {
    let selected = *current == column;
    let arrow = if selected {
        if *descending { " ▼" } else { " ▲" }
    } else {
        ""
    };
    if ui.small_button(format!("{label}{arrow}")).clicked() {
        if selected {
            *descending = !*descending;
        } else {
            *current = column;
            *descending = column != GroupSort::Label;
        }
    }
}

fn group_text_cell(ui: &mut egui::Ui, text: &str, row: &AnalyticsGroupRow, lang: Lang) -> bool {
    ui.add(egui::Label::new(text).selectable(false).truncate())
        .on_hover_text(row_hover_text(row, lang))
        .clicked()
}

fn group_numeric_cell(
    ui: &mut egui::Ui,
    text: String,
    row: &AnalyticsGroupRow,
    lang: Lang,
) -> bool {
    ui.add(egui::Label::new(egui::RichText::new(text).monospace()).selectable(false))
        .on_hover_text(row_hover_text(row, lang))
        .clicked()
}

pub(super) fn sort_group_rows(rows: &mut [&AnalyticsGroupRow], sort: GroupSort, descending: bool) {
    rows.sort_by(|a, b| {
        let ord = match sort {
            GroupSort::Label => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
            GroupSort::Rows => a.rows.cmp(&b.rows),
            GroupSort::Declarations => a.declarations.cmp(&b.declarations),
            GroupSort::Companies => a.companies.cmp(&b.companies),
            GroupSort::Value => cmp_f64(a.total_value_usd, b.total_value_usd),
            GroupSort::NetKg => cmp_f64(a.total_net_kg, b.total_net_kg),
            GroupSort::GrossKg => cmp_f64(a.total_gross_kg, b.total_gross_kg),
            GroupSort::Quantity => cmp_f64(a.total_quantity, b.total_quantity),
            GroupSort::Share => cmp_f64(a.share_percent, b.share_percent),
            GroupSort::ValuePerKg => cmp_f64(a.avg_value_per_net_kg, b.avg_value_per_net_kg),
        };
        if descending { ord.reverse() } else { ord }
    });
}

fn cmp_f64(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

fn top_share_pattern(lang: Lang) -> &'static str {
    tr(lang).top_share_pattern
}

pub(super) fn group_explorer_title(kind: AnalyticsSectionKind, lang: Lang) -> String {
    fmt(tr(lang).group_all_title, &[section_title(kind, lang)])
}

pub(super) fn group_explorer_hint(lang: Lang) -> &'static str {
    tr(lang).group_explorer_hint
}

pub(super) fn group_search_hint(lang: Lang) -> &'static str {
    tr(lang).group_search_hint
}

pub(super) fn group_explorer_count(rows: u64, limited: bool, lang: Lang) -> String {
    let pattern = if limited {
        tr(lang).group_loaded_first
    } else {
        tr(lang).group_loaded_rows
    };
    fmt(pattern, &[&group_digits(rows)])
}

pub(super) fn group_visible_count(visible: u64, total: u64, limited: bool, lang: Lang) -> String {
    let pattern = if limited {
        tr(lang).group_showing_first
    } else {
        tr(lang).group_showing
    };
    fmt(pattern, &[&group_digits(visible), &group_digits(total)])
}

pub(super) fn copy_visible_label(lang: Lang) -> &'static str {
    tr(lang).copy_visible
}

fn label_header(lang: Lang) -> &'static str {
    tr(lang).col_label
}

fn companies_header(lang: Lang) -> &'static str {
    tr(lang).col_companies
}

fn share_header(lang: Lang) -> &'static str {
    tr(lang).col_share
}

pub(super) fn section_title(kind: AnalyticsSectionKind, lang: Lang) -> &'static str {
    let t = tr(lang);
    match kind {
        AnalyticsSectionKind::Recipients => t.sec_recipients,
        AnalyticsSectionKind::Senders => t.sec_senders,
        AnalyticsSectionKind::Edrpou => t.sec_edrpou,
        AnalyticsSectionKind::ProductCodes => t.sec_product_codes,
        AnalyticsSectionKind::Trademarks => t.sec_trademarks,
        AnalyticsSectionKind::ProductGroups => t.sec_product_groups,
        AnalyticsSectionKind::OriginCountries => t.sec_origin_countries,
        AnalyticsSectionKind::DispatchCountries => t.sec_dispatch_countries,
        AnalyticsSectionKind::TradeCountries => t.sec_trade_countries,
    }
}

fn row_counts_label(row: &AnalyticsGroupRow, lang: Lang) -> String {
    fmt(
        tr(lang).row_counts,
        &[
            &group_digits(row.rows),
            &group_digits(row.declarations),
            &group_digits(row.companies),
        ],
    )
}

fn row_hover_text(row: &AnalyticsGroupRow, lang: Lang) -> String {
    let counts = row_counts_label(row, lang);
    fmt(
        tr(lang).row_hover,
        &[
            &row.label,
            &counts,
            &fmt_money_exact(&row.measures, lang),
            &fmt_decimal(row.total_net_kg, 3),
            &fmt_decimal(row.share_percent, 2),
            &fmt_money_per_kg(&row.measures, lang),
        ],
    )
}

fn pivot_click_hint(lang: Lang) -> &'static str {
    tr(lang).pivot_click_hint
}

#[cfg(test)]
mod tests {
    use super::group_rows_tsv;
    use crate::db::{
        AnalyticsCurrencyTotal, AnalyticsGroupRow, AnalyticsMeasures, AnalyticsValuePerWeight,
    };
    use crate::i18n::Lang;

    fn row(label: &str, buckets: Vec<(&str, bool, f64)>, raw_sum: f64) -> AnalyticsGroupRow {
        let currency_totals: Vec<AnalyticsCurrencyTotal> = buckets
            .iter()
            .map(|(currency, known, total)| AnalyticsCurrencyTotal {
                currency: (*currency).to_string(),
                known: *known,
                valued_rows: 1,
                total_value: *total,
            })
            .collect();
        AnalyticsGroupRow {
            label: label.to_string(),
            rows: 2,
            // The scalar the table used to print. It stays populated on
            // purpose: the point of these tests is that nothing renders it.
            total_value_usd: raw_sum,
            avg_value_per_net_kg: 4.0,
            total_net_kg: 250.0,
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

    /// The table copies straight into Excel, so what it copies is what the
    /// person then works from. A cross-currency sum pasted into a sheet is
    /// worse than one on screen: it outlives the window that produced it.
    #[test]
    fn a_row_in_one_currency_is_copied_with_that_currency() {
        let rows = [row("ACME", vec![("USD", true, 1500.0)], 1500.0)];
        let tsv = group_rows_tsv(&rows.iter().collect::<Vec<_>>(), Lang::En);
        let line = tsv.lines().nth(1).expect("one data line");
        assert!(line.contains("\t1\u{202F}500 USD\t"), "{line}");
        assert!(line.ends_with("\t15 USD"), "value per kg too: {line}");
    }

    #[test]
    fn a_row_spanning_two_currencies_is_copied_as_neither() {
        let rows = [row(
            "GLOBEX",
            vec![("USD", true, 1000.0), ("EUR", true, 500.0)],
            1500.0,
        )];
        let tsv = group_rows_tsv(&rows.iter().collect::<Vec<_>>(), Lang::En);
        let line = tsv.lines().nth(1).expect("one data line");
        assert!(line.contains("Several currencies"), "{line}");
        assert!(
            !line.contains("1500"),
            "the cross-currency sum reached the clipboard: {line}"
        );
    }

    /// A source that never stated its currency still has one, and its number
    /// has always been shown bare. Labelling it "several currencies" would be
    /// a new lie in place of the old one.
    #[test]
    fn a_row_whose_currency_was_never_stated_still_shows_its_number() {
        let rows = [row("NO-CODE", vec![("__unknown__", false, 800.0)], 800.0)];
        let tsv = group_rows_tsv(&rows.iter().collect::<Vec<_>>(), Lang::En);
        let line = tsv.lines().nth(1).expect("one data line");
        assert!(line.contains("\t800\t"), "{line}");
        assert!(!line.contains("Several currencies"), "{line}");
    }
}
