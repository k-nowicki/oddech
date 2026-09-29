//! The timeline: ruler, track headers, lanes, scrollbar and all mouse
//! interaction on them (DaVinci Resolve–style navigation).

use egui::{Color32, CornerRadius, CursorIcon, PointerButton, Rect, Sense, Stroke, Ui, pos2, vec2};
use oddech_core::time::{format_ruler_label, nice_tick_step};
use oddech_core::{Region, RegionId, RegionKind, RegionSource, TrackId};

use super::theme;
use super::waveform::{self, WaveScratch, WaveSource, WaveStyle};
use super::widgets;
use crate::session::{Selection, Session};
use crate::timeline::TimelineView;
use crate::settings::Settings;

pub const HEADER_WIDTH: f32 = 240.0;
const RULER_HEIGHT: f32 = 40.0;
const SCROLLBAR_HEIGHT: f32 = 14.0;
const MIN_LANE_HEIGHT: f32 = 110.0;
const EDGE_GRAB_PX: f32 = 6.0;
/// Pixels of movement before a press on a region becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;
const DB_PER_PX: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Scrub,
    Select { track: TrackId, anchor: f64 },
    Edge { region: RegionId, side: Side },
    /// Pressed on a region body; becomes `Gain` or `Select` once moved.
    PendingRegion { region: RegionId, track: TrackId, anchor: f64 },
    Gain { region: RegionId, start_db: f32 },
    Pan,
    Thumb { grab_offset: f64 },
}

/// Interaction state that persists across frames.
#[derive(Default)]
pub struct TimelineState {
    drag: Option<Drag>,
    scratch: WaveScratch,
    pub spec: super::spectrogram::SpectrogramCache,
    /// Track rows scroll vertically when they do not fit.
    v_scroll: f32,
}

struct Lane {
    index: usize,
    id: TrackId,
    rect: Rect,
}

pub fn show(ui: &mut Ui, session: &mut Session, settings: &Settings, state: &mut TimelineState) {
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, CornerRadius::ZERO, theme::BG_LANES);

    let lanes_left = full.left() + HEADER_WIDTH;
    let ruler_rect = Rect::from_min_max(pos2(lanes_left, full.top()), pos2(full.right(), full.top() + RULER_HEIGHT));
    let scrollbar_rect =
        Rect::from_min_max(pos2(lanes_left, full.bottom() - SCROLLBAR_HEIGHT), pos2(full.right(), full.bottom()));
    let body = Rect::from_min_max(pos2(full.left(), ruler_rect.bottom()), pos2(full.right(), scrollbar_rect.top()));
    let lanes_area = Rect::from_min_max(pos2(lanes_left, body.top()), body.max);

    session.view.set_width(f64::from(lanes_area.width()));
    if settings.split_channels {
        session.request_channel_pyramids();
    }

    // Track rows share the height; below a minimum they scroll vertically.
    let n = session.tracks.len().max(1);
    let lane_h = (body.height() / n as f32).max(MIN_LANE_HEIGHT);
    let content_h = lane_h * n as f32;
    state.v_scroll = state.v_scroll.clamp(0.0, (content_h - body.height()).max(0.0));
    let lanes: Vec<Lane> = session
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| Lane {
            index: i,
            id: t.info.id,
            rect: Rect::from_min_size(
                pos2(lanes_left, body.top() + i as f32 * lane_h - state.v_scroll),
                vec2(lanes_area.width(), lane_h),
            ),
        })
        .collect();

    handle_wheel(ui, session, state, lanes_area.union(ruler_rect), body, content_h);

    // --- Painting -------------------------------------------------------
    let painter = ui.painter_at(body);
    let ppp = ui.ctx().pixels_per_point();
    if settings.show_spectrogram {
        state.spec.begin_frame(ui.ctx(), super::spectrogram::params_from(&settings.spectrogram));
    }
    for lane in &lanes {
        if !lane.rect.intersects(body) {
            continue;
        }
        draw_lane(&painter, ui, session, settings, state, lane, ppp);
        // Header column.
        let header_rect = Rect::from_min_max(pos2(full.left(), lane.rect.top()), pos2(lanes_left, lane.rect.bottom()));
        painter.hline(full.x_range(), lane.rect.bottom(), Stroke::new(1.0, theme::SEPARATOR));
        draw_header(ui, session, header_rect.intersect(body), header_rect, lane.index);
    }
    ui.painter().vline(lanes_left, body.y_range(), Stroke::new(1.0, theme::SEPARATOR));

    draw_ruler(ui, session, ruler_rect, state);
    draw_scrollbar(ui, session, scrollbar_rect, state);

    // --- Lane interaction --------------------------------------------------
    let resp = ui.interact(lanes_area, ui.id().with("lanes"), Sense::click_and_drag());
    lane_interaction(ui, session, settings, state, &lanes, &resp);
    add_breath_button(ui, session, state, &lanes, body);

    // Playhead over everything.
    let playhead = session.playhead();
    let x = lanes_left + session.view.x_of(playhead) as f32;
    if (lanes_left..=full.right()).contains(&x) {
        ui.painter().vline(x, full.top()..=scrollbar_rect.top(), Stroke::new(1.5, theme::PLAYHEAD));
        let tri = vec![pos2(x - 6.0, full.top()), pos2(x + 6.0, full.top()), pos2(x, full.top() + 8.0)];
        ui.painter().add(egui::Shape::convex_polygon(tri, theme::PLAYHEAD, Stroke::NONE));
    }
}

