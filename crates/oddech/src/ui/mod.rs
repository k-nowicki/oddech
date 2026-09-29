//! User interface. Layout and hierarchy follow `Mockups/`: everything needed
//! all the time is on screen; everything else lives in "Więcej opcji".

pub mod help;
pub mod options;
pub mod review;
pub mod spectrogram;
pub mod statusbar;
pub mod theme;
pub mod timeline;
pub mod topbar;
pub mod waveform;
pub mod widgets;

use std::time::{Duration, Instant};

use egui::{Key, KeyboardShortcut, Modifiers, Ui};

use crate::session::Session;
use crate::settings::Settings;

/// Requests from the UI that the app layer carries out (they involve
/// background work or dialogs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Open,
    Export,
    Detect,
    ExportTimeline,
    ExportLabels(options::LabelFormat),
    /// Measure the cache size.
    MeasureCache,
    /// Remove every cached recording except the open one.
    ClearCache,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Default)]
pub struct UiState {
    pub options_open: bool,
    pub options_page: options::Page,
    pub help_open: bool,
    pub help_page: help::HelpPage,
    pub timeline: timeline::TimelineState,
    pub toasts: Vec<Toast>,
    /// A background job (detection, export) is running.
    pub busy: bool,
    /// Status line of the running job, with progress in `[0, 1]`.
    pub job_status: Option<(String, f32)>,
    /// Set by the "Anuluj" button; consumed by the app.
    pub cancel_requested: bool,
    /// Size of the decoded-audio cache in bytes (`u64::MAX` = not known
    /// yet), measured off the UI thread.
    pub cache_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The measurement above is out of date.
    pub cache_stale: bool,
}

impl UiState {
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, until: Instant::now() + Duration::from_secs(5) });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, until: Instant::now() + Duration::from_secs(12) });
    }
}

const TOP_BAR_HEIGHT: f32 = 76.0;
/// Redraw interval while playing (30 fps).
const PLAYBACK_FRAME: Duration = Duration::from_micros(33_333);
/// Redraw interval while a job runs or spectrogram tiles are computed.
const PROGRESS_FRAME: Duration = Duration::from_millis(100);

pub fn show_session(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState) -> Vec<Action> {
    let mut actions = Vec::new();
    session.apply_settings(settings);
    session.tick();
    handle_keys(ui, session, settings, state);

    egui::Panel::top("top_bar")
        .exact_size(TOP_BAR_HEIGHT)
        .frame(egui::Frame::new().fill(theme::BG))
        .show_separator_line(false)
        .show(ui, |ui| topbar::show(ui, session, settings, state, &mut actions));

    if state.help_open {
        help::show(ui, state);
    } else if state.options_open {
        egui::Panel::right("options")
            .exact_size(460.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::BG_ELEVATED).inner_margin(egui::Margin::symmetric(28, 0)))
            .show(ui, |ui| options::show(ui, session, settings, state, &mut actions));
    }

    let central = egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG_LANES)).show(ui, |ui| {
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, theme::SEPARATOR));
        timeline::show(ui, session, settings, &mut state.timeline);
        rect
    });
    let lanes_rect = central.inner;

    // Follow the playhead (page-style, like Resolve).
    if session.is_playing() && settings.follow_playhead && !ui.input(|i| i.pointer.any_down()) {
        session.view.ensure_visible(session.playhead(), 0.04);
    }

    if review::show(ui, session, settings, lanes_rect) {
        match &session.save_error {
            None => state.toast("Zapisano poprawnie."),
            Some(e) => state.error(e.clone()),
        }
    }
    show_status(ui, session, state, lanes_rect);

    // Only the playhead and progress move by themselves, so there is no
    // need to redraw at the display rate (software rendering has no vsync
    // at all and would redraw as fast as it can). Input still repaints at
    // once. egui shortens the delay by the predicted frame time; add it back.
    let predicted = Duration::from_secs_f32(ui.input(|i| i.predicted_dt));
    if session.is_playing() {
        ui.ctx().request_repaint_after(PLAYBACK_FRAME + predicted);
    } else if state.busy || state.timeline.spec.is_busy() {
        ui.ctx().request_repaint_after(PROGRESS_FRAME + predicted);
    }
    actions
}

