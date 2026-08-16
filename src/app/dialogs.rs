use std::path::Path;
use std::time::Instant;

use super::ACCENT;
use super::state::StatusLine;
use crate::db::{RecordCard, StartupPhase};
use crate::i18n::{Lang, Tr, help_sections};

pub(super) enum ResultsEmptyAction {
    Import,
}

pub(super) enum SettingsAction {
    Persist { key: &'static str, value: String },
    CopyDbPath,
    OpenDbFolder,
    OptimizeDatabase,
    ClearDatabase,
}

pub(super) struct SettingsWindowInput<'a> {
    pub(super) show_settings: &'a mut bool,
    pub(super) lang: &'a mut Lang,
    pub(super) db_path: &'a Path,
    pub(super) busy: bool,
    pub(super) db_ready: bool,
    pub(super) t: &'a Tr,
    pub(super) app_version: &'a str,
}

/// The label for what opening the database is doing right now.
pub(super) fn startup_phase_label(phase: StartupPhase, t: &Tr) -> &'static str {
    match phase {
        StartupPhase::CheckingVersion => t.phase_checking_version,
        StartupPhase::CheckingFreeSpace => t.phase_checking_space,
        StartupPhase::CreatingBackup => t.phase_backup,
        StartupPhase::VerifyingBackup => t.phase_verify_backup,
        StartupPhase::UpgradingStructure => t.phase_upgrading,
        StartupPhase::RebuildingFingerprints { .. } => t.phase_fingerprints,
        StartupPhase::ComputingTypedColumns { .. } => t.phase_typed_columns,
        StartupPhase::VerifyingUpgrade => t.phase_verifying,
    }
}

pub(super) fn startup_state(
    ui: &mut egui::Ui,
    db_path: &Path,
    startup_started: &Instant,
    phase: Option<StartupPhase>,
    status: &StatusLine,
    t: &Tr,
) {
    ui.add_space((ui.available_height() * 0.28).max(0.0));
    ui.vertical_centered(|ui| {
        ui.spinner();
        ui.add_space(12.0);
        ui.heading(t.startup_opening);
        // A plain open says nothing beyond "this can take a moment". An
        // upgrade says which of its phases is running, because it rewrites the
        // database and the person needs to know not to close the window.
        match phase.filter(StartupPhase::is_upgrade) {
            Some(phase) => {
                ui.label(egui::RichText::new(startup_phase_label(phase, t)).strong());
                if let Some((done, total)) = phase.progress()
                    && total > 0
                {
                    ui.add_space(6.0);
                    let fraction = (done as f32 / total as f32).clamp(0.0, 1.0);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(320.0)
                            .text(format!("{done} / {total}")),
                    );
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new(t.upgrade_once_note).weak());
            }
            None => {
                ui.label(egui::RichText::new(t.startup_hint).weak());
            }
        }
        ui.add_space(12.0);
        egui::Grid::new("startup_database_state")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new(t.startup_database_label).weak());
                ui.label(db_path.display().to_string());
                ui.end_row();

                ui.label(egui::RichText::new(t.startup_size_label).weak());
                let size = std::fs::metadata(db_path)
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                ui.label(format!("{:.2} GB", size as f64 / (1u64 << 30) as f64));
                ui.end_row();

                ui.label(egui::RichText::new(t.startup_elapsed_label).weak());
                ui.label(format!("{} s", startup_started.elapsed().as_secs()));
                ui.end_row();
            });
        if status.is_error && !status.text.is_empty() {
            ui.add_space(8.0);
            ui.colored_label(ui.visuals().error_fg_color, &status.text);
        }
    });
}

