use super::analytics_groups::{AnalyticsCardAction, analytics_cards};
use super::compare_view::{
    analytics_compare_label, compare_empty, compare_hint, compare_previous_year_label,
    compare_run_label, compare_text_label, compare_ui,
};
use super::format::{currency_breakdown, fmt_compact, fmt_money_compact, fmt_money_per_kg};
use super::month_chart::{MonthMetric, months_chart};
use super::overview_view::{
    overview_dispatch_countries_help, overview_dispatch_countries_label, overview_edrpou_help,
    overview_gross_help, overview_origin_countries_label, overview_quantity_help,
    overview_senders_help, overview_story_cards, overview_trade_countries_help,
    overview_trade_countries_label, overview_trademarks_help,
};
use super::pivot_view::{pivot_dim_combo, pivot_table_ui};
use super::price_view::price_table;
use super::reports::{
    analytics_report_label, report_copy_label, report_export_label, report_hint, report_title,
    report_ui,
};
use super::state::AnalyticsView;
use super::ui_text::{analytics_calc_lines, analytics_calc_short_note, analytics_calc_title};
use super::underpricing_view::underpricing_table;
use super::widgets::kpi_tile;
use crate::db::{
    Analytics, AnalyticsFilterAction, AnalyticsSectionKind, PivotDim, PivotMetric, PivotResult,
    Query, Undervaluation,
};
use crate::i18n::{Lang, Tr, fmt, group_digits};

pub(super) struct AnalyticsViewInput<'a> {
    pub(super) active_query: &'a Query,
    pub(super) analytics: Option<&'a Analytics>,
    pub(super) analytics_loading: bool,
    pub(super) search_in_flight: bool,
    pub(super) analytics_view: AnalyticsView,
    pub(super) analytics_loaded: &'a [bool; AnalyticsView::COUNT],
    pub(super) analytics_limit: u64,
    pub(super) month_metric: MonthMetric,
    pub(super) hs_level: u8,
    pub(super) pivot: Option<&'a PivotResult>,
    pub(super) pivot_row_dim: PivotDim,
    pub(super) pivot_col_dim: PivotDim,
    pub(super) pivot_metric: PivotMetric,
    pub(super) underpricing: Option<&'a Undervaluation>,
    pub(super) underpricing_loading: bool,
    pub(super) compare_text: &'a str,
    pub(super) compare_year: &'a str,
    pub(super) compare_analytics: Option<&'a Analytics>,
    pub(super) compare_query: Option<&'a Query>,
    pub(super) compare_loading: bool,
    pub(super) report_ready: bool,
    pub(super) lang: Lang,
    pub(super) t: &'static Tr,
}

#[derive(Default)]
pub(super) struct AnalyticsViewActions {
    pub(super) request_analytics: bool,
    pub(super) filter_action: Option<AnalyticsFilterAction>,
    pub(super) explore_kind: Option<AnalyticsSectionKind>,
    pub(super) show_more: bool,
    pub(super) new_metric: Option<MonthMetric>,
    pub(super) new_view: Option<AnalyticsView>,
    pub(super) new_hs: Option<u8>,
    pub(super) new_pivot_row: Option<PivotDim>,
    pub(super) new_pivot_col: Option<PivotDim>,
    pub(super) new_pivot_metric: Option<PivotMetric>,
    pub(super) copy_pivot: bool,
    pub(super) copy_report: bool,
    pub(super) export_report: bool,
    pub(super) scan_underpricing: bool,
    pub(super) open_card_id: Option<i64>,
    pub(super) compare_text: Option<String>,
    pub(super) compare_year: Option<String>,
    pub(super) run_compare: bool,
}

