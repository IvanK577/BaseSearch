use crate::i18n::{Tr, fmt, group_digits};
use crate::import::FileSummary;

pub(super) fn import_report_window(ctx: &egui::Context, report: &[FileSummary], t: &Tr) -> bool {
    let mut open = true;
    egui::Window::new(t.import_report)
        .open(&mut open)
        .default_width(560.0)
        .collapsible(false)
        .show(ctx, |ui| {
            egui::Grid::new("report_grid")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    for summary in report {
                        ui.label(egui::RichText::new(&summary.file_name).strong());
                        if let Some(err) = &summary.error {
                            ui.colored_label(ui.visuals().error_fg_color, err);
                        } else if let Some(previous) = &summary.skipped_duplicate_of {
                            ui.label(egui::RichText::new(fmt(t.file_skipped, &[previous])).weak());
                        } else {
                            let mut text = fmt(
                                t.file_result,
                                &[
                                    &group_digits(summary.imported),
                                    &group_digits(summary.duplicates),
                                    &format!("{:.1}", summary.seconds),
                                ],
                            );
                            if summary.cancelled {
                                text.push_str(" \u{00B7} ");
                                text.push_str(t.cancelled);
                            }
                            ui.vertical(|ui| {
                                ui.label(text);
                                ui.label(
                                    egui::RichText::new(import_quality_line(summary))
                                        .weak()
                                        .small(),
                                );
                                for warning in &summary.quality.warnings {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        egui::RichText::new(warning).small(),
                                    );
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    !open
}

fn import_quality_line(summary: &FileSummary) -> String {
    let quality = &summary.quality;
    if quality.layout.is_empty() {
        return "Quality: not available".to_string();
    }
    format!(
        "Quality: {} · header row {} · columns {} (recognized {}, extra {}) · filled {:.0}%",
        quality.layout,
        quality.header_row,
        group_digits(quality.source_columns),
        group_digits(quality.recognized_columns),
        group_digits(quality.extra_columns),
        quality.filled_percent()
    )
}
