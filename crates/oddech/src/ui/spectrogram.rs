//! Spectrogram tiles: requested on demand, rendered on the rayon pool,
//! uploaded as textures, evicted least-recently-used.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2};
use oddech_core::spectrogram::{self, SpecParams, SpecWorker, TILE_COLS, TILE_ROWS};
use oddech_core::store::TrackStore;

use crate::settings::{Colormap, SpectrogramSettings};
use crate::timeline::TimelineView;

/// ~256 KiB of GPU memory per tile; 384 tiles ≈ 100 MiB.
const MAX_TILES: usize = 384;
const MAX_IN_FLIGHT: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TileKey {
    track: usize,
    /// `None` = mono mix.
    channel: Option<u8>,
    level: u32,
    tile: u64,
}

struct Tile {
    texture: TextureHandle,
    last_used: u64,
}

type Rendered = (u64, TileKey, Vec<[u8; 4]>);

pub struct SpectrogramCache {
    generation: u64,
    params: Option<SpecParams>,
    tiles: HashMap<TileKey, Tile>,
    pending: HashSet<TileKey>,
    tx: Sender<Rendered>,
    rx: Receiver<Rendered>,
    frame: u64,
}

impl Default for SpectrogramCache {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { generation: 0, params: None, tiles: HashMap::new(), pending: HashSet::new(), tx, rx, frame: 0 }
    }
}

pub fn params_from(settings: &SpectrogramSettings) -> SpecParams {
    SpecParams {
        fft_size: settings.fft_size,
        log_frequency: settings.log_frequency,
        db_range: settings.db_range,
        colormap: match settings.colormap {
            Colormap::Magma => spectrogram::Colormap::Magma,
            Colormap::Viridis => spectrogram::Colormap::Viridis,
            Colormap::Gray => spectrogram::Colormap::Gray,
        },
    }
}

thread_local! {
    static WORKER: RefCell<Option<SpecWorker>> = const { RefCell::new(None) };
}

impl SpectrogramCache {
    /// Call once per frame before drawing: applies parameter changes and
    /// uploads finished tiles.
    pub fn begin_frame(&mut self, ctx: &egui::Context, params: SpecParams) {
        self.frame += 1;
        if self.params.map(|p| p.cache_key()) != Some(params.cache_key()) {
            self.params = Some(params);
            self.generation += 1;
            self.tiles.clear();
            self.pending.clear();
        }
        while let Ok((generation, key, pixels)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.pending.remove(&key);
            let image = ColorImage::from_rgba_unmultiplied([TILE_COLS, TILE_ROWS], bytemuck_flat(&pixels));
            let name = format!("spec-{}-{:?}-{}-{}", key.track, key.channel, key.level, key.tile);
            let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
            self.tiles.insert(key, Tile { texture, last_used: self.frame });
        }
        if self.tiles.len() > MAX_TILES {
            let mut ages: Vec<(u64, TileKey)> = self.tiles.iter().map(|(k, t)| (t.last_used, *k)).collect();
            ages.sort_unstable_by_key(|(age, _)| *age);
            for (_, k) in ages.into_iter().take(self.tiles.len() - MAX_TILES) {
                self.tiles.remove(&k);
            }
        }
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn request(&mut self, ctx: &egui::Context, store: &Arc<TrackStore>, key: TileKey) {
        if self.pending.len() >= MAX_IN_FLIGHT || self.pending.contains(&key) {
            return;
        }
        let Some(params) = self.params else { return };
        self.pending.insert(key);
        let (tx, generation, store, ctx) = (self.tx.clone(), self.generation, Arc::clone(store), ctx.clone());
        rayon::spawn(move || {
            let pixels = WORKER.with(|w| {
                let mut w = w.borrow_mut();
                if w.as_ref().is_none_or(|w| w.fft_size() != params.fft_size) {
                    *w = Some(SpecWorker::new(params.fft_size));
                }
                w.as_mut().unwrap().render_tile(&store, key.channel.map(usize::from), &params, key.level, key.tile)
            });
            let _ = tx.send((generation, key, pixels));
            ctx.request_repaint();
        });
    }

    /// Draws the spectrogram of one track into `rect` (whose left edge is
    /// timeline x = 0).
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        rect: Rect,
        track: usize,
        channel: Option<usize>,
        store: &Arc<TrackStore>,
        view: &TimelineView,
        ppp: f32,
    ) {
        let Some(params) = self.params else { return };
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        let rate = f64::from(store.rate());
        let spp = view.secs_per_px / f64::from(ppp) * rate;
        let level = spectrogram::level_for(&params, spp);
        let tile_samples = |level: u32| (params.hop(level) * TILE_COLS) as f64;
        let first = ((view.start * rate).max(0.0) / tile_samples(level)).floor() as u64;
        let last_sample = (view.end() * rate).min(store.len() as f64);
        if last_sample <= 0.0 {
            return;
        }
        let last = (last_sample / tile_samples(level)).floor() as u64;
        let x_of = |sample: f64| rect.left() + view.x_of(sample / rate) as f32;

        for tile in first..=last {
            let channel = channel.map(|c| c as u8);
            let key = TileKey { track, channel, level, tile };
            let s0 = tile as f64 * tile_samples(level);
            let s1 = s0 + tile_samples(level);
            let dst = Rect::from_min_max(pos2(x_of(s0), rect.top()), pos2(x_of(s1), rect.bottom()));
            if let Some(t) = self.tiles.get_mut(&key) {
                t.last_used = self.frame;
                painter.image(t.texture.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                continue;
            }
            self.request(ctx, store, key);
            // Meanwhile, stretch a coarser tile if one is available.
            for up in 1..=6 {
                let coarse_level = level + up;
                let coarse_tile = (s0 / tile_samples(coarse_level)).floor() as u64;
                let key = TileKey { track, channel, level: coarse_level, tile: coarse_tile };
                if let Some(t) = self.tiles.get_mut(&key) {
                    t.last_used = self.frame;
                    let c0 = coarse_tile as f64 * tile_samples(coarse_level);
                    let span = tile_samples(coarse_level);
                    let u0 = ((s0 - c0) / span) as f32;
                    let u1 = ((s1 - c0) / span) as f32;
                    painter.image(t.texture.id(), dst, Rect::from_min_max(pos2(u0, 0.0), pos2(u1, 1.0)), Color32::WHITE);
                    break;
                }
            }
        }
    }
}

fn bytemuck_flat(pixels: &[[u8; 4]]) -> &[u8] {
    pixels.as_flattened()
}