fn draw_lane(
    painter: &egui::Painter,
    ui: &Ui,
    session: &Session,
    settings: &Settings,
    state: &mut TimelineState,
    lane: &Lane,
    ppp: f32,
) {
    let track = &session.tracks[lane.index];
    let ts = session.project.track(lane.id);
    let view = &session.view;
    let wave_rect = lane.rect.shrink2(vec2(0.0, 14.0));
    let region_rect = |a: f64, b: f64| {
        Rect::from_min_max(
            pos2(lane.rect.left() + view.x_of(a) as f32, lane.rect.top() + 12.0),
            pos2(lane.rect.left() + view.x_of(b) as f32, lane.rect.bottom() - 12.0),
        )
    };
    let lane_clip = painter.with_clip_rect(lane.rect.intersect(painter.clip_rect()));

    // Region backgrounds (only on fixed tracks; other tracks keep their
    // regions but they are inactive).
    let visible: Vec<_> = session
        .project
        .regions
        .iter()
        .filter(|r| r.track == lane.id && r.kind == RegionKind::Breath && r.overlaps(view.start, view.end()))
        // Far from speech: hidden entirely ("Dyskryminacja").
        .filter(|r| r.near_speech(&session.project.discrimination))
        .collect();
    // Zoomed out, regions are a few pixels wide and there can be thousands
    // of them: those are drawn merged into spans (see `draw_merged`).
    let narrow_px = |r: &&&Region| (view.x_of(r.end) - view.x_of(r.start)) < f64::from(MERGE_BELOW_PX);
    let (narrow, wide): (Vec<&&Region>, Vec<&&Region>) =
        visible.iter().partition(|r| narrow_px(r) && session.selected_region != Some(r.id));
    // Channel layout of the lane: the mix, or one sub-lane per channel.
    let n_ch = usize::from(track.info.channels.max(1));
    let split = settings.split_channels && n_ch > 1;
    let sub_rect = |r: Rect, c: usize, n: usize| {
        let h = r.height() / n as f32;
        Rect::from_min_size(pos2(r.left(), r.top() + c as f32 * h), vec2(r.width(), h))
    };

    if settings.show_spectrogram {
        let spec_rect = lane.rect.shrink2(vec2(0.0, 6.0));
        if split {
            for c in 0..n_ch {
                let r = sub_rect(spec_rect, c, n_ch).shrink2(vec2(0.0, 1.0));
                state.spec.draw(ui.ctx(), &lane_clip, r, lane.index, Some(c), &track.store, view, ppp);
            }
        } else {
            state.spec.draw(ui.ctx(), &lane_clip, spec_rect, lane.index, None, &track.store, view, ppp);
        }
        if !ts.fix {
            lane_clip.rect_filled(spec_rect, CornerRadius::ZERO, Color32::from_black_alpha(110));
        }
    } else {
        for r in wide.iter().filter(|r| ts.fix && r.is_active_breath(&session.gate())) {
            let rect = region_rect(r.start, r.end);
            lane_clip.rect_filled(rect, CornerRadius::same(4), theme::region_fill(r.confidence.unwrap_or(1.0), true));
        }
    }

    // Selection range.
    if let Some(sel) = session.selection
        && sel.track.is_none_or(|t| t == lane.id)
    {
        let rect = Rect::from_min_max(
            pos2(lane.rect.left() + view.x_of(sel.start) as f32, lane.rect.top()),
            pos2(lane.rect.left() + view.x_of(sel.end) as f32, lane.rect.bottom()),
        );
        lane_clip.rect_filled(rect, CornerRadius::ZERO, theme::SELECTION);
        lane_clip.vline(rect.left(), rect.y_range(), Stroke::new(1.0, theme::ACCENT));
        lane_clip.vline(rect.right(), rect.y_range(), Stroke::new(1.0, theme::ACCENT));
    }

    let color = if ts.mute {
        theme::WAVE_MUTED
    } else if ts.fix {
        theme::WAVE_ACTIVE
    } else {
        theme::WAVE_INACTIVE
    };
    let style = WaveStyle {
        color,
        attenuated: if ts.mute { theme::WAVE_MUTED } else { theme::ACCENT },
        original: Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, 110),
    };
    if !settings.show_spectrogram {
        let envelope = (ts.fix && session.after).then(|| session.envelope_for(lane.id, track.info.sample_rate));
        let mix = WaveSource {
            store: &track.store,
            pyramid: &track.pyramid,
            channel: None,
            display_scale: track.display_scale,
        };
        match track.channel_pyramids.as_deref().filter(|_| split) {
            Some(pyramids) => {
                for (c, pyramid) in pyramids.iter().enumerate() {
                    let r = sub_rect(wave_rect, c, n_ch);
                    let src = WaveSource { pyramid, channel: Some(c), ..mix };
                    waveform::draw(&lane_clip, r, src, view, ppp, envelope.as_ref(), &style, &mut state.scratch);
                }
            }
            None => waveform::draw(&lane_clip, wave_rect, mix, view, ppp, envelope.as_ref(), &style, &mut state.scratch),
        }
    }
    if split {
        for c in 0..n_ch {
            let r = sub_rect(lane.rect.shrink2(vec2(0.0, 6.0)), c, n_ch);
            lane_clip.text(
                r.left_top() + vec2(6.0, 2.0),
                egui::Align2::LEFT_TOP,
                channel_label(c, n_ch),
                theme::mono(11.0),
                theme::TEXT_TERTIARY,
            );
        }
    }

    draw_merged(&lane_clip, lane, view, &narrow, |r| ts.fix && r.is_active_breath(&session.gate()), settings.show_spectrogram);

    // Region outlines on top of the waveform.
    for r in &wide {
        let active = ts.fix && r.is_active_breath(&session.gate());
        let rect = region_rect(r.start, r.end);
        let selected = session.selected_region == Some(r.id);
        if !active && !selected {
            // Below the sensitivity threshold: only a faint dashed hint that
            // a candidate exists (clickable, but clearly not a detection).
            let c = Color32::from_rgba_unmultiplied(0x5A, 0xC8, 0xFA, 70);
            let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
            lane_clip.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0, c), 3.0, 4.0));
            continue;
        }
        let stroke = if selected {
            Stroke::new(2.0, Color32::WHITE)
        } else if settings.show_spectrogram {
            Stroke::new(1.5, if active { theme::ACCENT } else { theme::region_border(false) })
        } else {
            Stroke::new(1.0, theme::region_border(active))
        };
        widgets::outline_rect(&lane_clip, rect, 5, stroke);
        if let Some(db) = r.gain_db
            && rect.width() >= GAIN_LABEL_MIN_PX
        {
            let label = if db.is_finite() { format!("{db:.0} dB") } else { "wycisz".into() };
            lane_clip.text(
                rect.center_top() + vec2(0.0, 4.0),
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::proportional(11.0),
                theme::ACCENT,
            );
        }
    }
}