pub(super) fn analytics_view_panel(
    root: &mut egui::Ui,
    input: AnalyticsViewInput<'_>,
) -> AnalyticsViewActions {
    let mut actions = AnalyticsViewActions::default();
    egui::CentralPanel::default().show_inside(root, |ui| {
        let t = input.t;
        if input.active_query.is_empty() {
            ui.add_space((ui.available_height() * 0.30).max(0.0));
            ui.vertical_centered(|ui| {
                ui.heading(t.analytics);
                ui.add_space(8.0);
                ui.label(egui::RichText::new(t.analytics_hint).weak());
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Try a SKU, invoice number, company name, warehouse, or year 2024.",
                    )
                    .weak()
                    .small(),
                );
                ui.label(
                    egui::RichText::new(
                        "Analytics always uses the same search and filters as Results.",
                    )
                    .weak()
                    .small(),
                );
            });
            return;
        }

        let Some(analytics) = input.analytics else {
            actions.request_analytics = !input.analytics_loading;
            ui.add_space((ui.available_height() * 0.30).max(0.0));
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.add_space(8.0);
                ui.label(t.searching);
            });
            return;
        };

        let p_row = input.pivot_row_dim;
        let p_col = input.pivot_col_dim;
        let p_metric = input.pivot_metric;
        let view = input.analytics_view;
        let view_ready = input.analytics_loaded[view.index()];
        let lang = input.lang;
        let mut compare_text = input.compare_text.to_string();
        let mut compare_year = input.compare_year.to_string();

        ui.horizontal(|ui| {
            for v in AnalyticsView::ALL {
                let label = match v {
                    AnalyticsView::Overview => t.tab_overview,
                    AnalyticsView::Companies => t.companies_section,
                    AnalyticsView::Products => t.products_section,
                    AnalyticsView::Countries => t.countries_section,
                    AnalyticsView::Prices => t.prices_section,
                    AnalyticsView::Pivot => t.tab_pivot,
                    AnalyticsView::Report => analytics_report_label(lang),
                    AnalyticsView::Compare => analytics_compare_label(lang),
                };
                if ui.selectable_label(view == v, label).clicked() && v != view {
                    actions.new_view = Some(v);
                }
            }
            if input.analytics_loading || input.search_in_flight {
                ui.spinner();
            }
            if matches!(
                view,
                AnalyticsView::Companies | AnalyticsView::Products | AnalyticsView::Countries
            ) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if input.analytics_limit < 50 && ui.button(t.show_more).clicked() {
                        actions.show_more = true;
                    }
                    let shown = input.analytics_limit.min(50);
                    ui.label(egui::RichText::new(fmt(t.showing_top, &[&shown.to_string()])).weak());
                });
            }
        });

        ui.horizontal(|ui| {
            let summary = ui.label(
                egui::RichText::new(fmt(
                    t.mini_summary,
                    &[
                        &group_digits(analytics.overview.row_count),
                        &fmt_money_compact(&analytics.overview.measures, lang),
                        &fmt_compact(analytics.overview.total_net_kg),
                    ],
                ))
                .weak()
                .small(),
            );
            summary.on_hover_text(analytics_calc_short_note(lang));
            if let (Some(first), Some(last)) = (analytics.months.first(), analytics.months.last()) {
                ui.label(
                    egui::RichText::new(fmt(
                        t.period_of,
                        &[
                            &first.month,
                            &last.month,
                            &analytics.months.len().to_string(),
                        ],
                    ))
                    .weak()
                    .small(),
                );
            }
        });
        // When the figures above read "several currencies", this is where the
        // person finds out which ones and how much of each. Absent on the
        // ordinary single-currency workspace, where it would be noise.
        if let Some(breakdown) = currency_breakdown(&analytics.overview.measures, lang) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t.currency_breakdown).weak().small());
                ui.label(egui::RichText::new(breakdown).small().strong());
            });
        }
        egui::CollapsingHeader::new(analytics_calc_title(lang))
            .id_salt("analytics_calculation_notes")
            .show(ui, |ui| {
                for line in analytics_calc_lines(lang) {
                    ui.label(egui::RichText::new(*line).weak().small());
                }
            });
        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            match view {
                AnalyticsView::Overview => {
                    overview_panel(ui, analytics, input.month_metric, lang, t, &mut actions)
                }
                AnalyticsView::Companies | AnalyticsView::Countries => {
                    let (sections, hint) = if view == AnalyticsView::Companies {
                        (&analytics.company_sections, t.companies_section_hint)
                    } else {
                        (&analytics.country_sections, t.countries_section_hint)
                    };
                    ui.label(egui::RichText::new(hint).weak().small());
                    ui.add_space(6.0);
                    if !view_ready {
                        loading_block(ui);
                    } else if let Some(next) = analytics_cards(ui, sections, lang) {
                        apply_card_action(&mut actions, next);
                    }
                }
                AnalyticsView::Products => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(t.products_section_hint).weak().small());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (level, label) in [(10u8, t.hs_full), (6, "6"), (4, "4"), (2, "2")]
                            {
                                if ui
                                    .selectable_label(input.hs_level == level, label)
                                    .clicked()
                                    && level != input.hs_level
                                {
                                    actions.new_hs = Some(level);
                                }
                            }
                            ui.label(egui::RichText::new(t.hs_level_label).weak().small());
                        });
                    });
                    ui.add_space(6.0);
                    if !view_ready {
                        loading_block(ui);
                    } else if let Some(next) =
                        analytics_cards(ui, &analytics.product_sections, lang)
                    {
                        apply_card_action(&mut actions, next);
                    }
                }
                AnalyticsView::Prices => {
                    ui.label(egui::RichText::new(t.prices_section_hint).weak().small());
                    ui.add_space(6.0);
                    if !view_ready {
                        loading_block(ui);
                    } else {
                        price_table(ui, &analytics.price_sections, lang);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(t.currency_note).weak().small());

                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(t.underpricing_title).strong());
                        ui.label(egui::RichText::new(t.underpricing_hint).weak().small());
                        ui.add_space(6.0);
                        match input.underpricing {
                            _ if input.underpricing_loading => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(t.searching);
                                });
                            }
                            Some(uv) => {
                                if let Some(id) =
                                    underpricing_table(ui, uv, lang, &mut actions.scan_underpricing)
                                {
                                    actions.open_card_id = Some(id);
                                }
                            }
                            None => {
                                if ui
                                    .button(format!("\u{1F6A9} {}", t.underpricing_scan))
                                    .clicked()
                                {
                                    actions.scan_underpricing = true;
                                }
                            }
                        }
                    }
                }
                AnalyticsView::Pivot => {
                    ui.label(egui::RichText::new(t.pivot_hint).weak().small());
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(t.pivot_rows).strong());
                        pivot_dim_combo(ui, "pv_row", p_row, lang, &mut actions.new_pivot_row);
                        ui.separator();
                        ui.label(egui::RichText::new(t.pivot_cols).strong());
                        pivot_dim_combo(ui, "pv_col", p_col, lang, &mut actions.new_pivot_col);
                        ui.separator();
                        ui.label(egui::RichText::new(t.pivot_metric_label).strong());
                        for (m, label) in [
                            (PivotMetric::Value, t.metric_value),
                            (PivotMetric::Rows, t.metric_rows),
                            (PivotMetric::NetKg, t.metric_weight),
                        ] {
                            if ui.selectable_label(p_metric == m, label).clicked() && p_metric != m
                            {
                                actions.new_pivot_metric = Some(m);
                            }
                        }
                    });
                    ui.add_space(6.0);
                    match input.pivot {
                        Some(pivot) if input.analytics_loaded[AnalyticsView::Pivot.index()] => {
                            if pivot.row_labels.is_empty() {
                                ui.add_space(16.0);
                                ui.label(egui::RichText::new(t.nothing_found).weak());
                            } else {
                                ui.horizontal(|ui| {
                                    if ui
                                        .small_button(format!("\u{29C9} {}", t.copy_all))
                                        .on_hover_text(t.copy_table_hover)
                                        .clicked()
                                    {
                                        actions.copy_pivot = true;
                                    }
                                });
                                ui.add_space(4.0);
                                if let Some(next) =
                                    pivot_table_ui(ui, pivot, p_row, p_col, p_metric, lang)
                                {
                                    actions.filter_action = Some(next);
                                }
                            }
                        }
                        _ => loading_block(ui),
                    }
                }
                AnalyticsView::Report => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(report_title(lang)).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    input.report_ready,
                                    egui::Button::new(format!(
                                        "\u{29C9} {}",
                                        report_copy_label(lang)
                                    )),
                                )
                                .clicked()
                            {
                                actions.copy_report = true;
                            }
                            if ui
                                .add_enabled(
                                    input.report_ready,
                                    egui::Button::new(report_export_label(lang)),
                                )
                                .clicked()
                            {
                                actions.export_report = true;
                            }
                        });
                    });
                    ui.label(egui::RichText::new(report_hint(lang)).weak().small());
                    ui.add_space(8.0);
                    if !input.report_ready {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(t.searching);
                        });
                    } else {
                        report_ui(ui, analytics, input.active_query, lang);
                    }
                }
                AnalyticsView::Compare => {
                    ui.label(egui::RichText::new(compare_hint(lang)).weak().small());
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(compare_text_label(lang)).strong());
                        ui.add(egui::TextEdit::singleline(&mut compare_text).desired_width(220.0));
                        ui.label(egui::RichText::new(t.year).strong());
                        ui.add(egui::TextEdit::singleline(&mut compare_year).desired_width(80.0));
                        if ui.button(compare_previous_year_label(lang)).clicked() {
                            if let Ok(year) = input.active_query.filters.year.trim().parse::<i32>()
                            {
                                compare_year = (year - 1).to_string();
                            }
                            if compare_text.trim().is_empty() {
                                compare_text = input.active_query.text.clone();
                            }
                        }
                        if ui.button(compare_run_label(lang)).clicked() {
                            actions.run_compare = true;
                        }
                    });
                    ui.add_space(8.0);
                    if input.compare_loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(t.searching);
                        });
                    }
                    match (input.compare_analytics, input.compare_query) {
                        (Some(other), Some(other_query)) => {
                            compare_ui(ui, analytics, other, input.active_query, other_query, lang);
                        }
                        _ if !input.compare_loading => {
                            ui.label(egui::RichText::new(compare_empty(lang)).weak());
                        }
                        _ => {}
                    }
                }
            }
            ui.add_space(8.0);
        });

        if compare_text != input.compare_text {
            actions.compare_text = Some(compare_text);
        }
        if compare_year != input.compare_year {
            actions.compare_year = Some(compare_year);
        }
    });
    actions
}

