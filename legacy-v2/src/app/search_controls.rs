use super::advanced_query::{
    AdvancedChipAction, add_advanced_condition, apply_advanced_chip_action, ensure_advanced_root,
    filter_field, flat_filter_chips, ui_query_group,
};
use super::columns::{
    ColumnPreset, column_group, column_in_preset, field_glossary, field_matches_filter,
};
use super::ui_text::expr_label_for_ui;
use crate::db::Filters;
use crate::i18n::Tr;
use crate::search::{FieldInfo, QueryExpr, QueryGroup, default_condition_for_field};

/// Renders the simple structured filters and returns true when a search should start.
pub(super) fn filters_ui(ui: &mut egui::Ui, filters: &mut Filters, t: &Tr) -> bool {
    let mut search = false;
    ui.horizontal_wrapped(|ui| {
        filter_field(ui, t.year, &mut filters.year, 60.0, "2024", &mut search);
        filter_field(
            ui,
            t.product_code,
            &mut filters.product_code,
            110.0,
            "SKU-42",
            &mut search,
        );
        filter_field(
            ui,
            t.edrpou,
            &mut filters.edrpou,
            100.0,
            "Company ID",
            &mut search,
        );
        filter_field(
            ui,
            t.trademark,
            &mut filters.trademark,
            120.0,
            "Brand",
            &mut search,
        );
        filter_field(
            ui,
            t.sender,
            &mut filters.sender,
            180.0,
            "Company",
            &mut search,
        );
        filter_field(
            ui,
            t.recipient,
            &mut filters.recipient,
            180.0,
            "Importer",
            &mut search,
        );
        filter_field(
            ui,
            t.description,
            &mut filters.description,
            180.0,
            "phones",
            &mut search,
        );
        filter_field(
            ui,
            t.trade_country,
            &mut filters.trade_country,
            80.0,
            "CN",
            &mut search,
        );
        filter_field(
            ui,
            t.dispatch_country,
            &mut filters.dispatch_country,
            80.0,
            "PL",
            &mut search,
        );
        filter_field(
            ui,
            t.origin_country,
            &mut filters.origin_country,
            80.0,
            "CN",
            &mut search,
        );
        ui.vertical(|ui| {
            ui.label(" ");
            if ui.button(t.clear_filters).clicked() {
                filters.clear();
                search = true;
            }
        });
    });
    search
}

/// Renders the result-column selector menu. Returns true when visibility changed
/// and the caller should persist the hidden-column metadata.
pub(super) fn columns_menu_ui(
    ui: &mut egui::Ui,
    result_fields: &[FieldInfo],
    visible_cols: &mut [bool],
    columns_filter: &mut String,
) -> bool {
    ui.set_min_width(430.0);
    ui.horizontal(|ui| {
        ui.strong("Visible columns");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} / {}",
                    visible_cols.iter().filter(|visible| **visible).count(),
                    result_fields.len()
                ))
                .weak()
                .small(),
            );
        });
    });
    ui.add_space(6.0);
    ui.add(
        egui::TextEdit::singleline(columns_filter)
            .desired_width(f32::INFINITY)
            .hint_text("Search columns, abbreviations, prices, weights..."),
    );
    ui.add_space(6.0);

    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (label, preset) in [
            ("Basic", ColumnPreset::Basic),
            ("Companies", ColumnPreset::Companies),
            ("Goods", ColumnPreset::Goods),
            ("Prices", ColumnPreset::Prices),
            ("Logistics", ColumnPreset::Logistics),
        ] {
            if ui.small_button(label).clicked() {
                apply_column_preset(result_fields, visible_cols, preset);
                changed = true;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("All").clicked() {
            set_all_columns(visible_cols, true);
            changed = true;
        }
        if ui.small_button("Hide all").clicked() {
            set_all_columns(visible_cols, false);
            changed = true;
        }
        if ui.small_button("Reset").clicked() {
            apply_column_preset(result_fields, visible_cols, ColumnPreset::Basic);
            changed = true;
        }
        if ui.small_button("Clear search").clicked() {
            columns_filter.clear();
        }
    });
    ui.separator();

    let needle = columns_filter.trim().to_lowercase();
    let mut current_group = "";
    egui::ScrollArea::vertical()
        .max_height(470.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, field) in result_fields.iter().enumerate() {
                if !field_matches_filter(field, &needle) {
                    continue;
                }
                let group = column_group(field);
                if group != current_group {
                    if !current_group.is_empty() {
                        ui.add_space(4.0);
                    }
                    current_group = group;
                    ui.label(egui::RichText::new(group).strong().small());
                }
                let mut visible = visible_cols.get(i).copied().unwrap_or(true);
                let response = ui.checkbox(&mut visible, &field.label);
                let response = match field_glossary(field) {
                    Some(glossary) => response.on_hover_text(glossary),
                    None => response,
                };
                if response.changed() {
                    let visible_count = visible_cols.iter().filter(|item| **item).count();
                    if (visible || visible_count > 1)
                        && let Some(slot) = visible_cols.get_mut(i)
                    {
                        *slot = visible;
                        changed = true;
                    }
                }
            }
        });
    changed
}