/// Regions narrower than this (in points) are drawn merged.
const MERGE_BELOW_PX: f32 = 4.0;
/// Narrower regions have no room for their gain label.
const GAIN_LABEL_MIN_PX: f32 = 36.0;

/// Draws narrow regions as merged spans: runs of regions that touch or lie
/// within a pixel of each other become one rectangle. At that zoom single
/// regions cannot be told apart anyway, and drawing each one (with its
/// outline) costs more than the rest of the timeline.
fn draw_merged(
    painter: &egui::Painter,
    lane: &Lane,
    view: &TimelineView,
    regions: &[&&Region],
    active: impl Fn(&Region) -> bool,
    spectrogram: bool,
) {
    let (top, bottom) = (lane.rect.top() + 12.0, lane.rect.bottom() - 12.0);
    let mut spans: [Vec<(f32, f32, f32)>; 2] = [Vec::new(), Vec::new()];
    let mut sorted: Vec<_> = regions.iter().map(|r| (r, active(r))).collect();
    sorted.sort_by(|a, b| a.0.start.total_cmp(&b.0.start));
    for (r, is_active) in sorted {
        let x0 = lane.rect.left() + view.x_of(r.start) as f32;
        let x1 = (lane.rect.left() + view.x_of(r.end) as f32).max(x0 + 1.0);
        let conf = r.confidence.unwrap_or(1.0);
        let list = &mut spans[usize::from(is_active)];
        match list.last_mut() {
            Some(last) if x0 <= last.1 + 1.0 => {
                last.1 = last.1.max(x1);
                last.2 = last.2.max(conf);
            }
            _ => list.push((x0, x1, conf)),
        }
    }
    for (is_active, list) in [(false, &spans[0]), (true, &spans[1])] {
        for &(x0, x1, conf) in list {
            let rect = Rect::from_min_max(pos2(x0, top), pos2(x1, bottom));
            if !spectrogram || !is_active {
                painter.rect_filled(rect, CornerRadius::ZERO, theme::region_fill(conf, is_active));
            }
            let stroke = if spectrogram && is_active {
                Stroke::new(1.5, theme::ACCENT)
            } else {
                Stroke::new(1.0, theme::region_border(is_active))
            };
            painter.rect_stroke(rect, CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
        }
    }
}

/// A small "Dodaj wdech" action next to a fresh selection on a track that
/// is being fixed (the keyboard shortcut B does the same).
fn add_breath_button(ui: &mut Ui, session: &mut Session, state: &TimelineState, lanes: &[Lane], body: Rect) {
    if state.drag.is_some() {
        return;
    }
    let Some(sel) = session.selection.filter(|s| s.duration() >= 0.01) else { return };
    let Some(track) = sel.track else { return };
    if !session.project.track(track).fix {
        return;
    }
    let Some(lane) = lanes.iter().find(|l| l.id == track) else { return };
    let x0 = lane.rect.left() + session.view.x_of(sel.start) as f32;
    let x1 = lane.rect.left() + session.view.x_of(sel.end) as f32;
    let size = vec2(136.0, 28.0);
    // Right of the selection's top edge, kept inside the lane.
    let x = (x1 + 6.0).min(lane.rect.right() - size.x - 6.0).max(x0.max(lane.rect.left()) + 6.0);
    let rect = Rect::from_min_size(pos2(x, lane.rect.top() + 6.0), size);
    if !body.contains_rect(rect) {
        return;
    }
    let button = egui::Button::new(egui::RichText::new("+ Dodaj wdech").size(14.0).color(theme::ON_ACCENT))
        .wrap_mode(egui::TextWrapMode::Extend)
        .fill(theme::ACCENT)
        .corner_radius(CornerRadius::same(7));
    if ui.put(rect, button).on_hover_text("Oznacz zaznaczony fragment jako wdech (B)").clicked() {
        session.add_region_from_selection();
    }
}

fn channel_label(c: usize, n: usize) -> String {
    match (n, c) {
        (2, 0) => "L".into(),
        (2, 1) => "R".into(),
        _ => format!("{}", c + 1),
    }
}

fn draw_header(ui: &mut Ui, session: &mut Session, clip: Rect, rect: Rect, index: usize) {
    if clip.height() <= 0.0 {
        return;
    }
    let id = session.tracks[index].info.id;
    let name = session.tracks[index].info.display_name();
    let ts = session.project.track(id);
    let dim = !ts.fix;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(22.0, 18.0)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(clip);
    let text_color = if dim { theme::TEXT_SECONDARY } else { theme::TEXT };
    child.add(egui::Label::new(egui::RichText::new(&name).size(16.0).color(text_color)).wrap());
    child.add_space(4.0);
    let mut mix_changed = false;
    child.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if widgets::square_toggle(ui, "M", ts.mute, theme::MUTE_ON, "Wycisz ścieżkę (Mute)").clicked() {
            session.edit_silently(|p| p.track_mut(id).mute = !ts.mute);
            mix_changed = true;
        }
        if widgets::square_toggle(ui, "S", ts.solo, theme::SOLO_ON, "Tylko ta ścieżka (Solo)").clicked() {
            session.edit_silently(|p| p.track_mut(id).solo = !ts.solo);
            mix_changed = true;
        }
    });
    child.add_space(4.0);
    child.horizontal(|ui| {
        let mut fix = ts.fix;
        if widgets::toggle(ui, &mut fix).on_hover_text("Wykrywaj i ściszaj wdechy na tej ścieżce").changed() {
            session.set_fix(id, fix);
        }
        let color = if fix { theme::TEXT } else { theme::TEXT_SECONDARY };
        ui.label(egui::RichText::new("Popraw wdechy").size(15.0).color(color));
    });
    let _ = mix_changed;
}

