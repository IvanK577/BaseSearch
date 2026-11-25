use crate::db::Filters;
use crate::i18n::Tr;
use crate::search::{
    ConditionValue, FieldInfo, LogicOp, QueryCondition, QueryExpr, QueryGroup,
    default_condition_for_field, default_value_for_op, ensure_value_matches_operator, field_label,
};

use super::columns::{column_group, field_glossary, field_matches_filter};
use super::ui_text::{condition_op_label, group_label_for_ui, logic_op_label};

pub(super) enum AdvancedChipAction {
    Duplicate(usize),
    ToggleNot(usize),
    Remove(usize),
}

enum AdvancedTreeAction {
    Duplicate,
    Remove,
}

pub(super) type FilterClear = fn(&mut Filters);

pub(super) fn flat_filter_chips(
    filters: &Filters,
    t: &Tr,
) -> Vec<(&'static str, String, FilterClear)> {
    let mut chips = Vec::new();
    push_filter_chip(&mut chips, t.year, &filters.year, clear_filter_year);
    push_filter_chip(
        &mut chips,
        t.product_code,
        &filters.product_code,
        clear_filter_product_code,
    );
    push_filter_chip(&mut chips, t.edrpou, &filters.edrpou, clear_filter_edrpou);
    push_filter_chip(
        &mut chips,
        t.trademark,
        &filters.trademark,
        clear_filter_trademark,
    );
    push_filter_chip(&mut chips, t.sender, &filters.sender, clear_filter_sender);
    push_filter_chip(
        &mut chips,
        t.recipient,
        &filters.recipient,
        clear_filter_recipient,
    );
    push_filter_chip(
        &mut chips,
        t.description,
        &filters.description,
        clear_filter_description,
    );
    push_filter_chip(
        &mut chips,
        t.trade_country,
        &filters.trade_country,
        clear_filter_trade_country,
    );
    push_filter_chip(
        &mut chips,
        t.dispatch_country,
        &filters.dispatch_country,
        clear_filter_dispatch_country,
    );
    push_filter_chip(
        &mut chips,
        t.origin_country,
        &filters.origin_country,
        clear_filter_origin_country,
    );
    chips
}

