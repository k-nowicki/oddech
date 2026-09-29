//! Floating review bar: walk through detected breaths and accept/reject.

use egui::{Align2, CornerRadius, RichText, Ui, vec2};
use oddech_core::time::format_ruler_label;

use super::{theme, widgets};
use crate::session::Session;
use crate::settings::Settings;

/// Fixed geometry: the bar must not move or resize while stepping through
/// breaths, so buttons stay under the mouse.
const BAR_WIDTH: f32 = 860.0;
const TEXT_WIDTH: f32 = 290.0;

pub fn show(ui: &mut Ui, session: &mut Session, settings: &Settings, lanes_rect: egui::Rect) -> bool {
    let mut accepted_all = false;
    let list = session.review_list();
    let selected_idx = session.selected_region.and_then(|id| list.iter().position(|&x| x == id));
    if list.is_empty() && session.selected_region.is_none() {
        return false;
    }

    let area = egui::Area::new(ui.id().with("review_bar"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_BOTTOM, vec2(lanes_rect.center().x - ui.ctx().content_rect().center().x, -34.0))
        .interactable(true);
    area.show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(0x2A, 0x2A, 0x2D))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x3F, 0x3F, 0x42)))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin { left: 26, right: 12, top: 10, bottom: 10 })
            .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: egui::Color32::from_black_alpha(110) })
            .show(ui, |ui| {
                ui.set_width(BAR_WIDTH - 38.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.allocate_ui_with_layout(
                        vec2(TEXT_WIDTH, 44.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(TEXT_WIDTH);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let (title, subtitle) = describe(session, &list, selected_idx);
                            ui.add(egui::Label::new(RichText::new(title).font(theme::bold(17.0))).truncate());
                            ui.add(
                                egui::Label::new(RichText::new(subtitle).size(14.0).color(theme::TEXT_SECONDARY))
                                    .truncate(),
                            );
                        },
                    );
                    let has_prev = selected_idx.is_none_or(|i| i > 0) && !list.is_empty();
                    let has_next = selected_idx.is_none_or(|i| i + 1 < list.len()) && !list.is_empty();
                    if ui.add_enabled_ui(has_prev, |ui| widgets::icon_button(ui, "‹", "Poprzedni wdech (P)")).inner.clicked() {
                        session.step_region(false, settings);
                    }
                    if ui.add_enabled_ui(has_next, |ui| widgets::icon_button(ui, "›", "Następny wdech (N)")).inner.clicked() {
                        session.step_region(true, settings);
                    }
                    ui.add_space(24.0);
                    let can_decide = session.selected_region.is_some();
                    let reject = ui.add_enabled_ui(can_decide, |ui| widgets::secondary_button(ui, "To nie wdech")).inner;
                    if reject.on_hover_text("Odrzuć (X)").clicked() {
                        session.reject_selected(settings);
                    }
                    let accept = ui.add_enabled_ui(can_decide, |ui| widgets::primary_button(ui, "Zatwierdź")).inner;
                    if accept.on_hover_text("Zatwierdź (Z)").clicked() {
                        session.accept_selected(settings);
                    }
                    let all = ui
                        .add_enabled_ui(!list.is_empty(), |ui| widgets::secondary_button(ui, "Zatwierdź wszystkie"))
                        .inner
                        .on_hover_text("Zatwierdź wszystkie wdechy i zapisz");
                    if all.clicked() {
                        session.accept_all();
                        session.save_all();
                        accepted_all = true;
                    }
                });
            });
    });
    accepted_all
}

fn describe(session: &Session, list: &[oddech_core::RegionId], idx: Option<usize>) -> (String, String) {
    let region = session.selected_region.and_then(|id| session.project.region(id));
    match (idx, region) {
        (Some(i), Some(r)) => {
            let track = session
                .track_index(r.track)
                .map(|i| session.tracks[i].info.display_name())
                .unwrap_or_default();
            let status = if r.is_pending() { "" } else { " · zatwierdzony" };
            (
                format!("Wdech {} z {}", i + 1, list.len()),
                format!("{track} · {}{status}", format_ruler_label(r.start, 0.01)),
            )
        }
        (None, Some(r)) => (
            "Zaznaczony region".into(),
            format!("{} · nieaktywny", format_ruler_label(r.start, 0.01)),
        ),
        _ => {
            let pending = list.iter().filter(|id| session.project.region(**id).is_some_and(|r| r.is_pending())).count();
            let title = super::topbar::breath_count_label(list.len());
            let subtitle = if pending == 0 {
                "wszystkie przejrzane · N / P: przejdź".to_owned()
            } else {
                format!("do przejrzenia: {pending} · naciśnij N lub ›")
            };
            (title, subtitle)
        }
    }
}