fn draw_ruler(ui: &mut Ui, session: &mut Session, rect: Rect, state: &mut TimelineState) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
    painter.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme::SEPARATOR));
    let view = session.view;
    let step = nice_tick_step(view.secs_per_px * 110.0);
    let minor = step / 5.0;
    let first = (view.start / minor).floor() as i64;
    let last = (view.end() / minor).ceil() as i64;
    for k in first..=last {
        let t = k as f64 * minor;
        if t < 0.0 {
            continue;
        }
        let x = rect.left() + view.x_of(t) as f32;
        let is_major = (k % 5) == 0;
        let h = if is_major { 8.0 } else { 4.0 };
        painter.vline(x, rect.bottom() - h..=rect.bottom(), Stroke::new(1.0, theme::TEXT_TERTIARY));
        if is_major {
            painter.text(
                pos2(x + 4.0, rect.top() + 12.0),
                egui::Align2::LEFT_TOP,
                format_ruler_label(t, step),
                theme::mono(13.0),
                theme::TEXT_SECONDARY,
            );
        }
    }

    let resp = ui.interact(rect, ui.id().with("ruler"), Sense::click_and_drag());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    if resp.drag_started_by(PointerButton::Primary) {
        state.drag = Some(Drag::Scrub);
    }
    if (resp.clicked() || (state.drag == Some(Drag::Scrub) && resp.dragged()))
        && let Some(p) = resp.interact_pointer_pos()
    {
        session.seek(view.t_of(f64::from(p.x - rect.left())));
    }
    if resp.drag_stopped() && state.drag == Some(Drag::Scrub) {
        state.drag = None;
    }
}