pub(super) fn results_empty_state(
    ui: &mut egui::Ui,
    total: Option<u64>,
    active_query_empty: bool,
    search_in_flight: bool,
    count_in_flight: bool,
    t: &Tr,
) -> Option<ResultsEmptyAction> {
    let mut action = None;
    let empty_db = matches!(total, Some(0)) && active_query_empty;
    let text = match total {
        Some(0) if active_query_empty => t.db_empty,
        Some(0) => t.nothing_found,
        None if search_in_flight || count_in_flight => t.searching,
        _ => t.enter_query_hint,
    };
    ui.add_space((ui.available_height() * 0.30).max(0.0));
    ui.vertical_centered(|ui| {
        if search_in_flight || count_in_flight {
            ui.spinner();
        } else if empty_db {
            ui.heading("Ready for data");
        } else {
            ui.heading("No matching rows");
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new(text).size(16.0).weak());
        if empty_db {
            ui.add_space(14.0);
            if ui
                .add(
                    egui::Button::new(egui::RichText::new(t.import).color(egui::Color32::WHITE))
                        .fill(ACCENT),
                )
                .clicked()
            {
                action = Some(ResultsEmptyAction::Import);
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Data stays on this computer.")
                    .weak()
                    .small(),
            );
            ui.label(
                egui::RichText::new("Workflow: Import Excel -> Search -> Analytics -> Export.")
                    .weak()
                    .small(),
            );
        } else if matches!(total, Some(0)) {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Try a broader query, remove one filter, or open Questions for examples.",
                )
                .weak()
                .small(),
            );
        }
    });
    action
}

pub(super) fn card_window(
    ctx: &egui::Context,
    card_open: &mut bool,
    card: &mut Option<RecordCard>,
    t: &Tr,
) {
    if !*card_open {
        return;
    }
    let mut open = *card_open;
    if let Some(card) = card {
        egui::Window::new(t.details)
            .open(&mut open)
            .default_size([640.0, 660.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}: {}", t.file_col, card.source_file)).weak(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t.copy_all).clicked() {
                            let mut lines: Vec<String> = card
                                .fields
                                .iter()
                                .filter(|(_, v)| !v.is_empty())
                                .map(|(h, v)| format!("{h}: {v}"))
                                .collect();
                            lines.extend(
                                card.extra
                                    .iter()
                                    .filter(|(_, v)| !v.is_empty())
                                    .map(|(h, v)| format!("{h}: {v}")),
                            );
                            ctx.copy_text(lines.join("\n"));
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("card_grid")
                        .num_columns(2)
                        .striped(true)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            for (header, value) in &card.fields {
                                ui.label(egui::RichText::new(header.as_str()).strong());
                                if value.is_empty() {
                                    ui.label(egui::RichText::new("\u{2014}").weak());
                                } else {
                                    ui.add(egui::Label::new(value).wrap());
                                }
                                ui.end_row();
                            }
                            for (header, value) in &card.extra {
                                ui.label(egui::RichText::new(header.as_str()).strong().italics());
                                if value.is_empty() {
                                    ui.label(egui::RichText::new("\u{2014}").weak());
                                } else {
                                    ui.add(egui::Label::new(value).wrap());
                                }
                                ui.end_row();
                            }
                        });
                });
            });
    }
    *card_open = open;
    if !*card_open {
        *card = None;
    }
}

pub(super) fn help_window(ctx: &egui::Context, show_help: &mut bool, lang: Lang, t: &Tr) {
    if !*show_help {
        return;
    }
    let mut open = *show_help;
    egui::Window::new(format!("? {}", t.help))
        .open(&mut open)
        .collapsible(false)
        .default_width(560.0)
        .default_height(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for section in help_sections(lang) {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(section.title).strong().size(15.0));
                    ui.add_space(2.0);
                    for item in section.items {
                        ui.horizontal_top(|ui| {
                            ui.label(egui::RichText::new("\u{2022}").weak());
                            ui.label(*item);
                        });
                    }
                    ui.add_space(6.0);
                }
            });
        });
    *show_help = open;
}

