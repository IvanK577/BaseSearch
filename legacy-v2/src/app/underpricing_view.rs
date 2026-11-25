use super::format::{fmt_compact, fmt_decimal};
use super::widgets::kpi_tile;
use crate::db::{Undervaluation, UndervaluedRow};
use crate::i18n::{Lang, group_digits, tr};
use crate::schema::header_for;
use egui_extras::{Column, TableBuilder};

/// Table of flagged undervalued rows. Returns a record id when a row
/// is clicked (to open its card). `rescan` is set if the user asks to refresh.
pub(super) fn underpricing_table(
    ui: &mut egui::Ui,
    uv: &Undervaluation,
    lang: Lang,
    rescan: &mut bool,
) -> Option<i64> {
    let mut open_id = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(underpricing_found_text(uv, lang))
                .weak()
                .small(),
        );
        if ui.small_button(tr(lang).underpricing_rescan).clicked() {
            *rescan = true;
        }
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        kpi_tile(
            ui,
            underpricing_checked_rows_label(lang),
            group_digits(uv.checked_rows),
            underpricing_checked_rows_help(lang),
        );
        kpi_tile(
            ui,
            underpricing_checked_codes_label(lang),
            group_digits(uv.checked_codes),
            underpricing_checked_codes_help(lang),
        );
        kpi_tile(
            ui,
            underpricing_flagged_rows_label(lang),
            group_digits(uv.flagged_rows),
            underpricing_flagged_rows_help(lang),
        );
        kpi_tile(
            ui,
            underpricing_flagged_value_label(lang),
            fmt_compact(uv.flagged_value),
            underpricing_flagged_value_help(lang),
        );
        kpi_tile(
            ui,
            underpricing_estimated_gap_label(lang),
            fmt_compact(uv.estimated_gap),
            underpricing_estimated_gap_help(lang),
        );
    });
    if uv.rows.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tr(lang).underpricing_none).weak());
        return None;
    }
    ui.add_space(4.0);
    let (recip_h, code_h, desc_h) = (
        tr(lang).recipient,
        tr(lang).product_code,
        tr(lang).description,
    );
    let price_h = tr(lang).per_kg;
    let median_h = tr(lang).median;
    let below_h = tr(lang).below_by;
    let dark_mode = ui.visuals().dark_mode;
    egui::ScrollArea::horizontal()
        .id_salt("underpricing_scroll")
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(82.0).at_least(70.0))
                .column(Column::initial(180.0).at_least(100.0).clip(true))
                .column(Column::initial(86.0).at_least(64.0).clip(true))
                .column(Column::initial(96.0).at_least(70.0))
                .column(Column::initial(300.0).at_least(120.0).clip(true))
                .column(Column::initial(76.0).at_least(54.0))
                .column(Column::initial(76.0).at_least(54.0))
                .column(Column::initial(96.0).at_least(68.0))
                .column(Column::initial(72.0).at_least(56.0))
                .column(Column::initial(92.0).at_least(68.0))
                .column(Column::initial(76.0).at_least(58.0))
                .header(24.0, |mut h| {
                    h.col(|ui| {
                        ui.strong(header_for("declaration_date"));
                    });
                    h.col(|ui| {
                        ui.strong(recip_h);
                    });
                    h.col(|ui| {
                        ui.strong(tr(lang).edrpou);
                    });
                    h.col(|ui| {
                        ui.strong(code_h);
                    });
                    h.col(|ui| {
                        ui.strong(desc_h);
                    });
                    h.col(|ui| {
                        ui.strong(price_h);
                    });
                    h.col(|ui| {
                        ui.strong(median_h);
                    });
                    h.col(|ui| {
                        ui.strong("P25-P75");
                    });
                    h.col(|ui| {
                        ui.strong(below_h);
                    });
                    h.col(|ui| {
                        ui.strong(underpricing_gap_header(lang));
                    });
                    h.col(|ui| {
                        ui.strong(underpricing_samples_header(lang));
                    });
                })
                .body(|mut body| {
                    for row in &uv.rows {
                        body.row(22.0, |mut tr_row| {
                            let mut clicked = false;
                            let hover = underpricing_row_hover(row, lang);
                            let risk_color = undervaluation_risk_color(row.ratio, dark_mode);
                            tr_row.col(|ui| {
                                clicked |= underpricing_cell(ui, &row.declaration_date, &hover);
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_cell(ui, &row.recipient, &hover);
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_cell(ui, &row.edrpou, &hover);
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(&row.product_code).monospace(),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_cell(ui, &row.description, &hover);
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(fmt_decimal(row.price_per_kg, 2))
                                        .monospace()
                                        .color(risk_color),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(fmt_decimal(row.code_median, 2))
                                        .monospace(),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(format!(
                                        "{}-{}",
                                        fmt_decimal(row.code_p25, 2),
                                        fmt_decimal(row.code_p75, 2)
                                    ))
                                    .monospace(),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                let pct = ((1.0 - row.ratio) * 100.0).round() as i64;
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(format!("{pct}%"))
                                        .monospace()
                                        .strong()
                                        .color(risk_color),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(fmt_compact(row.estimated_gap))
                                        .monospace()
                                        .color(risk_color),
                                    &hover,
                                );
                            });
                            tr_row.col(|ui| {
                                clicked |= underpricing_rich_cell(
                                    ui,
                                    egui::RichText::new(group_digits(row.code_sample_count))
                                        .monospace(),
                                    &hover,
                                );
                            });
                            if clicked {
                                open_id = Some(row.id);
                            }
                        });
                    }
                });
        });
    open_id
}

fn underpricing_cell(ui: &mut egui::Ui, text: &str, hover: &str) -> bool {
    underpricing_rich_cell(ui, egui::RichText::new(text), hover)
}

