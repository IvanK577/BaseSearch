pub(super) fn kpi_tile(ui: &mut egui::Ui, label: &str, value: String, help: &str) {
    let frame = egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(8));
    let response = frame
        .show(ui, |ui| {
            ui.set_min_width(146.0);
            ui.label(egui::RichText::new(label).weak().small());
            ui.add_space(2.0);
            ui.label(egui::RichText::new(value).strong().monospace().size(16.0));
        })
        .response;
    response.on_hover_text(help);
}