fn set_all_columns(visible_cols: &mut [bool], visible: bool) {
    for item in visible_cols.iter_mut() {
        *item = visible;
    }
    if !visible && !visible_cols.is_empty() {
        visible_cols[0] = true;
    }
}

fn apply_column_preset(
    result_fields: &[FieldInfo],
    visible_cols: &mut [bool],
    preset: ColumnPreset,
) {
    for (field, visible) in result_fields.iter().zip(visible_cols.iter_mut()) {
        *visible = column_in_preset(field, preset);
    }
    if !visible_cols.iter().any(|visible| *visible) && !visible_cols.is_empty() {
        visible_cols[0] = true;
    }
}

pub(super) struct V2SearchControls<'a> {
    pub(super) filters: &'a mut Filters,
    pub(super) advanced_query: &'a mut Option<QueryExpr>,
    pub(super) search_fields: &'a [FieldInfo],
    pub(super) add_filter_search: &'a mut String,
    pub(super) advanced_field_search: &'a mut String,
    pub(super) show_filters: &'a mut bool,
    pub(super) show_advanced_search: &'a mut bool,
    pub(super) t: &'a Tr,
}

pub(super) fn v2_search_ui(ui: &mut egui::Ui, controls: V2SearchControls<'_>) -> bool {
    let V2SearchControls {
        filters,
        advanced_query,
        search_fields,
        add_filter_search,
        advanced_field_search,
        show_filters,
        show_advanced_search,
        t,
    } = controls;
    let mut search = false;
    let catalog = search_fields.to_vec();
    ui.horizontal_wrapped(|ui| {
        ui.menu_button(t.v2_add_filter, |ui| {
            ui.set_min_width(360.0);
            ui.strong(t.v2_add_filter);
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::singleline(add_filter_search)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search field, code, price, country..."),
            );
            ui.add_space(6.0);
            let needle = add_filter_search.trim().to_lowercase();
            let mut current_group = "";
            egui::ScrollArea::vertical()
                .max_height(430.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for field in &catalog {
                        if !field_matches_filter(field, &needle) {
                            continue;
                        }
                        let group = column_group(field);
                        if group != current_group {
                            if !current_group.is_empty() {
                                ui.add_space(4.0);
                            }
                            current_group = group;
                            ui.label(egui::RichText::new(group).strong().small());
                        }
                        let response = ui.button(&field.label);
                        let response = match field_glossary(field) {
                            Some(glossary) => response.on_hover_text(glossary),
                            None => response,
                        };
                        if response.clicked() {
                            add_advanced_condition(
                                advanced_query,
                                default_condition_for_field(field),
                            );
                            *show_advanced_search = true;
                            add_filter_search.clear();
                            search = true;
                            ui.close();
                        }
                    }
                });
            if ui.small_button("Clear search").clicked() {
                add_filter_search.clear();
            }
        });
        let advanced = advanced_query.as_ref().is_some_and(|expr| !expr.is_empty());
        let advanced_btn = ui.selectable_label(*show_advanced_search, t.v2_advanced);
        if advanced_btn.clicked() {
            *show_advanced_search = !*show_advanced_search;
            if *show_advanced_search && advanced_query.is_none() {
                *advanced_query = Some(QueryExpr::Group(QueryGroup::default()));
            }
        }
        if advanced && ui.small_button(t.v2_clear_advanced).clicked() {
            *advanced_query = None;
            search = true;
        }
        search |= filter_chips_ui(
            ui,
            filters,
            advanced_query,
            show_filters,
            show_advanced_search,
            &catalog,
            t,
        );
    });

    if *show_advanced_search {
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(t.v2_advanced_search);
            ui.label(egui::RichText::new(t.v2_logic_hint).weak());
        });
        ui.add_space(4.0);
        ensure_advanced_root(advanced_query);
        if let Some(QueryExpr::Group(group)) = advanced_query {
            search |= ui_query_group(ui, group, &catalog, "root", true, advanced_field_search, t);
        }
        ui.add_space(6.0);
        ui.separator();
    }

    if advanced_query.as_ref().is_some_and(QueryExpr::is_empty) && !*show_advanced_search {
        *advanced_query = None;
    }
    search
}