fn underpricing_rich_cell(ui: &mut egui::Ui, text: egui::RichText, hover: &str) -> bool {
    ui.add(egui::Label::new(text).selectable(false).truncate())
        .on_hover_text(hover)
        .clicked()
}

fn underpricing_found_text(uv: &Undervaluation, lang: Lang) -> String {
    match lang {
        Lang::Ua => format!(
            "Знайдено {} підозрілих рядків у {} кодах; показано перші {}.",
            group_digits(uv.flagged_rows),
            group_digits(uv.flagged_codes),
            group_digits(uv.rows.len() as u64)
        ),
        _ => format!(
            "Found {} suspicious rows in {} codes; showing first {}.",
            group_digits(uv.flagged_rows),
            group_digits(uv.flagged_codes),
            group_digits(uv.rows.len() as u64)
        ),
    }
}

fn underpricing_row_hover(row: &UndervaluedRow, lang: Lang) -> String {
    let below = ((1.0 - row.ratio) * 100.0).round().max(0.0) as i64;
    match lang {
        Lang::Ua => format!(
            "{}\nМД: {}\nОдержувач: {}\nВідправник: {}\nКод: {}\nЦіна: {} / кг\nМедіана коду: {} / кг\nP25-P75: {}-{} / кг\nНижче медіани на: {}%\nОціночний розрив: {}\nЗначень у коді: {}\nФВ вал.контр: {}\nНетто: {} кг\nНатисніть, щоб відкрити картку рядка.",
            undervaluation_severity(row.ratio, lang),
            row.declaration_number,
            row.recipient,
            row.sender,
            row.product_code,
            fmt_decimal(row.price_per_kg, 2),
            fmt_decimal(row.code_median, 2),
            fmt_decimal(row.code_p25, 2),
            fmt_decimal(row.code_p75, 2),
            below,
            fmt_decimal(row.estimated_gap, 2),
            group_digits(row.code_sample_count),
            fmt_decimal(row.source_value, 2),
            fmt_decimal(row.net_kg, 3),
        ),
        _ => format!(
            "{}\nDeclaration: {}\nRecipient: {}\nSender: {}\nCode: {}\nPrice: {} / kg\nCode median: {} / kg\nP25-P75: {}-{} / kg\nBelow median by: {}%\nEstimated gap: {}\nSamples in code: {}\nValue: {}\nNet: {} kg\nClick to open the row card.",
            undervaluation_severity(row.ratio, lang),
            row.declaration_number,
            row.recipient,
            row.sender,
            row.product_code,
            fmt_decimal(row.price_per_kg, 2),
            fmt_decimal(row.code_median, 2),
            fmt_decimal(row.code_p25, 2),
            fmt_decimal(row.code_p75, 2),
            below,
            fmt_decimal(row.estimated_gap, 2),
            group_digits(row.code_sample_count),
            fmt_decimal(row.source_value, 2),
            fmt_decimal(row.net_kg, 3),
        ),
    }
}

fn undervaluation_severity(ratio: f64, lang: Lang) -> &'static str {
    match (lang, ratio) {
        (Lang::Ua, r) if r < 0.25 => "Критична різниця",
        (Lang::Ua, r) if r < 0.4 => "Сильна різниця",
        (Lang::Ua, _) => "Помітна різниця",
        (_, r) if r < 0.25 => "Critical gap",
        (_, r) if r < 0.4 => "Strong gap",
        _ => "Visible gap",
    }
}

fn undervaluation_risk_color(ratio: f64, dark_mode: bool) -> egui::Color32 {
    if ratio < 0.25 {
        if dark_mode {
            egui::Color32::from_rgb(255, 105, 105)
        } else {
            egui::Color32::from_rgb(190, 35, 35)
        }
    } else if ratio < 0.4 {
        if dark_mode {
            egui::Color32::from_rgb(255, 165, 84)
        } else {
            egui::Color32::from_rgb(180, 88, 15)
        }
    } else if dark_mode {
        egui::Color32::from_rgb(255, 207, 105)
    } else {
        egui::Color32::from_rgb(145, 105, 0)
    }
}

fn underpricing_checked_rows_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Перевірено рядків",
        _ => "Checked rows",
    }
}

fn underpricing_checked_rows_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Рядки з ціною та нетто у кодах, де достатньо прикладів для порівняння.",
        _ => "Rows with value and net weight in product codes with enough samples to compare.",
    }
}

fn underpricing_checked_codes_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Перевірено кодів",
        _ => "Checked codes",
    }
}

fn underpricing_checked_codes_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Коди товару, де є мінімальна вибірка для медіани.",
        _ => "Product codes that have the minimum sample size for a median.",
    }
}

fn underpricing_flagged_rows_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Підозрілих рядків",
        _ => "Suspicious rows",
    }
}

fn underpricing_flagged_rows_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Рядки, де ціна за кг нижча за встановлений поріг від медіани коду.",
        _ => "Rows where price per kg is below the selected share of the code median.",
    }
}

fn underpricing_flagged_value_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Сума підозрілих",
        _ => "Suspicious value",
    }
}

fn underpricing_flagged_value_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "ФВ вал.контр у рядках, які потрапили в список підозрілих.",
        _ => "Declared value of rows that were flagged as suspicious.",
    }
}

fn underpricing_estimated_gap_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Оціночний розрив",
        _ => "Estimated gap",
    }
}

fn underpricing_estimated_gap_help(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => {
            "Скільки б становила різниця, якби рядки рахувалися за медіанною ціною свого коду."
        }
        _ => "Difference versus valuing flagged rows at their product-code median price.",
    }
}

fn underpricing_gap_header(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Розрив",
        _ => "Gap",
    }
}

fn underpricing_samples_header(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Вибірка",
        _ => "Samples",
    }
}