fn push_filter_chip(
    chips: &mut Vec<(&'static str, String, FilterClear)>,
    label: &'static str,
    value: &str,
    clear: FilterClear,
) {
    let value = value.trim();
    if !value.is_empty() {
        chips.push((label, value.to_string(), clear));
    }
}

fn clear_filter_year(filters: &mut Filters) {
    filters.year.clear();
}

fn clear_filter_product_code(filters: &mut Filters) {
    filters.product_code.clear();
}

fn clear_filter_edrpou(filters: &mut Filters) {
    filters.edrpou.clear();
}

fn clear_filter_trademark(filters: &mut Filters) {
    filters.trademark.clear();
}

fn clear_filter_sender(filters: &mut Filters) {
    filters.sender.clear();
}

fn clear_filter_recipient(filters: &mut Filters) {
    filters.recipient.clear();
}

fn clear_filter_description(filters: &mut Filters) {
    filters.description.clear();
}

fn clear_filter_trade_country(filters: &mut Filters) {
    filters.trade_country.clear();
}

fn clear_filter_dispatch_country(filters: &mut Filters) {
    filters.dispatch_country.clear();
}

fn clear_filter_origin_country(filters: &mut Filters) {
    filters.origin_country.clear();
}

pub(super) fn add_advanced_condition(query: &mut Option<QueryExpr>, condition: QueryCondition) {
    ensure_advanced_root(query);
    if let Some(QueryExpr::Group(group)) = query {
        group.children.push(QueryExpr::Condition(condition));
    }
}

pub(super) fn ensure_advanced_root(query: &mut Option<QueryExpr>) {
    let next = match query.take() {
        Some(QueryExpr::Group(group)) => QueryExpr::Group(group),
        Some(expr) => QueryExpr::Group(QueryGroup {
            op: LogicOp::And,
            negated: false,
            children: vec![expr],
        }),
        None => QueryExpr::Group(QueryGroup::default()),
    };
    *query = Some(next);
}

pub(super) fn apply_advanced_chip_action(
    query: &mut Option<QueryExpr>,
    action: AdvancedChipAction,
) {
    ensure_advanced_root(query);
    let Some(QueryExpr::Group(group)) = query else {
        return;
    };
    match action {
        AdvancedChipAction::Duplicate(index) => {
            if let Some(expr) = group.children.get(index).cloned() {
                group.children.insert(index + 1, expr);
            }
        }
        AdvancedChipAction::ToggleNot(index) => {
            if let Some(expr) = group.children.get_mut(index) {
                toggle_expr_not(expr);
            }
        }
        AdvancedChipAction::Remove(index) => {
            if index < group.children.len() {
                group.children.remove(index);
            }
        }
    }
}

fn toggle_expr_not(expr: &mut QueryExpr) {
    match expr {
        QueryExpr::Group(group) => group.negated = !group.negated,
        QueryExpr::Condition(condition) => condition.negated = !condition.negated,
    }
}

pub(super) fn ui_query_group(
    ui: &mut egui::Ui,
    group: &mut QueryGroup,
    catalog: &[FieldInfo],
    id: &str,
    is_root: bool,
    field_search: &mut String,
    t: &Tr,
) -> bool {
    let mut search = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(t.v2_match).weak());
        egui::ComboBox::from_id_salt(format!("{id}-logic"))
            .selected_text(logic_op_label(group.op, t))
            .width(135.0)
            .show_ui(ui, |ui| {
                search |= ui
                    .selectable_value(&mut group.op, LogicOp::And, t.v2_match_all)
                    .changed();
                search |= ui
                    .selectable_value(&mut group.op, LogicOp::Or, t.v2_match_any)
                    .changed();
            });
        search |= ui
            .checkbox(&mut group.negated, t.v2_exclude_group)
            .changed();
        ui.menu_button(t.v2_add_condition, |ui| {
            ui.set_min_width(360.0);
            ui.add(
                egui::TextEdit::singleline(field_search)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search field..."),
            );
            ui.add_space(6.0);
            let needle = field_search.trim().to_lowercase();
            let mut current_group = "";
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for field in catalog {
                        if !field_matches_filter(field, &needle) {
                            continue;
                        }
                        let group_name = column_group(field);
                        if group_name != current_group {
                            if !current_group.is_empty() {
                                ui.add_space(4.0);
                            }
                            current_group = group_name;
                            ui.label(egui::RichText::new(group_name).strong().small());
                        }
                        let response = ui.button(&field.label);
                        let response = match field_glossary(field) {
                            Some(glossary) => response.on_hover_text(glossary),
                            None => response,
                        };
                        if response.clicked() {
                            group
                                .children
                                .push(QueryExpr::Condition(default_condition_for_field(field)));
                            field_search.clear();
                            search = true;
                            ui.close();
                        }
                    }
                });
        });
        ui.menu_button(t.v2_add_group, |ui| {
            if ui.button(t.v2_add_and_group).clicked() {
                group.children.push(QueryExpr::Group(QueryGroup {
                    op: LogicOp::And,
                    negated: false,
                    children: Vec::new(),
                }));
                search = true;
                ui.close();
            }
            if ui.button(t.v2_add_or_group).clicked() {
                group.children.push(QueryExpr::Group(QueryGroup {
                    op: LogicOp::Or,
                    negated: false,
                    children: Vec::new(),
                }));
                search = true;
                ui.close();
            }
        });
        if is_root && ui.small_button(t.v2_clear_group).clicked() {
            group.children.clear();
            search = true;
        }
    });

    let mut action: Option<(usize, AdvancedTreeAction)> = None;
    for index in 0..group.children.len() {
        ui.push_id(format!("{id}-{index}"), |ui| {
            ui.indent("child", |ui| match &mut group.children[index] {
                QueryExpr::Group(child_group) => {
                    ui.horizontal(|ui| {
                        let mut label = group_label_for_ui(child_group.op, t);
                        if child_group.negated {
                            label = format!("{}: {label}", t.v2_excluding);
                        }
                        ui.label(egui::RichText::new(label).strong());
                        ui.menu_button(t.v2_more, |ui| {
                            if ui.button(t.v2_duplicate).clicked() {
                                action = Some((index, AdvancedTreeAction::Duplicate));
                                ui.close();
                            }
                            if ui.button(t.v2_remove).clicked() {
                                action = Some((index, AdvancedTreeAction::Remove));
                                ui.close();
                            }
                        });
                    });
                    search |= ui_query_group(
                        ui,
                        child_group,
                        catalog,
                        &format!("{id}-{index}"),
                        false,
                        field_search,
                        t,
                    );
                }
                QueryExpr::Condition(condition) => {
                    let child_action =
                        ui_query_condition(ui, condition, catalog, id, field_search, t);
                    search |= child_action.0;
                    if let Some(action_kind) = child_action.1 {
                        action = Some((index, action_kind));
                    }
                }
            });
        });
    }
    if let Some((index, action_kind)) = action {
        match action_kind {
            AdvancedTreeAction::Duplicate => {
                if let Some(expr) = group.children.get(index).cloned() {
                    group.children.insert(index + 1, expr);
                    search = true;
                }
            }
            AdvancedTreeAction::Remove => {
                if index < group.children.len() {
                    group.children.remove(index);
                    search = true;
                }
            }
        }
    }
    search
}