pub(super) fn settings_window(
    ctx: &egui::Context,
    input: SettingsWindowInput<'_>,
) -> Option<SettingsAction> {
    let SettingsWindowInput {
        show_settings,
        lang,
        db_path,
        busy,
        db_ready,
        t,
        app_version,
    } = input;
    if !*show_settings {
        return None;
    }
    let mut action = None;
    let mut open = *show_settings;
    egui::Window::new(format!("\u{2699} {}", t.settings))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([24.0, 10.0])
                .show(ui, |ui| {
                    ui.label(t.language);
                    let mut next_lang = *lang;
                    egui::ComboBox::from_id_salt("settings_lang")
                        .width(150.0)
                        .selected_text(next_lang.label())
                        .show_ui(ui, |ui| {
                            for l in Lang::ALL {
                                ui.selectable_value(&mut next_lang, l, l.label());
                            }
                        });
                    if next_lang != *lang {
                        *lang = next_lang;
                        action = Some(SettingsAction::Persist {
                            key: "lang",
                            value: next_lang.code().to_string(),
                        });
                    }
                    ui.end_row();

                    ui.label(t.theme_label);
                    ui.horizontal(|ui| {
                        let dark = ui.visuals().dark_mode;
                        if ui.selectable_label(!dark, t.theme_light).clicked() && dark {
                            ctx.set_theme(egui::Theme::Light);
                            action = Some(SettingsAction::Persist {
                                key: "theme",
                                value: "light".to_string(),
                            });
                        }
                        if ui.selectable_label(dark, t.theme_dark).clicked() && !dark {
                            ctx.set_theme(egui::Theme::Dark);
                            action = Some(SettingsAction::Persist {
                                key: "theme",
                                value: "dark".to_string(),
                            });
                        }
                    });
                    ui.end_row();

                    ui.label(t.zoom_label);
                    ui.horizontal(|ui| {
                        let zoom = ctx.zoom_factor();
                        let mut new_zoom = zoom;
                        if ui.button("\u{2212}").clicked() {
                            new_zoom = (zoom - 0.1).max(0.6);
                        }
                        ui.label(format!("{:.0}%", zoom * 100.0));
                        if ui.button("+").clicked() {
                            new_zoom = (zoom + 0.1).min(2.0);
                        }
                        if (new_zoom - zoom).abs() > f32::EPSILON {
                            ctx.set_zoom_factor(new_zoom);
                            action = Some(SettingsAction::Persist {
                                key: "zoom",
                                value: format!("{new_zoom:.2}"),
                            });
                        }
                        ui.label(egui::RichText::new("Ctrl + / \u{2212}").weak().small());
                    });
                    ui.end_row();
                });

            ui.separator();
            ui.label(egui::RichText::new(t.db_section).strong());
            ui.add_space(4.0);
            egui::Grid::new("settings_db_grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(t.db_file_label).weak());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(db_path.display().to_string()).small(),
                        )
                        .wrap(),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new(t.db_size_label).weak());
                    let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
                    ui.label(format!("{:.2} GB", size as f64 / (1u64 << 30) as f64));
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Copy path").clicked() {
                    action = Some(SettingsAction::CopyDbPath);
                }
                if ui.button("Open folder").clicked() {
                    action = Some(SettingsAction::OpenDbFolder);
                }
                if ui
                    .add_enabled(!busy && db_ready, egui::Button::new("Optimize database"))
                    .on_hover_text("Checkpoints the SQLite WAL file. This can reduce sidecar files without deleting data.")
                    .clicked()
                {
                    action = Some(SettingsAction::OptimizeDatabase);
                }
            });
            ui.add_space(8.0);
            let clear_btn =
                egui::Button::new(egui::RichText::new(t.clear_db).color(egui::Color32::WHITE))
                    .fill(egui::Color32::from_rgb(200, 50, 50));
            if ui.add_enabled(!busy, clear_btn).clicked() {
                action = Some(SettingsAction::ClearDatabase);
            }
            ui.add_space(6.0);
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{}: {app_version}", t.version_label))
                    .weak()
                    .small(),
            );
        });
    *show_settings = open;
    action
}

pub(super) fn confirm_clear_window(ctx: &egui::Context, confirm_clear: &mut bool, t: &Tr) -> bool {
    if !*confirm_clear {
        return false;
    }
    let mut confirmed = false;
    egui::Window::new(t.clear_db)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(t.clear_confirm);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let yes_btn =
                    egui::Button::new(egui::RichText::new(t.clear_yes).color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(200, 50, 50));
                if ui.add(yes_btn).clicked() {
                    *confirm_clear = false;
                    confirmed = true;
                }
                if ui.button(t.cancel).clicked() {
                    *confirm_clear = false;
                }
            });
        });
    confirmed
}
