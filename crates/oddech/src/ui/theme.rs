//! Colours, fonts and egui style. The palette follows the mockups: a dark,
//! quiet UI where only the accent (breaths, primary actions) stands out.

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

pub const BG: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x1E);
pub const BG_LANES: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x20);
pub const BG_ELEVATED: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2E);
pub const BG_CARD: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3C);
pub const BG_CONTROL: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3C);
pub const SEPARATOR: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2E);
pub const SEPARATOR_CARD: Color32 = Color32::from_rgb(0x48, 0x48, 0x4A);

pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x93);
pub const TEXT_TERTIARY: Color32 = Color32::from_rgb(0x63, 0x63, 0x66);

pub const ACCENT: Color32 = Color32::from_rgb(0x5A, 0xC8, 0xFA);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x08, 0x24, 0x33);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);
pub const MUTE_ON: Color32 = Color32::from_rgb(0xB8, 0x3A, 0x34);
pub const SOLO_ON: Color32 = Color32::from_rgb(0xE6, 0xB4, 0x22);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x60);

pub const WAVE_ACTIVE: Color32 = Color32::from_rgb(0xDA, 0xDA, 0xDF);
pub const WAVE_INACTIVE: Color32 = Color32::from_rgb(0x5C, 0x5C, 0x60);
pub const WAVE_MUTED: Color32 = Color32::from_rgb(0x3E, 0x3E, 0x42);
pub const SELECTION: Color32 = Color32::from_rgba_premultiplied(0x2A, 0x4A, 0x5A, 0x50);

pub const MONO: &str = "mono";

/// Region fill: intensity follows detector confidence.
pub fn region_fill(confidence: f32, active: bool) -> Color32 {
    if !active {
        return Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, 10);
    }
    let a = 16.0 + 30.0 * confidence.clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, a as u8)
}

pub fn region_border(active: bool) -> Color32 {
    if active {
        Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, 90)
    } else {
        Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, 45)
    }
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.slider_rail_height = 6.0;
        style.spacing.interact_size.y = 28.0;

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = BG_ELEVATED;
        v.extreme_bg_color = Color32::from_rgb(0x12, 0x12, 0x14);
        v.faint_bg_color = BG_ELEVATED;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT;
        v.selection.stroke.color = ON_ACCENT;
        v.hyperlink_color = ACCENT;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.window_corner_radius = 14.into();
        v.menu_corner_radius = 10.into();
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
            &mut v.widgets.noninteractive,
        ] {
            w.corner_radius = 8.into();
        }
        v.widgets.inactive.weak_bg_fill = BG_CONTROL;
        v.widgets.inactive.bg_fill = Color32::from_rgb(0x48, 0x48, 0x4A);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x48, 0x48, 0x4A);
        v.widgets.hovered.bg_fill = Color32::from_rgb(0x55, 0x55, 0x58);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0x55, 0x55, 0x58);
        v.widgets.noninteractive.bg_stroke.color = SEPARATOR;
    });
}

/// Uses DejaVu (matches the mockups and covers Polish glyphs) when
/// installed; otherwise egui's bundled fonts remain.
fn install_fonts(ctx: &egui::Context) {
    const DIR: &str = "/usr/share/fonts/truetype/dejavu";
    let load = |name: &str| std::fs::read(format!("{DIR}/{name}")).ok();
    let mut fonts = FontDefinitions::default();
    if let Some(sans) = load("DejaVuSans.ttf") {
        fonts.font_data.insert("dejavu".into(), FontData::from_owned(sans).into());
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "dejavu".into());
    }
    if let Some(bold) = load("DejaVuSans-Bold.ttf") {
        fonts.font_data.insert("dejavu-bold".into(), FontData::from_owned(bold).into());
        fonts.families.insert(FontFamily::Name("bold".into()), vec!["dejavu-bold".into()]);
    } else {
        let prop = fonts.families[&FontFamily::Proportional].clone();
        fonts.families.insert(FontFamily::Name("bold".into()), prop);
    }
    if let Some(mono) = load("DejaVuSansMono.ttf") {
        fonts.font_data.insert("dejavu-mono".into(), FontData::from_owned(mono).into());
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "dejavu-mono".into());
    }
    let mono = fonts.families[&FontFamily::Monospace].clone();
    fonts.families.insert(FontFamily::Name(MONO.into()), mono);
    ctx.set_fonts(fonts);
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO.into()))
}