fn show_status(ui: &mut Ui, session: &Session, state: &mut UiState, lanes_rect: egui::Rect) {
    let now = Instant::now();
    state.toasts.retain(|t| t.until > now);
    let mut lines: Vec<(String, bool, Option<f32>)> = Vec::new();
    let mut cancel = false;
    if let Some(e) = &session.audio_error {
        lines.push((format!("Brak dźwięku: {e}"), true, None));
    }
    if let Some((text, p)) = &state.job_status {
        lines.push((text.clone(), false, Some(*p)));
    }
    for t in &state.toasts {
        lines.push((t.text.clone(), t.error, None));
    }
    if lines.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("status"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(lanes_rect.right() - 420.0, lanes_rect.top() + 56.0))
        .show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            for (text, error, progress) in lines {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x2A, 0x2A, 0x2D))
                    .corner_radius(10)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(376.0);
                        let color = if error { theme::DANGER } else { theme::TEXT };
                        ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
                        if let Some(p) = progress {
                            ui.horizontal(|ui| {
                                ui.add(egui::ProgressBar::new(p).desired_height(6.0).desired_width(300.0));
                                if ui.small_button("Anuluj").clicked() {
                                    cancel = true;
                                }
                            });
                        }
                    });
                ui.add_space(6.0);
            }
        });
    if cancel {
        state.cancel_requested = true;
    }
    if !state.toasts.is_empty() {
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn handle_keys(ui: &mut Ui, session: &mut Session, settings: &mut Settings, state: &mut UiState) {
    let ctx = ui.ctx().clone();
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k) && i.modifiers.is_none());
    let shortcut = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));

    if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || shortcut(Modifiers::COMMAND, Key::Y) {
        session.redo();
    }
    if shortcut(Modifiers::COMMAND, Key::Z) {
        session.undo();
    }
    if pressed(Key::Space) {
        session.toggle_play(settings);
    }
    if pressed(Key::K) {
        session.stop();
    }
    if pressed(Key::L) && !session.is_playing() {
        session.toggle_play(settings);
    }
    if pressed(Key::J) {
        // No reverse playback: J jumps back 5 s (playback continues).
        session.seek(session.playhead() - 5.0);
    }
    if pressed(Key::Enter) {
        if session.selection.is_some() {
            session.play_selection(settings.loop_selection);
        } else if let Some(r) = session.selected_region.and_then(|id| session.project.region(id)).cloned() {
            session.selection = Some(crate::session::Selection { track: Some(r.track), start: r.start, end: r.end });
            session.play_selection(settings.loop_selection);
            session.selection = None;
        }
    }
    let view_x_center = session.view.width / 2.0;
    let playhead_x = session.view.x_of(session.playhead());
    let zoom_anchor = if (0.0..=session.view.width).contains(&playhead_x) { playhead_x } else { view_x_center };
    if ctx.input(|i| i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)) {
        session.view.zoom_at(zoom_anchor, 1.6);
    }
    if ctx.input(|i| i.key_pressed(Key::Minus)) {
        session.view.zoom_at(zoom_anchor, 1.0 / 1.6);
    }
    if shortcut(Modifiers::SHIFT, Key::Z) || pressed(Key::Backslash) {
        session.view.fit();
    }
    if pressed(Key::N) {
        session.step_region(true, settings);
    }
    if pressed(Key::P) {
        session.step_region(false, settings);
    }
    if pressed(Key::Z) && session.selected_region.is_some() {
        session.accept_selected(settings);
    }
    if pressed(Key::X) && session.selected_region.is_some() {
        session.reject_selected(settings);
    }
    if pressed(Key::B) && session.selection.is_some() {
        session.add_region_from_selection();
    }
    if pressed(Key::Delete) || pressed(Key::Backspace) {
        session.delete_selected_region();
    }
    if pressed(Key::Home) {
        session.seek(0.0);
        session.view.ensure_visible(0.0, 0.0);
    }
    if pressed(Key::End) {
        session.seek(session.duration());
    }
    if pressed(Key::Escape) {
        if state.help_open {
            state.help_open = false;
        } else if state.options_open {
            state.options_open = false;
        } else {
            session.selection = None;
            session.selected_region = None;
        }
    }
}