fn draw_scrollbar(ui: &mut Ui, session: &mut Session, rect: Rect, state: &mut TimelineState) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
    let view = session.view;
    let total = view.duration.max(view.visible_secs());
    let frac0 = (view.start.max(0.0) / total) as f32;
    let frac1 = ((view.end().min(total)) / total) as f32;
    let track_rect = rect.shrink2(vec2(4.0, 3.0));
    let thumb = Rect::from_min_max(
        pos2(track_rect.left() + frac0 * track_rect.width(), track_rect.top()),
        pos2((track_rect.left() + frac1 * track_rect.width()).max(track_rect.left() + frac0 * track_rect.width() + 20.0), track_rect.bottom()),
    );
    let resp = ui.interact(rect, ui.id().with("hscroll"), Sense::click_and_drag());
    let active = resp.hovered() || matches!(state.drag, Some(Drag::Thumb { .. }));
    let color = if active { Color32::from_gray(0x70) } else { Color32::from_gray(0x4A) };
    painter.rect_filled(thumb, CornerRadius::same(4), color);

    let t_at = |x: f32| f64::from((x - track_rect.left()) / track_rect.width()) * total;
    if resp.drag_started_by(PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let grab_offset = if thumb.contains(p) { t_at(p.x) - view.start } else { view.visible_secs() / 2.0 };
        state.drag = Some(Drag::Thumb { grab_offset });
    }
    if let Some(Drag::Thumb { grab_offset }) = state.drag {
        if let Some(p) = resp.interact_pointer_pos() {
            session.view.start = t_at(p.x) - grab_offset;
            session.view.clamp();
        }
        if resp.drag_stopped() {
            state.drag = None;
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        session.view.start = t_at(p.x) - view.visible_secs() / 2.0;
        session.view.clamp();
    }
}