fn overview_panel(
    ui: &mut egui::Ui,
    analytics: &Analytics,
    month_metric: MonthMetric,
    lang: Lang,
    t: &'static Tr,
    actions: &mut AnalyticsViewActions,
) {
    ui.label(egui::RichText::new(t.analytics_scope_note).weak().small());
    ui.add_space(6.0);
    overview_story_cards(ui, &analytics.overview, lang);
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        kpi_tile(
            ui,
            t.rows_label,
            group_digits(analytics.overview.row_count),
            t.rows_help,
        );
        kpi_tile(
            ui,
            t.declarations_label,
            group_digits(analytics.overview.declaration_count),
            t.declarations_help,
        );
        kpi_tile(
            ui,
            t.recipients_label,
            group_digits(analytics.overview.distinct_recipients),
            t.recipients_help,
        );
        kpi_tile(
            ui,
            t.sender,
            group_digits(analytics.overview.distinct_senders),
            overview_senders_help(lang),
        );
        kpi_tile(
            ui,
            t.edrpou,
            group_digits(analytics.overview.distinct_edrpou),
            overview_edrpou_help(lang),
        );
        kpi_tile(
            ui,
            t.total_value,
            fmt_money_compact(&analytics.overview.measures, lang),
            t.total_value_help,
        );
        kpi_tile(
            ui,
            t.net_weight,
            format!("{} kg", fmt_compact(analytics.overview.total_net_kg)),
            t.net_weight_help,
        );
        kpi_tile(
            ui,
            t.gross_weight,
            format!("{} kg", fmt_compact(analytics.overview.total_gross_kg)),
            overview_gross_help(lang),
        );
        kpi_tile(
            ui,
            t.quantity,
            fmt_compact(analytics.overview.total_quantity),
            overview_quantity_help(lang),
        );
        kpi_tile(
            ui,
            t.avg_value_kg,
            fmt_money_per_kg(&analytics.overview.measures, lang),
            t.avg_value_kg_help,
        );
        kpi_tile(
            ui,
            t.product_codes_count,
            group_digits(analytics.overview.distinct_product_codes),
            t.product_codes_help,
        );
        kpi_tile(
            ui,
            t.trademark,
            group_digits(analytics.overview.distinct_trademarks),
            overview_trademarks_help(lang),
        );
        kpi_tile(
            ui,
            overview_origin_countries_label(lang),
            group_digits(analytics.overview.distinct_origin_countries),
            t.countries_help,
        );
        kpi_tile(
            ui,
            overview_dispatch_countries_label(lang),
            group_digits(analytics.overview.distinct_dispatch_countries),
            overview_dispatch_countries_help(lang),
        );
        kpi_tile(
            ui,
            overview_trade_countries_label(lang),
            group_digits(analytics.overview.distinct_trade_countries),
            overview_trade_countries_help(lang),
        );
    });
    ui.add_space(12.0);
    if !analytics.months.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(t.months_section).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (metric, label) in [
                    (MonthMetric::AvgPrice, t.metric_price),
                    (MonthMetric::NetWeight, t.metric_weight),
                    (MonthMetric::Rows, t.metric_rows),
                    (MonthMetric::Value, t.metric_value),
                ] {
                    if ui.selectable_label(month_metric == metric, label).clicked() {
                        actions.new_metric = Some(metric);
                    }
                }
            });
        });
        ui.label(egui::RichText::new(t.months_hint).weak().small());
        ui.add_space(2.0);
        months_chart(ui, &analytics.months, month_metric, lang);
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(t.currency_note).weak().small());
}

fn apply_card_action(actions: &mut AnalyticsViewActions, action: AnalyticsCardAction) {
    match action {
        AnalyticsCardAction::Filter(filter) => actions.filter_action = Some(filter),
        AnalyticsCardAction::Explore(kind) => actions.explore_kind = Some(kind),
    }
}

fn loading_block(ui: &mut egui::Ui) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.spinner();
    });
}
