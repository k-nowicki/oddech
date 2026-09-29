//! Small custom widgets matching the mockups.

use egui::{Color32, CornerRadius, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use super::theme;

/// iOS-style on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let size = vec2(42.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let enabled = ui.is_enabled();
        let off_bg = Color32::from_rgb(0x48, 0x48, 0x4A);
        let mut bg = lerp_color(off_bg, theme::ACCENT, t);
        let mut knob = Color32::WHITE;
        if !enabled {
            bg = bg.gamma_multiply(0.5);
            knob = Color32::from_gray(0x70);
        }
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(12), bg);
        let r = rect.height() / 2.0 - 2.5;
        let x = egui::lerp(rect.left() + r + 2.5..=rect.right() - r - 2.5, t);
        painter.circle_filled(pos2(x, rect.center().y), r, knob);
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Two-state segmented control ("Przed | Po"). Returns true on change.
pub fn segmented(ui: &mut Ui, value: &mut bool, off_label: &str, on_label: &str) -> bool {
    let font = egui::FontId::proportional(16.0);
    let pad = vec2(14.0, 7.0);
    let galleys = [off_label, on_label].map(|t| {
        ui.painter().layout_no_wrap(t.to_owned(), font.clone(), theme::TEXT)
    });
    let seg_w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 2.0 * pad.x;
    let h = galleys[0].size().y + 2.0 * pad.y;
    let (rect, _) = ui.allocate_exact_size(vec2(seg_w * 2.0 + 4.0, h + 4.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(9), Color32::from_rgb(0x26, 0x26, 0x28));
    let mut changed = false;
    for (i, galley) in galleys.into_iter().enumerate() {
        let seg = egui::Rect::from_min_size(rect.min + vec2(2.0 + i as f32 * seg_w, 2.0), vec2(seg_w, h));
        let id = ui.id().with(("segment", off_label, i));
        let resp = ui.interact(seg, id, Sense::click());
        let selected = *value == (i == 1);
        if selected {
            painter.rect_filled(seg, CornerRadius::same(7), Color32::from_rgb(0x48, 0x48, 0x4A));
        } else if resp.hovered() {
            painter.rect_filled(seg, CornerRadius::same(7), Color32::from_rgb(0x33, 0x33, 0x36));
        }
        let color = if selected { theme::TEXT } else { theme::TEXT_SECONDARY };
        let pos = seg.center() - galley.size() / 2.0;
        painter.galley(pos, galley, color);
        if resp.clicked() && !selected {
            *value = i == 1;
            changed = true;
        }
    }
    changed
}

/// Filled accent button (primary action).
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let label = egui::RichText::new(text).font(theme::bold(16.0)).color(theme::ON_ACCENT);
    ui.add(
        egui::Button::new(label)
            .fill(theme::ACCENT)
            .corner_radius(CornerRadius::same(9))
            .min_size(vec2(0.0, 36.0)),
    )
}

/// Quiet secondary button.
pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(16.0))
            .fill(Color32::from_rgb(0x3A, 0x3A, 0x3C))
            .corner_radius(CornerRadius::same(9))
            .min_size(vec2(0.0, 36.0)),
    )
}

/// Small square M/S toggle button in track headers.
pub fn square_toggle(ui: &mut Ui, label: &str, on: bool, on_color: Color32, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let bg = if on {
        on_color
    } else if response.hovered() {
        Color32::from_rgb(0x44, 0x44, 0x47)
    } else {
        Color32::from_rgb(0x36, 0x36, 0x39)
    };
    let fg = if on { Color32::WHITE } else { theme::TEXT_SECONDARY };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, theme::bold(13.0), fg);
    response.on_hover_text(tooltip)
}

/// Round play/pause button (white disc).
pub fn play_button(ui: &mut Ui, playing: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() { Color32::from_gray(0xFF) } else { Color32::from_gray(0xEE) };
    painter.circle_filled(rect.center(), 20.0, fill);
    let c = rect.center();
    let dark = Color32::from_rgb(0x1C, 0x1C, 0x1E);
    if playing {
        for dx in [-4.5, 2.0] {
            let bar = egui::Rect::from_min_size(pos2(c.x + dx, c.y - 6.5), vec2(3.0, 13.0));
            painter.rect_filled(bar, CornerRadius::same(1), dark);
        }
    } else {
        let pts = vec![pos2(c.x - 3.5, c.y - 6.5), pos2(c.x - 3.5, c.y + 6.5), pos2(c.x + 7.0, c.y)];
        painter.add(egui::Shape::convex_polygon(pts, dark, Stroke::NONE));
    }
    response.on_hover_text("Odtwarzaj / pauza (Spacja)")
}

/// Borderless icon-like text button (e.g. "…", "‹", "›").
pub fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(0x36, 0x36, 0x39));
    }
    let color = if ui.is_enabled() { theme::TEXT } else { theme::TEXT_TERTIARY };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(20.0), color);
    response.on_hover_text(tooltip)
}

/// A rounded "card" grouping rows, like iOS settings.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(16, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Section title above a card ("WYKRYWANIE").
pub fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(14.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(13.0).color(theme::TEXT_SECONDARY).extra_letter_spacing(0.8));
    ui.add_space(2.0);
}

/// Thin separator between rows inside a card.
pub fn row_separator(ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().top();
    ui.painter().hline(rect.left()..=rect.right() + 16.0, y, Stroke::new(1.0, theme::SEPARATOR_CARD));
    ui.add_space(1.0);
}

/// Fixed-height row with a label on the left and content on the right.
pub fn row<R>(ui: &mut Ui, label: &str, add_right: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.set_min_height(40.0);
        ui.label(egui::RichText::new(label).size(16.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add_right).inner
    })
    .inner
}

/// A row that navigates to a sub-page ("Informacje o ścieżkach  ›").
pub fn nav_row(ui: &mut Ui, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(8.0, 0.0)), CornerRadius::same(6), Color32::from_white_alpha(6));
    }
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(16.0),
        theme::TEXT,
    );
    ui.painter().text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        "›",
        egui::FontId::proportional(20.0),
        theme::TEXT_SECONDARY,
    );
    response
}

pub fn outline_rect(painter: &egui::Painter, rect: egui::Rect, radius: u8, stroke: Stroke) {
    painter.rect_stroke(rect, CornerRadius::same(radius), stroke, StrokeKind::Inside);
}