fn handle_wheel(ui: &Ui, session: &mut Session, state: &mut TimelineState, area: Rect, body: Rect, content_h: f32) {
    let Some(pointer) = ui.ctx().pointer_hover_pos() else { return };
    if !area.contains(pointer) && !body.contains(pointer) {
        return;
    }
    // Raw wheel events give exact control over modifiers (egui would
    // otherwise turn Ctrl+wheel into UI zoom and Shift+wheel into x-scroll).
    let line = 40.0;
    let page = body.height();
    let mut wheel = egui::Vec2::ZERO;
    let mut mods = egui::Modifiers::NONE;
    let mut pinch = 1.0f32;
    ui.input(|i| {
        for e in &i.raw.events {
            match e {
                egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => line,
                        egui::MouseWheelUnit::Page => page,
                    };
                    wheel += *delta * scale;
                    mods = *modifiers;
                }
                egui::Event::Zoom(f) => pinch *= f,
                _ => {}
            }
        }
    });
    let x = f64::from(pointer.x - area.left());
    if pinch != 1.0 && area.contains(pointer) {
        session.view.zoom_at(x, f64::from(pinch));
    }
    if wheel == egui::Vec2::ZERO {
        return;
    }
    if !area.contains(pointer) {
        // Over the track headers: scroll rows vertically.
        if content_h > body.height() {
            state.v_scroll -= wheel.y;
        }
        return;
    }
    if mods.shift || wheel.x.abs() > wheel.y.abs() {
        let d = if wheel.x.abs() > wheel.y.abs() { wheel.x } else { wheel.y };
        session.view.scroll_px(-f64::from(d));
    } else if mods.alt && content_h > body.height() {
        state.v_scroll -= wheel.y;
    } else {
        // Plain wheel and Ctrl+wheel both zoom around the cursor.
        session.view.zoom_at(x, (f64::from(wheel.y) * 0.004).exp());
    }
}

/// Region-aware hit testing inside a lane.
enum Hit {
    Edge(RegionId, Side),
    Body(RegionId),
    Empty,
}

fn hit_test(session: &Session, lane: &Lane, p: egui::Pos2) -> Hit {
    let view = &session.view;
    let mut best: Option<(f32, RegionId, Side)> = None;
    let mut body = None;
    let d = session.project.discrimination;
    for r in session.project.regions.iter().filter(|r| r.track == lane.id && r.kind == RegionKind::Breath && r.near_speech(&d)) {
        let x0 = lane.rect.left() + view.x_of(r.start) as f32;
        let x1 = lane.rect.left() + view.x_of(r.end) as f32;
        for (x, side) in [(x0, Side::Start), (x1, Side::End)] {
            let d = (p.x - x).abs();
            // Very narrow regions: prefer the body in the middle third.
            if d <= EDGE_GRAB_PX && (x1 - x0 > 3.0 * EDGE_GRAB_PX || (p.x - (x0 + x1) / 2.0).abs() > (x1 - x0) / 6.0)
                && best.is_none_or(|(bd, _, _)| d < bd)
            {
                best = Some((d, r.id, side));
            }
        }
        if p.x >= x0 && p.x <= x1 {
            body = Some(r.id);
        }
    }
    match (best, body) {
        (Some((_, id, side)), _) => Hit::Edge(id, side),
        (None, Some(id)) => Hit::Body(id),
        _ => Hit::Empty,
    }
}

