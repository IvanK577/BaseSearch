use super::analytics_groups::{AnalyticsCardAction, analytics_cards_with_options};
use super::format::{fmt_compact, fmt_money_compact, fmt_money_per_kg};
use super::month_chart::{MonthMetric, months_chart};
use super::price_view::price_table;
use super::ui_text::trunc_label;
use super::widgets::kpi_tile;
use crate::db::{
    AnalyticsFilterAction, AnalyticsFilterField, AnalyticsGroupRow, AnalyticsSection,
    AnalyticsSectionKind, CompanyProfile,
};
use crate::i18n::{Lang, Tr, fmt, group_digits};

pub(super) enum ProfileViewAction {
    Close,
    Filter(AnalyticsFilterAction),
}

pub(super) fn profile_view(
    root: &mut egui::Ui,
    profile: Option<&CompanyProfile>,
    profile_loading: bool,
    t: &Tr,
    lang: Lang,
) -> Option<ProfileViewAction> {
    let mut action = None;
    egui::CentralPanel::default().show_inside(root, |ui| {
        ui.horizontal(|ui| {
            if ui.button(format!("\u{2190} {}", t.profile_back)).clicked() {
                action = Some(ProfileViewAction::Close);
            }
            ui.heading(t.company_profile);
            if profile_loading {
                ui.spinner();
            }
        });
        ui.add_space(4.0);

        let Some(profile) = profile else {
            ui.add_space((ui.available_height() * 0.30).max(0.0));
            ui.vertical_centered(|ui| {
                ui.spinner();
            });
            return;
        };

        let primary = profile.names.first().cloned().unwrap_or_default();
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    cols[0].label(
                        egui::RichText::new(if primary.is_empty() {
                            profile.edrpou.clone()
                        } else {
                            primary.clone()
                        })
                        .size(20.0)
                        .strong(),
                    );
                    cols[0].horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{}: {}", t.edrpou, profile.edrpou)).weak(),
                        );
                        if ui.small_button(t.show_results).clicked() {
                            action = Some(ProfileViewAction::Filter(AnalyticsFilterAction {
                                field: AnalyticsFilterField::Edrpou,
                                value: profile.edrpou.clone(),
                            }));
                        }
                    });
                    if profile.names.len() > 1 {
                        cols[0].label(
                            egui::RichText::new(fmt(
                                t.also_known_as,
                                &[&profile.names[1..].join(" · ")],
                            ))
                            .weak()
                            .small(),
                        );
                    }

                    profile_highlight_row(
                        &mut cols[1],
                        profile_highlight_product(lang),
                        profile.top_products.first(),
                        lang,
                    );
                    profile_highlight_row(
                        &mut cols[1],
                        profile_highlight_sender(lang),
                        profile.top_senders.first(),
                        lang,
                    );
                    profile_highlight_row(
                        &mut cols[1],
                        profile_highlight_country(lang),
                        profile.top_origin_countries.first(),
                        lang,
                    );
                });
            });
        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                kpi_tile(
                    ui,
                    t.rows_label,
                    group_digits(profile.overview.row_count),
                    t.rows_help,
                );
                kpi_tile(
                    ui,
                    t.declarations_label,
                    group_digits(profile.overview.declaration_count),
                    t.declarations_help,
                );
                kpi_tile(
                    ui,
                    t.total_value,
                    fmt_money_compact(&profile.overview.measures, lang),
                    t.total_value_help,
                );
                kpi_tile(
                    ui,
                    t.net_weight,
                    format!("{} kg", fmt_compact(profile.overview.total_net_kg)),
                    t.net_weight_help,
                );
                kpi_tile(
                    ui,
                    t.avg_value_kg,
                    fmt_money_per_kg(&profile.overview.measures, lang),
                    t.avg_value_kg_help,
                );
                kpi_tile(
                    ui,
                    t.product_codes_count,
                    group_digits(profile.overview.distinct_product_codes),
                    t.product_codes_help,
                );
                kpi_tile(
                    ui,
                    t.unique_senders,
                    group_digits(profile.overview.distinct_senders),
                    t.unique_senders,
                );
            });
            ui.add_space(12.0);

            if !profile.months.is_empty() {
                ui.label(egui::RichText::new(t.months_section).strong());
                ui.add_space(2.0);
                months_chart(ui, &profile.months, MonthMetric::Value, lang);
                ui.add_space(12.0);
            }

            ui.columns(2, |cols| {
                cols[0].label(egui::RichText::new(t.products_section).strong());
                cols[0].label(egui::RichText::new(t.products_section_hint).weak().small());
                if let Some(next) = analytics_cards_with_options(
                    &mut cols[0],
                    &profile.product_sections,
                    lang,
                    false,
                ) && let AnalyticsCardAction::Filter(filter) = next
                {
                    action = Some(ProfileViewAction::Filter(filter));
                }

                cols[1].label(egui::RichText::new(t.companies_section).strong());
                let sender_section = [AnalyticsSection {
                    kind: AnalyticsSectionKind::Senders,
                    rows: profile.top_senders.clone(),
                }];
                if let Some(next) =
                    analytics_cards_with_options(&mut cols[1], &sender_section, lang, false)
                    && let AnalyticsCardAction::Filter(filter) = next
                {
                    action = Some(ProfileViewAction::Filter(filter));
                }
            });
            ui.add_space(10.0);

            ui.columns(2, |cols| {
                cols[0].label(egui::RichText::new(t.countries_section).strong());
                cols[0].label(egui::RichText::new(t.countries_section_hint).weak().small());
                if let Some(next) = analytics_cards_with_options(
                    &mut cols[0],
                    &profile.country_sections,
                    lang,
                    false,
                ) && let AnalyticsCardAction::Filter(filter) = next
                {
                    action = Some(ProfileViewAction::Filter(filter));
                }

                cols[1].label(egui::RichText::new(t.prices_section).strong());
                cols[1].label(egui::RichText::new(t.prices_section_hint).weak().small());
                price_table(&mut cols[1], &profile.price_sections, lang);
            });
            ui.add_space(8.0);
        });
    });
    action
}

fn profile_highlight_product(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Головний товар",
        _ => "Main good",
    }
}

fn profile_highlight_sender(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Головний відправник",
        _ => "Main sender",
    }
}

fn profile_highlight_country(lang: Lang) -> &'static str {
    match lang {
        Lang::Ua => "Головна країна",
        _ => "Main country",
    }
}

fn profile_highlight_row(
    ui: &mut egui::Ui,
    label: &str,
    row: Option<&AnalyticsGroupRow>,
    lang: Lang,
) {
    let value = row
        .map(|row| {
            format!(
                "{} · {}",
                trunc_label(&row.label, 34),
                fmt_money_compact(&row.measures, lang)
            )
        })
        .unwrap_or_else(|| "—".to_string());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).weak().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).monospace());
        });
    });
}
