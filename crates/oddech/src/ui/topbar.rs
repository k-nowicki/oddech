//! Top bar: file name and breath count, transport, attenuation, Before/After,
//! Export and the "…" menu.

use egui::{Align, Layout, RichText, Ui, vec2};
use oddech_core::gain::MUTE_FLOOR_DB;
use oddech_core::time::{format_hms_frames, format_hms_millis};

use super::{Action, UiState, theme, widgets};
use crate::session::Session;
use crate::settings::Settings;

pub fn show(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState, actions: &mut Vec<Action>) {
    let rect = ui.available_rect_before_wrap();
    let h = rect.height();

    // Right: attenuation, Before/After, Export, menu.
    let mut right = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(24.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    right.spacing_mut().item_spacing.x = 14.0;
    if widgets::icon_button(&mut right, "…", "Więcej opcji").clicked() {
        state.options_open = !state.options_open;
        state.help_open = false;
        if state.options_open {
            // Measure the cache again for "Pamięć podręczna".
            state.cache_bytes.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
            state.cache_stale = true;
        }
        state.options_page = super::options::Page::Main;
    }
    let export = right.add_enabled_ui(!state.busy, |ui| widgets::primary_button(ui, "  Eksportuj  ")).inner;
    if export.on_hover_text(settings.export_format.label()).clicked() {
        actions.push(Action::Export);
    }
    let mut after = session.after;
    if widgets::segmented(&mut right, &mut after, "Przed", "Po") {
        session.set_after(after);
    }
    right.add_space(6.0);
    let db = settings.breath_gain_db;
    right.label(RichText::new(gain_label(db)).font(theme::mono(15.0)).color(theme::TEXT));
    let mut slider_db = if db.is_finite() { db.max(MUTE_FLOOR_DB) } else { MUTE_FLOOR_DB };
    right.spacing_mut().slider_width = 170.0;
    let slider = right.add(egui::Slider::new(&mut slider_db, MUTE_FLOOR_DB..=0.0).show_value(false).step_by(1.0));
    if slider.changed() {
        // One value for all tracks and recordings (application setting).
        settings.breath_gain_db = if slider_db <= MUTE_FLOOR_DB { f32::NEG_INFINITY } else { slider_db };
    }
    slider.on_hover_text("Ściszenie wdechów (przeciągnij do końca w lewo, aby wyciszyć całkowicie)");
    right.label(RichText::new("Ściszenie").size(16.0).color(theme::TEXT_SECONDARY));

    // Left: file name and count.
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(28.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    left.spacing_mut().item_spacing.x = 12.0;
    if widgets::secondary_button(&mut left, "Otwórz…").on_hover_text("Otwórz inne nagranie (Ctrl+O lub upuść plik w oknie)").clicked() {
        actions.push(Action::Open);
    }
    // Long file names are truncated so they never push the transport.
    let count_w = 120.0;
    let centre_left = (rect.center().x - 125.0).min(right.min_rect().left() - 16.0 - 250.0);
    let name_budget = (centre_left - 16.0 - count_w - left.cursor().left()).max(80.0);
    left.scope(|ui| {
        ui.set_max_width(name_budget);
        let name = session.file_name();
        ui.add(egui::Label::new(RichText::new(&name).font(theme::bold(18.0))).truncate()).on_hover_text(name);
    });
    let n = session.review_list().len();
    left.label(RichText::new(breath_count_label(n)).size(15.0).color(theme::TEXT_SECONDARY));
    if session.save_error.is_some() {
        left.label(RichText::new("⚠").color(theme::DANGER)).on_hover_text(session.save_error.clone().unwrap_or_default());
    }

    // Centre: play + timecode, centred in the window when there is room,
    // otherwise in the free space between the left and right groups.
    let center_w = 250.0;
    let free_left = left.min_rect().right() + 16.0;
    let free_right = right.min_rect().left() - 16.0;
    let cx = rect.center().x.clamp(free_left + center_w / 2.0, (free_right - center_w / 2.0).max(free_left + center_w / 2.0));
    let center = egui::Rect::from_center_size(egui::pos2(cx, rect.center().y), vec2(center_w, h));
    let mut mid = ui.new_child(egui::UiBuilder::new().max_rect(center).layout(Layout::left_to_right(Align::Center)));
    mid.spacing_mut().item_spacing.x = 16.0;
    let playing = session.is_playing();
    if widgets::play_button(&mut mid, playing).clicked() {
        session.toggle_play(settings);
    }
    let t = session.playhead();
    let tc = match (settings.timecode_frames, session.info.video_fps) {
        (true, Some(fps)) => format_hms_frames(t, fps),
        _ => format_hms_millis(t),
    };
    let tc_resp = mid.add(egui::Label::new(RichText::new(tc).font(theme::mono(22.0))).sense(egui::Sense::click()));
    if session.info.video_fps.is_some() && tc_resp.on_hover_text("Kliknij: przełącz ms / klatki").clicked() {
        settings.timecode_frames = !settings.timecode_frames;
    }
    ui.allocate_rect(rect, egui::Sense::hover());
}

fn gain_label(db: f32) -> String {
    if db.is_finite() && db > MUTE_FLOOR_DB { format!("{:>4.0} dB", db).replace('-', "−") } else { "wycisz".into() }
}

pub fn breath_count_label(n: usize) -> String {
    // Polish plural forms: 1 wdech, 2–4 wdechy (except 12–14), 5+ wdechów.
    let word = if n == 1 {
        "wdech"
    } else if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {
        "wdechy"
    } else {
        "wdechów"
    };
    format!("{n} {word}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_plurals() {
        assert_eq!(breath_count_label(0), "0 wdechów");
        assert_eq!(breath_count_label(1), "1 wdech");
        assert_eq!(breath_count_label(3), "3 wdechy");
        assert_eq!(breath_count_label(12), "12 wdechów");
        assert_eq!(breath_count_label(22), "22 wdechy");
        assert_eq!(breath_count_label(41), "41 wdechów");
    }
}
