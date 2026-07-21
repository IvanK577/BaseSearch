use super::ACCENT;
use super::search_controls::{V2SearchControls, columns_menu_ui, filters_ui, v2_search_ui};
use super::state::AppTab;
use super::stored_queries::StoredQuery;
use super::ui_text::{
    GuidedQuestionAction, GuidedQuestionSection, clear_recent_queries_label,
    empty_recent_queries_label, empty_saved_queries_label, guided_question_action,
    guided_question_title, guided_questions_empty, guided_questions_for, guided_questions_hover,
    guided_questions_label, guided_section_title, query_summary, recent_queries_label,
    save_current_query_label, saved_queries_label, trunc_label,
};
use crate::db::{Filters, Query};
use crate::i18n::{Lang, Tr, fmt, group_digits};
use crate::search::{FieldInfo, QueryExpr};

pub(super) struct ToolbarInput<'a> {
    pub(super) lang: Lang,
    pub(super) t: &'a Tr,
    pub(super) db_ready: bool,
    pub(super) busy: bool,
    pub(super) db_total_rows: Option<u64>,
    pub(super) total: Option<u64>,
    pub(super) rows_empty: bool,
    pub(super) active_query_text: &'a str,
    pub(super) active_tab: &'a mut AppTab,
    pub(super) query_text: &'a mut String,
    pub(super) filters: &'a mut Filters,
    pub(super) advanced_query: &'a mut Option<QueryExpr>,
    pub(super) search_fields: &'a [FieldInfo],
    pub(super) result_fields: &'a [FieldInfo],
    pub(super) visible_cols: &'a mut [bool],
    pub(super) recent_queries: &'a [StoredQuery],
    pub(super) saved_queries: &'a [StoredQuery],
    pub(super) show_filters: &'a mut bool,
    pub(super) show_advanced_search: &'a mut bool,
    pub(super) show_settings: &'a mut bool,
    pub(super) show_help: &'a mut bool,
    pub(super) columns_filter: &'a mut String,
    pub(super) add_filter_search: &'a mut String,
    pub(super) advanced_field_search: &'a mut String,
}

#[derive(Default)]
pub(super) struct ToolbarAction {
    pub(super) search: bool,
    pub(super) import: bool,
    pub(super) export: bool,
    pub(super) request_analytics: bool,
    pub(super) apply_stored_query: Option<Query>,
    pub(super) guided_action: Option<GuidedQuestionAction>,
    pub(super) save_current_query: bool,
    pub(super) remove_saved_query: Option<usize>,
    pub(super) clear_recent_queries: bool,
    pub(super) columns_changed: bool,
}

pub(super) fn toolbar_panel(root: &mut egui::Ui, input: ToolbarInput<'_>) -> ToolbarAction {
    let ToolbarInput {
        lang,
        t,
        db_ready,
        busy,
        db_total_rows,
        total,
        rows_empty,
        active_query_text,
        active_tab,
        query_text,
        filters,
        advanced_query,
        search_fields,
        result_fields,
        visible_cols,
        recent_queries,
        saved_queries,
        show_filters,
        show_advanced_search,
        show_settings,
        show_help,
        columns_filter,
        add_filter_search,
        advanced_field_search,
    } = input;
    let ctx = root.ctx().clone();
    let guided_filters = filters.clone();
    let guided_text = query_text.clone();
    let guided_query = Query {
        text: guided_text.clone(),
        filters: guided_filters.clone(),
        advanced: advanced_query.clone(),
        record_scope: Default::default(),
    };
    let guided_items = guided_questions_for(&guided_text, &guided_filters);
    let mut action = ToolbarAction::default();
    let frame = egui::Frame::side_top_panel(&ctx.global_style()).inner_margin(egui::Margin {
        left: 12,
        right: 12,
        top: 10,
        bottom: 8,
    });
    egui::Panel::top("toolbar")
        .frame(frame)
        .show_inside(root, |ui| {
            ui.horizontal(|ui| {
                ui.heading(t.app_title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("\u{2699}").on_hover_text(t.settings).clicked() {
                        *show_settings = !*show_settings;
                        if *show_settings {
                            *show_help = false;
                        }
                    }
                    if ui
                        .button("?")
                        .on_hover_text(format!("{} (F1)", t.help))
                        .clicked()
                    {
                        *show_help = true;
                        *show_settings = false;
                    }
                    ui.separator();
                    if let Some(total) = db_total_rows {
                        ui.label(
                            egui::RichText::new(fmt(t.db_rows, &[&group_digits(total)])).weak(),
                        );
                    }
                });
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new(t.import)).clicked() {
                    action.import = true;
                }
                let can_export = !busy && (total.unwrap_or(0) > 0 || !rows_empty);
                if ui
                    .add_enabled(can_export, egui::Button::new(t.export))
                    .clicked()
                {
                    action.export = true;
                }
                ui.separator();
                if ui
                    .selectable_label(*active_tab == AppTab::Results, t.results_tab)
                    .clicked()
                {
                    *active_tab = AppTab::Results;
                }
                if ui
                    .selectable_label(*active_tab == AppTab::Analytics, t.analytics)
                    .clicked()
                {
                    action.request_analytics = *active_tab != AppTab::Analytics;
                    *active_tab = AppTab::Analytics;
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(t.columns_btn, |ui| {
                        action.columns_changed |=
                            columns_menu_ui(ui, result_fields, visible_cols, columns_filter);
                    });
                    let filters_btn = ui.selectable_label(*show_filters, t.filters);
                    if filters_btn.clicked() {
                        *show_filters = !*show_filters;
                    }
                    guided_questions_menu(
                        ui,
                        GuidedMenuInput {
                            lang,
                            t,
                            guided_query: &guided_query,
                            guided_items: &guided_items,
                            guided_text: &guided_text,
                            guided_filters: &guided_filters,
                        },
                        &mut action,
                    );
                    saved_queries_menu(ui, lang, t, saved_queries, &mut action);
                    recent_queries_menu(ui, lang, t, recent_queries, &mut action);
                    let find_btn =
                        egui::Button::new(egui::RichText::new(t.find).color(egui::Color32::WHITE))
                            .fill(ACCENT);
                    if ui.add_enabled(db_ready, find_btn).clicked() {
                        action.search = true;
                    }
                    let edit = egui::TextEdit::singleline(query_text)
                        .hint_text(t.search_hint)
                        .desired_width(ui.available_width());
                    let response = ui.add(edit);
                    if db_ready
                        && response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        action.search = true;
                    }
                    if db_ready
                        && response.changed()
                        && query_text.trim().is_empty()
                        && !active_query_text.trim().is_empty()
                    {
                        action.search = true;
                    }
                });
            });

            if *show_filters {
                ui.add_space(6.0);
                action.search |= filters_ui(ui, filters, t);
            }
            ui.add_space(6.0);
            action.search |= v2_search_ui(
                ui,
                V2SearchControls {
                    filters,
                    advanced_query,
                    search_fields,
                    add_filter_search,
                    advanced_field_search,
                    show_filters,
                    show_advanced_search,
                    t,
                },
            );
            ui.add_space(2.0);
        });
    action
}