fn ui_query_condition(
    ui: &mut egui::Ui,
    condition: &mut QueryCondition,
    catalog: &[FieldInfo],
    id: &str,
    field_search: &mut String,
    t: &Tr,
) -> (bool, Option<AdvancedTreeAction>) {
    ensure_value_matches_operator(condition);
    let mut search = false;
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        search |= ui
            .checkbox(&mut condition.negated, t.v2_exclude_rule)
            .changed();
        let field_id = condition.field.id();
        ui.menu_button(field_label(&condition.field, catalog), |ui| {
            ui.set_min_width(360.0);
            ui.add(
                egui::TextEdit::singleline(field_search)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search field..."),
            );
            ui.add_space(6.0);
            let needle = field_search.trim().to_lowercase();
            let mut current_group = "";
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for field in catalog {
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
                        let response = ui.selectable_label(field.id == field_id, &field.label);
                        let response = match field_glossary(field) {
                            Some(glossary) => response.on_hover_text(glossary),
                            None => response,
                        };
                        if response.clicked() {
                            *condition = default_condition_for_field(field);
                            field_search.clear();
                            search = true;
                            ui.close();
                        }
                    }
                });
        });
        let ops = catalog
            .iter()
            .find(|field| field.id == condition.field.id())
            .map(|field| field.operators.clone())
            .unwrap_or_else(|| vec![condition.op]);
        egui::ComboBox::from_id_salt(format!("{id}-op-{}", condition.field.id()))
            .selected_text(condition_op_label(condition.op, t))
            .width(120.0)
            .show_ui(ui, |ui| {
                for op in ops {
                    if ui
                        .selectable_value(&mut condition.op, op, condition_op_label(op, t))
                        .changed()
                    {
                        condition.value = default_value_for_op(op);
                        search = true;
                    }
                }
            });
        search |= ui_condition_value(ui, condition, id, t);
        ui.menu_button(t.v2_more, |ui| {
            if ui.button(t.v2_duplicate).clicked() {
                action = Some(AdvancedTreeAction::Duplicate);
                ui.close();
            }
            if ui.button(t.v2_remove).clicked() {
                action = Some(AdvancedTreeAction::Remove);
                ui.close();
            }
        });
    });
    (search, action)
}

fn ui_condition_value(ui: &mut egui::Ui, condition: &mut QueryCondition, id: &str, t: &Tr) -> bool {
    match &mut condition.value {
        ConditionValue::None => {
            ui.label(egui::RichText::new(t.v2_no_value).weak());
            false
        }
        ConditionValue::Single(value) => {
            let response = ui.add(
                egui::TextEdit::singleline(value)
                    .desired_width(170.0)
                    .hint_text(t.v2_value_hint),
            );
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
        ConditionValue::List(values) => {
            let mut raw = values.join(", ");
            let response = ui.add(
                egui::TextEdit::singleline(&mut raw)
                    .desired_width(220.0)
                    .hint_text(t.v2_list_hint),
            );
            if response.changed() {
                *values = raw
                    .split(',')
                    .map(|value| value.trim().to_string())
                    .collect();
            }
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
        ConditionValue::Range { from, to } => {
            let mut from_text = from.clone().unwrap_or_default();
            let mut to_text = to.clone().unwrap_or_default();
            let from_response = ui.add(
                egui::TextEdit::singleline(&mut from_text)
                    .desired_width(95.0)
                    .hint_text(t.v2_from_hint)
                    .id_salt(format!("{id}-from")),
            );
            ui.label("..");
            let to_response = ui.add(
                egui::TextEdit::singleline(&mut to_text)
                    .desired_width(95.0)
                    .hint_text(t.v2_to_hint)
                    .id_salt(format!("{id}-to")),
            );
            if from_response.changed() {
                *from = (!from_text.trim().is_empty()).then_some(from_text.trim().to_string());
            }
            if to_response.changed() {
                *to = (!to_text.trim().is_empty()).then_some(to_text.trim().to_string());
            }
            (from_response.lost_focus() || to_response.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
    }
}

pub(super) fn filter_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    width: f32,
    hint: &str,
    search: &mut bool,
) {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(label).small().weak());
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(width)
                .hint_text(hint),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            *search = true;
        }
    });
}
