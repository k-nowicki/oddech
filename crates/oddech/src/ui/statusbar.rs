//! Status bar along the bottom of the window: where breath detection runs.

use egui::{Color32, RichText, Sense, Stroke, Ui, vec2};
use oddech_detect::respiro::{self, Device, Status};

use super::theme;

pub const HEIGHT: f32 = 24.0;

pub fn show(ui: &mut Ui, state: &mut super::UiState) {
    egui::Panel::bottom("status_bar")
        .exact_size(HEIGHT)
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 0)))
        .show_separator_line(false)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::SEPARATOR));
            ui.horizontal_centered(|ui| {
                detection_indicator(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let link = egui::Button::new(RichText::new("Pomoc (F1)").size(12.0).color(theme::TEXT_SECONDARY)).frame(false);
                    if ui.add(link).on_hover_text("Skróty klawiszowe i opis działania").clicked() {
                        super::help::toggle(state);
                    }
                });
            });
        });
}

fn detection_indicator(ui: &mut Ui) {
    let log = crate::logging::log_path().map(|p| format!("\n\nSzczegóły w dzienniku: {}", home_relative(&p))).unwrap_or_default();
    let (dot, label, tip) = match respiro::status() {
        Status::NotLoaded | Status::Loading => {
            (theme::TEXT_TERTIARY, "Wykrywanie: …".to_owned(), "Uruchamianie modelu wykrywania wdechów…".to_owned())
        }
        Status::Ready(Device::Gpu { name }) => {
            (theme::ACCENT, "Wykrywanie: GPU".to_owned(), format!("Wykrywanie wdechów działa na karcie graficznej:\n{name}"))
        }
        Status::Ready(Device::Cpu { reason, fallback: false }) => (
            theme::TEXT_SECONDARY,
            "Wykrywanie: CPU".to_owned(),
            format!("Wykrywanie wdechów działa na procesorze (wolniej niż na karcie NVIDIA).\nPowód: {reason}"),
        ),
        Status::Ready(Device::Cpu { reason, fallback: true }) => (
            theme::SOLO_ON,
            "Wykrywanie: CPU".to_owned(),
            format!("Karta graficzna zawiodła, wykrywanie wdechów działa na procesorze.\nPowód: {reason}{log}"),
        ),
        Status::Failed(e) => {
            (theme::DANGER, "Wykrywanie niedostępne".to_owned(), format!("Nie można uruchomić wykrywania wdechów:\n{e}{log}"))
        }
    };
    let rect = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 3.5, dot);
            ui.add(egui::Label::new(RichText::new(label).size(12.0).color(theme::TEXT_SECONDARY)).selectable(false));
        })
        .response
        .rect;
    ui.interact(rect, ui.id().with("detection_device"), Sense::hover())
        .on_hover_text(RichText::new(tip).color(Color32::WHITE));
}

/// `~/…` for paths under the home directory.
fn home_relative(path: &std::path::Path) -> String {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_owned());
    match home.and_then(|h| path.strip_prefix(h).ok().map(|rest| format!("~/{}", rest.display()))) {
        Some(p) => p,
        None => path.display().to_string(),
    }
}