fn lane_interaction(
    ui: &Ui,
    session: &mut Session,
    settings: &Settings,
    state: &mut TimelineState,
    lanes: &[Lane],
    resp: &egui::Response,
) {
    let lane_at = |p: egui::Pos2| lanes.iter().find(|l| l.rect.contains(p));
    let area_left = lanes.first().map_or(0.0, |l| l.rect.left());
    let t_at = |x: f32, s: &Session| s.view.t_of(f64::from(x - area_left));

    // Hover feedback.
    if state.drag.is_none()
        && let Some(p) = resp.hover_pos()
        && let Some(lane) = lane_at(p)
    {
        match hit_test(session, lane, p) {
            Hit::Edge(..) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            Hit::Body(id) => {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                if let Some(r) = session.project.region(id) {
                    let db = r.gain_db.unwrap_or(session.gain_db());
                    let conf = r.confidence.map(|c| format!("\npewność modelu: {:.0}%", c * 100.0)).unwrap_or_default();
                    resp.clone().on_hover_text_at_pointer(format!(
                        "Wdech {:.2} s · {}{}\nPrzeciągnij w pionie, aby zmienić ściszenie",
                        r.duration(),
                        if db.is_finite() { format!("{db:.0} dB") } else { "wyciszony".into() },
                        conf
                    ));
                }
            }
            Hit::Empty => {}
        }
    }

    // Middle-button pan.
    if resp.drag_started_by(PointerButton::Middle) {
        state.drag = Some(Drag::Pan);
    }
    if state.drag == Some(Drag::Pan) {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        session.view.scroll_px(-f64::from(resp.drag_delta().x));
        if resp.drag_stopped() {
            state.drag = None;
        }
        return;
    }

    if resp.drag_started_by(PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
        if let Some(lane) = lane_at(origin) {
            let t = t_at(origin.x, session);
            state.drag = Some(match hit_test(session, lane, origin) {
                Hit::Edge(region, side) => {
                    session.begin_gesture();
                    session.selected_region = Some(region);
                    Drag::Edge { region, side }
                }
                Hit::Body(region) => Drag::PendingRegion { region, track: lane.id, anchor: t },
                Hit::Empty => Drag::Select { track: lane.id, anchor: t },
            });
        }
    }

    if resp.dragged_by(PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
    {
        let t = t_at(p.x, session).clamp(0.0, session.duration());
        match state.drag {
            Some(Drag::PendingRegion { region, track, anchor }) => {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                let d = p - origin;
                if d.length() > DRAG_THRESHOLD {
                    if d.y.abs() > d.x.abs() {
                        let start_db = session
                            .project
                            .region(region)
                            .and_then(|r| r.gain_db)
                            .unwrap_or_else(|| session.gain_db());
                        session.begin_gesture();
                        session.selected_region = Some(region);
                        state.drag = Some(Drag::Gain { region, start_db: start_db.max(-60.0) });
                    } else {
                        state.drag = Some(Drag::Select { track, anchor });
                    }
                }
            }
            Some(Drag::Select { track, anchor }) => {
                session.selected_region = None;
                session.selection = Some(Selection::normalized(anchor, t, Some(track)));
            }
            Some(Drag::Edge { region, side }) => {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                session.edit_silently(|p| {
                    if let Some(r) = p.region_mut(region) {
                        const MIN_LEN: f64 = 0.005;
                        match side {
                            Side::Start => r.start = t.min(r.end - MIN_LEN),
                            Side::End => r.end = t.max(r.start + MIN_LEN),
                        }
                        r.source = RegionSource::User;
                    }
                });
            }
            Some(Drag::Gain { region, start_db }) => {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                // Dragging down attenuates more.
                let db = (start_db - (p.y - origin.y) * DB_PER_PX).clamp(-60.0, 0.0);
                let db = if db <= -60.0 { f32::NEG_INFINITY } else { db.round() };
                session.edit_silently(|p| {
                    if let Some(r) = p.region_mut(region) {
                        r.gain_db = Some(db);
                    }
                });
                let label = if db.is_finite() { format!("{db:.0} dB") } else { "wyciszony".into() };
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    ui.id().with("gain_tip"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| ui.label(label));
            }
            _ => {}
        }
    }

    if resp.drag_stopped_by(PointerButton::Primary) {
        match state.drag.take() {
            Some(Drag::PendingRegion { region, .. }) => session.go_to_region(region, settings),
            Some(Drag::Select { .. }) => {
                if session.selection.is_some_and(|s| s.duration() < 1e-3) {
                    session.selection = None;
                }
            }
            _ => {}
        }
    }

    if resp.clicked_by(PointerButton::Primary)
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(lane) = lane_at(p)
    {
        match hit_test(session, lane, p) {
            Hit::Body(id) | Hit::Edge(id, _) => session.go_to_region(id, settings),
            Hit::Empty => {
                session.selection = None;
                session.selected_region = None;
                session.seek(t_at(p.x, session));
            }
        }
    }
}