fn filter_chips_ui(
    ui: &mut egui::Ui,
    filters: &mut Filters,
    advanced_query: &mut Option<QueryExpr>,
    show_filters: &mut bool,
    show_advanced_search: &mut bool,
    catalog: &[FieldInfo],
    t: &Tr,
) -> bool {
    let mut search = false;
    for (label, value, clear) in flat_filter_chips(filters, t) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                if ui.small_button("\u{00D7}").clicked() {
                    clear(filters);
                    search = true;
                }
                let response = ui.button(format!("{label}: {value}"));
                if response.clicked() {
                    *show_filters = true;
                }
                response.on_hover_text(t.v2_edit_in_filters);
            });
        });
    }
    let mut action = None;
    if let Some(QueryExpr::Group(group)) = advanced_query {
        for (idx, child) in group.children.iter().enumerate() {
            if child.is_empty() {
                continue;
            }
            let label = expr_label_for_ui(child, catalog, t);
            ui.menu_button(label, |ui| {
                if ui.button(t.v2_edit).clicked() {
                    *show_advanced_search = true;
                    ui.close();
                }
                if ui.button(t.v2_duplicate).clicked() {
                    action = Some(AdvancedChipAction::Duplicate(idx));
                    ui.close();
                }
                if ui.button(t.v2_toggle_not).clicked() {
                    action = Some(AdvancedChipAction::ToggleNot(idx));
                    ui.close();
                }
                if ui.button(t.v2_remove).clicked() {
                    action = Some(AdvancedChipAction::Remove(idx));
                    ui.close();
                }
            });
        }
    } else if let Some(expr) = advanced_query {
        let label = expr_label_for_ui(expr, catalog, t);
        ui.menu_button(label, |ui| {
            if ui.button(t.v2_edit).clicked() {
                *show_advanced_search = true;
                ui.close();
            }
            if ui.button(t.v2_toggle_not).clicked() {
                action = Some(AdvancedChipAction::ToggleNot(0));
                ui.close();
            }
            if ui.button(t.v2_remove).clicked() {
                action = Some(AdvancedChipAction::Remove(0));
                ui.close();
            }
        });
    }
    if let Some(action) = action {
        apply_advanced_chip_action(advanced_query, action);
        search = true;
    }
    search
}
