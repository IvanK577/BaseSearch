use std::collections::HashSet;

use egui_extras::{Column, TableBuilder};

use super::ACCENT;
use super::columns::{CellKind, field_col_spec, field_glossary};
use super::ui_text::trunc_label;
use crate::i18n::{Tr, fmt};
use crate::search::FieldInfo;

/// Action from the table row context menu.
pub(super) enum RowMenuAction {
    CopyCell(String),
    CopyRow(usize),
    CopySelected,
    FilterSender(String),
    FilterRecipient(String),
    FilterCode(String),
    FilterEdrpou(String),
    OpenProfile(String),
}

type QuickAction = (&'static str, &'static str, fn(String) -> RowMenuAction);

pub(super) struct ResultsTableInput<'a> {
    pub(super) result_fields: &'a [FieldInfo],
    pub(super) visible_cols: &'a [bool],
    pub(super) rows: &'a [Vec<String>],
    pub(super) result_dups: &'a [Option<String>],
    pub(super) selected: &'a HashSet<usize>,
    pub(super) t: &'a Tr,
}

#[derive(Default)]
pub(super) struct ResultsTableActions {
    pub(super) clicked_row: Option<(usize, egui::Modifiers)>,
    pub(super) open_card: Option<usize>,
    pub(super) menu_action: Option<RowMenuAction>,
}

pub(super) fn results_table(
    ui: &mut egui::Ui,
    input: ResultsTableInput<'_>,
) -> ResultsTableActions {
    let ResultsTableInput {
        result_fields,
        visible_cols,
        rows,
        result_dups,
        selected,
        t,
    } = input;
    let visible: Vec<usize> = (0..result_fields.len())
        .filter(|i| visible_cols[*i])
        .collect();
    let modifiers = row_click_modifiers(ui);
    let dark = ui.visuals().dark_mode;
    let code_color = if dark {
        egui::Color32::from_rgb(132, 170, 255)
    } else {
        ACCENT
    };
    let dup_color = if dark {
        egui::Color32::from_rgb(235, 170, 90)
    } else {
        egui::Color32::from_rgb(160, 90, 0)
    };
    let mut actions = ResultsTableActions::default();
    let n_selected = selected.len();
    let text_height = egui::TextStyle::Body.resolve(ui.style()).size + 9.0;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .min_scrolled_height(0.0);
        for idx in &visible {
            let (width, _) = field_col_spec(&result_fields[*idx]);
            table = table.column(Column::initial(width).at_least(40.0).clip(true));
        }
        table
            .header(28.0, |mut header| {
                for idx in &visible {
                    let field = &result_fields[*idx];
                    let (_, kind) = field_col_spec(field);
                    header.col(|ui| {
                        let resp = if kind == CellKind::Number {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.strong(&field.label)
                            })
                            .inner
                        } else {
                            ui.strong(&field.label)
                        };
                        if let Some(glossary) = field_glossary(field) {
                            resp.on_hover_text(glossary);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(text_height, rows.len(), |mut row| {
                    let i = row.index();
                    row.set_selected(selected.contains(&i));
                    let dup_first = result_dups.get(i).and_then(|d| d.clone());
                    let is_dup = dup_first.is_some();
                    let mut clicked = false;
                    let mut double = false;
                    for idx in &visible {
                        let value = rows
                            .get(i)
                            .and_then(|row| row.get(*idx))
                            .map(String::as_str)
                            .unwrap_or("");
                        let (_, kind) = field_col_spec(&result_fields[*idx]);
                        let (_, response) = row.col(|ui| {
                            let rich = match kind {
                                CellKind::Normal => egui::RichText::new(value),
                                CellKind::Weak => egui::RichText::new(value).weak(),
                                CellKind::Code => {
                                    egui::RichText::new(value).monospace().color(code_color)
                                }
                                CellKind::Number => egui::RichText::new(value).monospace(),
                            };
                            let rich = if is_dup { rich.color(dup_color) } else { rich };
                            let label = egui::Label::new(rich).selectable(false).truncate();
                            if kind == CellKind::Number {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.add(label);
                                    },
                                );
                            } else {
                                ui.add(label);
                            }
                        });
                        let response = if let Some(first_file) = &dup_first {
                            response.on_hover_text(fmt(t.dup_first_seen, &[first_file]))
                        } else {
                            response
                        };
                        clicked |= response.clicked();
                        double |= response.double_clicked();
                        response.context_menu(|ui| {
                            if let Some(action) =
                                row_context_menu(ui, rows, result_fields, i, value, n_selected, t)
                            {
                                actions.menu_action = Some(action);
                            }
                        });
                    }
                    if double {
                        actions.open_card = Some(i);
                    } else if clicked {
                        actions.clicked_row = Some((i, modifiers));
                    }
                });
            });
    });
    actions
}

fn row_click_modifiers(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|e| match e {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    })
}

fn row_context_menu(
    ui: &mut egui::Ui,
    rows: &[Vec<String>],
    result_fields: &[FieldInfo],
    row_index: usize,
    value: &str,
    n_selected: usize,
    t: &Tr,
) -> Option<RowMenuAction> {
    let cells = &rows[row_index];
    if n_selected > 1
        && ui
            .button(fmt(t.copy_selected, &[&n_selected.to_string()]))
            .clicked()
    {
        ui.close();
        return Some(RowMenuAction::CopySelected);
    }
    if ui.button(t.copy_value).clicked() {
        ui.close();
        return Some(RowMenuAction::CopyCell(value.to_string()));
    }
    if ui.button(t.copy_row).clicked() {
        ui.close();
        return Some(RowMenuAction::CopyRow(row_index));
    }
    ui.separator();

    if let Some(col) = result_field_index(result_fields, "edrpou") {
        let edrpou = cells[col].trim();
        if !edrpou.is_empty()
            && ui
                .button(format!("\u{1F3E2} {}: {}", t.open_profile, edrpou))
                .clicked()
        {
            ui.close();
            return Some(RowMenuAction::OpenProfile(edrpou.to_string()));
        }
    }

    let quick: [QuickAction; 4] = [
        (t.flt_sender, "sender", RowMenuAction::FilterSender),
        (t.flt_recipient, "recipient", RowMenuAction::FilterRecipient),
        (t.flt_code, "product_code", RowMenuAction::FilterCode),
        (t.flt_edrpou, "edrpou", RowMenuAction::FilterEdrpou),
    ];
    for (label, column, make) in quick {
        let Some(col) = result_field_index(result_fields, column) else {
            continue;
        };
        let cell = cells[col].trim();
        if cell.is_empty() {
            continue;
        }
        let text = format!("{label}: {}", trunc_label(cell, 24));
        if ui.button(text).clicked() {
            ui.close();
            return Some(make(cell.to_string()));
        }
    }
    None
}

fn result_field_index(fields: &[FieldInfo], id: &str) -> Option<usize> {
    fields.iter().position(|field| field.id == id)
}
