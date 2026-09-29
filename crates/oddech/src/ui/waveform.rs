//! Waveform rendering for one track lane.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};
use oddech_core::gain::GainEnvelope;
use oddech_core::pyramid::{BASE_BUCKET, MinMax, WaveformPyramid, columns_from_raw};
use oddech_core::store::TrackStore;

use crate::timeline::TimelineView;

/// What to draw: one channel or the mix of a track.
#[derive(Clone, Copy)]
pub struct WaveSource<'a> {
    pub store: &'a TrackStore,
    pub pyramid: &'a WaveformPyramid,
    /// `None` = mono mix.
    pub channel: Option<usize>,
    pub display_scale: f32,
}

/// Reusable buffers so drawing does not allocate every frame.
#[derive(Default)]
pub struct WaveScratch {
    cols: Vec<MinMax>,
    raw: Vec<f32>,
    gains: Vec<f32>,
}

pub struct WaveStyle {
    pub color: Color32,
    /// Colour of attenuated parts in "Po" mode.
    pub attenuated: Color32,
    /// Outline of the original signal where attenuated.
    pub original: Color32,
}

/// Draws the waveform of `track` into `rect`. With `envelope` given, the
/// waveform is drawn as it will sound (attenuated) and the original is
/// shown as a faint outline.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    track: WaveSource<'_>,
    view: &TimelineView,
    ppp: f32,
    envelope: Option<&GainEnvelope>,
    style: &WaveStyle,
    scratch: &mut WaveScratch,
) {
    let rate = f64::from(track.store.rate());
    let px = 1.0 / ppp; // one physical pixel in points
    let n_cols = (rect.width() * ppp).ceil().max(1.0) as usize;
    let x0 = rect.left();
    // Lane rect starts at view.x == 0.
    let secs_per_col = view.secs_per_px * f64::from(px);
    let spp = secs_per_col * rate;
    let first = view.t_of(0.0) * rate;
    let mid = rect.center().y;
    let half = rect.height() * 0.45 * track.display_scale;

    if spp < 2.0 {
        draw_samples(painter, rect, track, view, envelope, style, scratch);
        return;
    }

    if spp >= BASE_BUCKET as f64 {
        track.pyramid.columns(first, spp, n_cols, &mut scratch.cols);
    } else {
        let s0 = first.floor() as i64 - 1;
        let s1 = (first + spp * n_cols as f64).ceil() as i64 + 2;
        scratch.raw.resize((s1 - s0).max(0) as usize, 0.0);
        track.store.read_view(track.channel, s0, &mut scratch.raw);
        columns_from_raw(&scratch.raw, s0, first, spp, n_cols, &mut scratch.cols);
    }

    scratch.gains.clear();
    if let Some(env) = envelope {
        for c in 0..n_cols {
            let a = (first + c as f64 * spp).floor() as i64;
            let b = (first + (c + 1) as f64 * spp).ceil() as i64;
            scratch.gains.push(env.min_gain_in(a, b.max(a + 1)));
        }
    }

    let mut mesh = Mesh::default();
    mesh.reserve_vertices(n_cols * 2);
    mesh.reserve_triangles(n_cols * 2);
    let mut outline_top: Vec<Pos2> = Vec::new();
    let mut outline_bottom: Vec<Pos2> = Vec::new();
    let mut outlines: Vec<(Vec<Pos2>, Vec<Pos2>)> = Vec::new();
    let mut prev_valid = false;

    for (c, mm) in scratch.cols.iter().enumerate() {
        let x = x0 + c as f32 * px;
        if mm.is_empty() {
            prev_valid = false;
            continue;
        }
        let g = scratch.gains.get(c).copied().unwrap_or(1.0);
        let shown = mm.scaled(g);
        let (mut top, mut bottom) = (mid - shown.max * half, mid - shown.min * half);
        // Keep silence visible as a hairline.
        if bottom - top < px {
            let m = (top + bottom) / 2.0;
            top = m - px / 2.0;
            bottom = m + px / 2.0;
        }
        let top = top.max(rect.top());
        let bottom = bottom.min(rect.bottom());
        let color = if g < 0.999 { style.attenuated } else { style.color };
        let idx = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex { pos: pos2(x, top), uv: WHITE_UV, color });
        mesh.vertices.push(Vertex { pos: pos2(x, bottom), uv: WHITE_UV, color });
        if prev_valid {
            mesh.indices.extend_from_slice(&[idx - 2, idx - 1, idx, idx - 1, idx + 1, idx]);
        }
        prev_valid = true;

        if g < 0.999 {
            outline_top.push(pos2(x, (mid - mm.max * half).max(rect.top())));
            outline_bottom.push(pos2(x, (mid - mm.min * half).min(rect.bottom())));
        } else if !outline_top.is_empty() {
            outlines.push((std::mem::take(&mut outline_top), std::mem::take(&mut outline_bottom)));
        }
    }
    if !outline_top.is_empty() {
        outlines.push((outline_top, outline_bottom));
    }
    // A single column would not form a triangle; widen it.
    if mesh.vertices.len() == 2 {
        let v = mesh.vertices.clone();
        for mut vx in v {
            vx.pos.x += px;
            mesh.vertices.push(vx);
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
    }
    painter.add(mesh);

    let stroke = Stroke::new(1.0, style.original);
    for (top, bottom) in outlines {
        painter.add(egui::Shape::dashed_line(&top, stroke, 3.0, 3.0));
        painter.add(egui::Shape::dashed_line(&bottom, stroke, 3.0, 3.0));
    }
}

/// Deep zoom: individual samples connected by lines, with dots when there
/// is enough room.
fn draw_samples(
    painter: &Painter,
    rect: Rect,
    track: WaveSource<'_>,
    view: &TimelineView,
    envelope: Option<&GainEnvelope>,
    style: &WaveStyle,
    scratch: &mut WaveScratch,
) {
    let rate = f64::from(track.store.rate());
    let s0 = (view.start * rate).floor() as i64 - 1;
    let s1 = (view.end() * rate).ceil() as i64 + 2;
    let n = (s1 - s0).max(0) as usize;
    scratch.raw.resize(n, 0.0);
    track.store.read_view(track.channel, s0, &mut scratch.raw);
    let mid = rect.center().y;
    let half = rect.height() * 0.45 * track.display_scale;
    let px_per_sample = 1.0 / (view.secs_per_px * rate);
    let len = track.store.len() as i64;
    let mut pts = Vec::with_capacity(n);
    let mut original = Vec::new();
    for (i, &x) in scratch.raw.iter().enumerate() {
        let s = s0 + i as i64;
        if s < 0 || s >= len {
            continue;
        }
        let t = s as f64 / rate;
        let px = rect.left() + view.x_of(t) as f32;
        let g = envelope.map_or(1.0, |e| e.gain_at(s));
        pts.push(pos2(px, (mid - x * g * half).clamp(rect.top(), rect.bottom())));
        if g < 0.999 {
            original.push(pos2(px, (mid - x * half).clamp(rect.top(), rect.bottom())));
        }
    }
    if !original.is_empty() {
        painter.add(egui::Shape::dashed_line(&original, Stroke::new(1.0, style.original), 3.0, 3.0));
    }
    painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.5, style.color)));
    if px_per_sample > 6.0 {
        for p in pts {
            painter.circle_filled(p, 2.0, style.color);
        }
    }
    painter.hline(rect.x_range(), mid, Stroke::new(1.0, Color32::from_white_alpha(12)));
}