struct GuidedMenuInput<'a> {
    lang: Lang,
    t: &'a Tr,
    guided_query: &'a Query,
    guided_items: &'a [(GuidedQuestionSection, super::ui_text::GuidedQuestionKind)],
    guided_text: &'a str,
    guided_filters: &'a Filters,
}

fn guided_questions_menu(
    ui: &mut egui::Ui,
    input: GuidedMenuInput<'_>,
    action: &mut ToolbarAction,
) {
    let GuidedMenuInput {
        lang,
        t,
        guided_query,
        guided_items,
        guided_text,
        guided_filters,
    } = input;
    let questions_resp = ui
        .menu_button(guided_questions_label(lang), |ui| {
            if guided_items.is_empty() || guided_query.is_empty() {
                ui.label(egui::RichText::new(guided_questions_empty(lang)).weak());
                ui.separator();
                ui.label(egui::RichText::new("Examples").strong().small());
                for example in [
                    "SKU-42",
                    "invoice 2024",
                    "company name",
                    "warehouse",
                    "Brand",
                ] {
                    ui.label(egui::RichText::new(example).weak().small());
                }
                return;
            }
            ui.label(
                egui::RichText::new(query_summary(guided_query, t))
                    .weak()
                    .small(),
            );
            let mut current_section: Option<GuidedQuestionSection> = None;
            for (section, kind) in guided_items {
                if current_section != Some(*section) {
                    if current_section.is_some() {
                        ui.separator();
                    }
                    current_section = Some(*section);
                    ui.label(egui::RichText::new(guided_section_title(*section, lang)).strong());
                }
                let Some(next) = guided_question_action(*kind, guided_text, guided_filters) else {
                    continue;
                };
                if ui.button(guided_question_title(*kind, lang)).clicked() {
                    action.guided_action = Some(next);
                    ui.close();
                }
            }
        })
        .response;
    questions_resp.on_hover_text(guided_questions_hover(lang));
}

fn saved_queries_menu(
    ui: &mut egui::Ui,
    lang: Lang,
    t: &Tr,
    saved_queries: &[StoredQuery],
    action: &mut ToolbarAction,
) {
    let saved_resp = ui
        .menu_button("\u{2605}", |ui| {
            if ui.button(save_current_query_label(lang)).clicked() {
                action.save_current_query = true;
                ui.close();
            }
            ui.separator();
            if saved_queries.is_empty() {
                ui.label(egui::RichText::new(empty_saved_queries_label(lang)).weak());
            } else {
                for (idx, item) in saved_queries.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .button(trunc_label(&item.name, 56))
                            .on_hover_text(query_summary(&item.query, t))
                            .clicked()
                        {
                            action.apply_stored_query = Some(item.query.clone());
                            ui.close();
                        }
                        if ui.small_button("\u{00D7}").clicked() {
                            action.remove_saved_query = Some(idx);
                            ui.close();
                        }
                    });
                }
            }
        })
        .response;
    saved_resp.on_hover_text(saved_queries_label(lang));
}

fn recent_queries_menu(
    ui: &mut egui::Ui,
    lang: Lang,
    t: &Tr,
    recent_queries: &[StoredQuery],
    action: &mut ToolbarAction,
) {
    let recent_resp = ui
        .menu_button("\u{21BA}", |ui| {
            if recent_queries.is_empty() {
                ui.label(egui::RichText::new(empty_recent_queries_label(lang)).weak());
            } else {
                for item in recent_queries {
                    if ui
                        .button(trunc_label(&item.name, 64))
                        .on_hover_text(query_summary(&item.query, t))
                        .clicked()
                    {
                        action.apply_stored_query = Some(item.query.clone());
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(clear_recent_queries_label(lang)).clicked() {
                    action.clear_recent_queries = true;
                    ui.close();
                }
            }
        })
        .response;
    recent_resp.on_hover_text(recent_queries_label(lang));
}
