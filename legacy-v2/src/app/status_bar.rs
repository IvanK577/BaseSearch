use super::state::{OpKind, OpState, StatusLine};
use crate::i18n::{Tr, fmt, group_digits};
use crate::import::ImportPhase;
use crate::workers::PAGE_SIZE;

pub(super) struct StatusBarInput<'a> {
    pub(super) op: Option<&'a OpState>,
    pub(super) search_in_flight: bool,
    pub(super) count_in_flight: bool,
    pub(super) status: &'a StatusLine,
    pub(super) last_search_ms: Option<u64>,
    pub(super) page: u64,
    pub(super) page_count: u64,
    pub(super) total: Option<u64>,
    pub(super) rows_len: usize,
    pub(super) page_has_next: bool,
    pub(super) selected_len: usize,
    pub(super) t: &'a Tr,
}

#[derive(Default)]
pub(super) struct StatusBarAction {
    pub(super) cancel_operation: bool,
    pub(super) goto_page: Option<u64>,
}

pub(super) fn status_bar_panel(root: &mut egui::Ui, input: StatusBarInput<'_>) -> StatusBarAction {
    let mut action = StatusBarAction::default();
    egui::Panel::bottom("status").show_inside(root, |ui| {
        ui.add_space(4.0);
        if let Some(op) = input.op
            && progress_ui(ui, op, input.t)
        {
            action.cancel_operation = true;
        }
        if input.op.is_some() {
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            status_text_ui(ui, &input);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                action.goto_page = pagination_ui(ui, &input);
            });
        });
        ui.add_space(4.0);
    });
    action
}

fn status_text_ui(ui: &mut egui::Ui, input: &StatusBarInput<'_>) {
    if input.search_in_flight {
        ui.spinner();
        ui.label(input.t.searching);
    } else if !input.status.text.is_empty() {
        let color = if input.status.is_error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().text_color()
        };
        ui.colored_label(color, &input.status.text);
    } else if let Some(ms) = input.last_search_ms {
        result_range_ui(ui, input, ms);
    }
}

fn result_range_ui(ui: &mut egui::Ui, input: &StatusBarInput<'_>, ms: u64) {
    let start = input.page * PAGE_SIZE + 1;
    let end = input.page * PAGE_SIZE + input.rows_len as u64;
    if let Some(total) = input.total {
        if total == 0 {
            return;
        }
        let mut text = fmt(
            input.t.rows_of,
            &[
                &group_digits(start),
                &group_digits(end.min(total)),
                &group_digits(total),
            ],
        );
        append_search_timing(&mut text, input, ms);
        ui.label(text);
    } else if input.rows_len > 0 {
        let mut text = fmt(
            input.t.rows_of,
            &[&group_digits(start), &group_digits(end), "?"],
        );
        append_search_timing(&mut text, input, ms);
        if input.count_in_flight {
            text.push_str("  \u{00B7}  ");
            text.push_str(input.t.searching);
        }
        ui.label(text);
    }
}

fn append_search_timing(text: &mut String, input: &StatusBarInput<'_>, ms: u64) {
    text.push_str("  \u{00B7}  ");
    text.push_str(&fmt(input.t.search_ms, &[&ms.to_string()]));
    if input.selected_len > 1 {
        text.push_str("  \u{00B7}  ");
        text.push_str(&fmt(input.t.selected_n, &[&input.selected_len.to_string()]));
    }
}

fn progress_ui(ui: &mut egui::Ui, op: &OpState, t: &Tr) -> bool {
    let mut cancel_clicked = false;
    ui.horizontal(|ui| {
        match op.kind {
            OpKind::Maintenance => {
                ui.spinner();
                ui.label("Optimizing database...");
            }
            OpKind::Clear => {
                ui.spinner();
                ui.label(t.clearing);
            }
            OpKind::Export => {
                let (done, total) = op.export_progress;
                ui.label(t.exporting);
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(ui.available_width() - 110.0)
                        .text(format!("{} / {}", group_digits(done), group_digits(total))),
                );
            }
            OpKind::Import => import_progress_ui(ui, op, t),
        }
        if !matches!(op.kind, OpKind::Clear | OpKind::Maintenance) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t.cancel).clicked() {
                    cancel_clicked = true;
                }
            });
        }
    });
    cancel_clicked
}

fn import_progress_ui(ui: &mut egui::Ui, op: &OpState, t: &Tr) {
    if let Some(ev) = &op.last_event {
        let phase = match ev.phase {
            ImportPhase::Reading => t.reading_file,
            ImportPhase::Inserting => t.writing_rows,
            ImportPhase::Indexing => t.indexing,
        };
        let label = if ev.file_name.is_empty() {
            phase.to_string()
        } else {
            format!(
                "{} \u{00B7} {}",
                fmt(
                    t.file_of,
                    &[
                        &ev.file_idx.to_string(),
                        &ev.file_count.to_string(),
                        &ev.file_name,
                    ],
                ),
                phase
            )
        };
        ui.label(label);
        if ev.total > 0 {
            let frac = ev.done as f32 / ev.total as f32;
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(ui.available_width() - 110.0)
                    .text(format!(
                        "{} / {}",
                        group_digits(ev.done),
                        group_digits(ev.total)
                    )),
            );
        } else {
            ui.spinner();
            if ev.done > 0 {
                ui.label(group_digits(ev.done));
            }
        }
    } else {
        ui.spinner();
        ui.label(t.reading_file);
    }
}

fn pagination_ui(ui: &mut egui::Ui, input: &StatusBarInput<'_>) -> Option<u64> {
    let page = input.page;
    let pages = input.page_count;
    let can_go_next = input
        .total
        .map(|_| page + 1 < pages)
        .unwrap_or(!input.search_in_flight && input.page_has_next);
    let mut goto: Option<u64> = None;
    if ui
        .add_enabled(
            input.total.is_some() && page + 1 < pages,
            egui::Button::new("\u{23ED}"),
        )
        .clicked()
    {
        goto = Some(pages - 1);
    }
    if ui
        .add_enabled(can_go_next, egui::Button::new("\u{25B6}"))
        .clicked()
    {
        goto = Some(page + 1);
    }
    let page_total = input
        .total
        .map(|_| group_digits(pages))
        .unwrap_or_else(|| "?".to_string());
    ui.label(format!("{} / {}", group_digits(page + 1), page_total));
    if ui
        .add_enabled(page > 0, egui::Button::new("\u{25C0}"))
        .clicked()
    {
        goto = Some(page - 1);
    }
    if ui
        .add_enabled(page > 0, egui::Button::new("\u{23EE}"))
        .clicked()
    {
        goto = Some(0);
    }
    goto
}
