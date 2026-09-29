//! Tiled, multi-resolution spectrogram rendering.
//!
//! The timeline is divided into tiles of [`TILE_COLS`] columns. At level
//! `L` one column spans `base_hop · 2^L` samples. To keep coarse levels
//! honest (a 300 ms breath must not vanish between two sparse FFT frames),
//! each column averages the power of several windows spread across its span.
//! Tiles are computed on demand and independently, so they parallelize
//! trivially and only visible tiles are ever computed.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

use crate::store::TrackStore;

pub const TILE_COLS: usize = 256;
pub const TILE_ROWS: usize = 256;
/// Most FFT windows averaged into one column at coarse levels.
const MAX_WINDOWS_PER_COL: usize = 12;
/// Lowest frequency shown on the logarithmic scale.
const LOG_MIN_HZ: f32 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colormap {
    Magma,
    Viridis,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecParams {
    pub fft_size: usize,
    pub log_frequency: bool,
    pub db_range: f32,
    pub colormap: Colormap,
}

impl SpecParams {
    /// Column step at level 0 (samples).
    pub fn base_hop(&self) -> usize {
        (self.fft_size / 4).max(32)
    }

    pub fn hop(&self, level: u32) -> usize {
        self.base_hop() << level
    }

    /// A key that changes whenever the rendered image would change.
    pub fn cache_key(&self) -> (usize, bool, u32, Colormap) {
        (self.fft_size, self.log_frequency, self.db_range.to_bits(), self.colormap)
    }
}

/// Level whose column step best matches `samples_per_px` (one column per
/// physical pixel, never coarser).
pub fn level_for(params: &SpecParams, samples_per_px: f64) -> u32 {
    let mut level = 0;
    while level < 24 && (params.hop(level + 1) as f64) <= samples_per_px {
        level += 1;
    }
    level
}

/// Reusable FFT state for one worker thread.
pub struct SpecWorker {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    scratch: Vec<realfft::num_complex::Complex<f32>>,
    output: Vec<realfft::num_complex::Complex<f32>>,
    power: Vec<f32>,
    mix: Vec<f32>,
}

impl SpecWorker {
    pub fn new(fft_size: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(fft_size);
        // Periodic Hann window, normalized so a full-scale sine reads 0 dB.
        let window: Vec<f32> = (0..fft_size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / fft_size as f32).cos())
            .collect();
        Self {
            input: fft.make_input_vec(),
            scratch: fft.make_scratch_vec(),
            output: fft.make_output_vec(),
            power: vec![0.0; fft_size / 2 + 1],
            mix: Vec::new(),
            fft,
            window,
        }
    }

    pub fn fft_size(&self) -> usize {
        self.window.len()
    }

    /// Renders one tile as RGBA (`TILE_COLS × TILE_ROWS`, row 0 = highest
    /// frequency, so the image can be drawn top-down directly).
    /// `channel` selects one channel; `None` analyses the mono mix.
    pub fn render_tile(
        &mut self,
        store: &TrackStore,
        channel: Option<usize>,
        params: &SpecParams,
        level: u32,
        tile: u64,
    ) -> Vec<[u8; 4]> {
        let n = self.fft_size();
        let bins = n / 2 + 1;
        let hop = params.hop(level);
        let windows_per_col = (hop / params.base_hop()).clamp(1, MAX_WINDOWS_PER_COL);
        let rate = store.rate() as f32;
        let rows = row_bins(params, rate, bins);
        let lut = colormap_lut(params.colormap);
        // Hann window coherent gain is 0.5; power reference for a full-scale
        // sine: (n/2 · 0.5)^2.
        let reference = (n as f32 * 0.25).powi(2);

        let mut img = vec![[0u8, 0, 0, 255]; TILE_COLS * TILE_ROWS];
        let first_col = tile * TILE_COLS as u64;
        let mut col_power = vec![0.0f32; bins];
        for c in 0..TILE_COLS {
            let col_start = (first_col + c as u64) as i64 * hop as i64;
            if col_start >= store.len() as i64 {
                break;
            }
            col_power.fill(0.0);
            for w in 0..windows_per_col {
                // Window centres spread evenly over the column's span.
                let centre = col_start + ((w as f64 + 0.5) * hop as f64 / windows_per_col as f64) as i64;
                self.window_power(store, channel, centre - (n / 2) as i64);
                for (acc, p) in col_power.iter_mut().zip(&self.power) {
                    *acc += p;
                }
            }
            let scale = 1.0 / (windows_per_col as f32 * reference);
            for (r, &(b0, b1)) in rows.iter().enumerate() {
                let mut p = 0.0f32;
                for b in b0..=b1 {
                    p = p.max(col_power[b]);
                }
                let db = 10.0 * (p * scale).max(1e-20).log10();
                let v = ((db + params.db_range) / params.db_range).clamp(0.0, 1.0);
                img[(TILE_ROWS - 1 - r) * TILE_COLS + c] = lut[(v * 255.0) as usize];
            }
        }
        img
    }

    fn window_power(&mut self, store: &TrackStore, channel: Option<usize>, start: i64) {
        let n = self.fft_size();
        self.mix.resize(n, 0.0);
        store.read_view(channel, start, &mut self.mix);
        for ((i, m), w) in self.input.iter_mut().zip(&self.mix).zip(&self.window) {
            *i = m * w;
        }
        // Lengths always match the plan, so this cannot fail.
        let _ = self.fft.process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch);
        for (p, c) in self.power.iter_mut().zip(&self.output) {
            *p = c.norm_sqr();
        }
    }
}

/// For each display row (bottom = 0), the inclusive range of FFT bins it
/// covers. Rows narrower than a bin map to the nearest bin.
fn row_bins(params: &SpecParams, rate: f32, bins: usize) -> Vec<(usize, usize)> {
    let nyquist = rate / 2.0;
    let hz_per_bin = nyquist / (bins - 1) as f32;
    let freq_at = |row: f32| -> f32 {
        let x = row / TILE_ROWS as f32;
        if params.log_frequency {
            LOG_MIN_HZ * (nyquist / LOG_MIN_HZ).powf(x)
        } else {
            x * nyquist
        }
    };
    (0..TILE_ROWS)
        .map(|r| {
            let lo = (freq_at(r as f32) / hz_per_bin).floor() as usize;
            let hi = (freq_at(r as f32 + 1.0) / hz_per_bin).ceil() as usize;
            let lo = lo.min(bins - 1);
            (lo, hi.saturating_sub(1).clamp(lo, bins - 1))
        })
        .collect()
}

/// Frequency (Hz) at a vertical position `y` in `[0, 1]` (0 = bottom).
pub fn frequency_at(params: &SpecParams, rate: u32, y: f32) -> f32 {
    let nyquist = rate as f32 / 2.0;
    if params.log_frequency { LOG_MIN_HZ * (nyquist / LOG_MIN_HZ).powf(y) } else { y * nyquist }
}

/// 256-entry RGBA lookup table.
pub fn colormap_lut(map: Colormap) -> Vec<[u8; 4]> {
    // Control points (position, r, g, b) approximating the matplotlib maps.
    const MAGMA: &[(f32, [f32; 3])] = &[
        (0.0, [0.0, 0.0, 0.02]),
        (0.13, [0.11, 0.07, 0.27]),
        (0.25, [0.31, 0.07, 0.48]),
        (0.38, [0.51, 0.15, 0.51]),
        (0.5, [0.71, 0.21, 0.48]),
        (0.63, [0.90, 0.32, 0.39]),
        (0.75, [0.98, 0.52, 0.37]),
        (0.88, [0.99, 0.75, 0.52]),
        (1.0, [0.99, 0.99, 0.75]),
    ];
    const VIRIDIS: &[(f32, [f32; 3])] = &[
        (0.0, [0.27, 0.0, 0.33]),
        (0.25, [0.23, 0.32, 0.55]),
        (0.5, [0.13, 0.57, 0.55]),
        (0.75, [0.37, 0.79, 0.38]),
        (1.0, [0.99, 0.91, 0.14]),
    ];
    const GRAY: &[(f32, [f32; 3])] = &[(0.0, [0.0; 3]), (1.0, [1.0; 3])];
    let points = match map {
        Colormap::Magma => MAGMA,
        Colormap::Viridis => VIRIDIS,
        Colormap::Gray => GRAY,
    };
    (0..256)
        .map(|i| {
            let x = i as f32 / 255.0;
            let k = points.windows(2).position(|w| x <= w[1].0).unwrap_or(points.len() - 2);
            let (x0, c0) = points[k];
            let (x1, c1) = points[k + 1];
            let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            let ch = |j: usize| ((c0[j] + (c1[j] - c0[j]) * t) * 255.0).round() as u8;
            [ch(0), ch(1), ch(2), 255]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(log: bool) -> SpecParams {
        SpecParams { fft_size: 1024, log_frequency: log, db_range: 80.0, colormap: Colormap::Gray }
    }

    /// Row with the brightest pixel in column `c`.
    fn peak_row(img: &[[u8; 4]], c: usize) -> usize {
        (0..TILE_ROWS).max_by_key(|&r| img[r * TILE_COLS + c][0]).unwrap()
    }

    #[test]
    fn sine_lands_on_the_right_row() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 48_000u32;
        let f = 3000.0f32;
        let data: Vec<f32> =
            (0..rate as usize * 2).map(|i| (i as f32 / rate as f32 * f * std::f32::consts::TAU).sin()).collect();
        let store = TrackStore::from_vecs(dir.path(), rate, &[data]).unwrap();
        let mut w = SpecWorker::new(1024);
        for log in [false, true] {
            let p = params(log);
            let img = w.render_tile(&store, None, &p, 0, 0);
            let row = peak_row(&img, 100);
            // Row index counts from the top.
            let y = (TILE_ROWS - 1 - row) as f32 / TILE_ROWS as f32;
            let shown = frequency_at(&p, rate, y);
            assert!((shown - f).abs() / f < 0.05, "log={log}: peak at {shown} Hz");
            // Full-scale sine is near 0 dB → near-white.
            assert!(img[row * TILE_COLS + 100][0] > 240);
        }
    }

    #[test]
    fn coarse_levels_do_not_miss_short_events() {
        // 200 ms of noise in 60 s of silence must be visible at a level
        // where one column spans ~0.7 s.
        let dir = tempfile::tempdir().unwrap();
        let rate = 48_000u32;
        let mut data = vec![0.0f32; rate as usize * 60];
        let mut seed = 1u32;
        for x in &mut data[rate as usize * 30..rate as usize * 30 + 9600] {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *x = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
        }
        let store = TrackStore::from_vecs(dir.path(), rate, &[data]).unwrap();
        let p = params(false);
        let level = 7; // hop = 256 · 128 = 32768 samples ≈ 0.68 s
        let mut w = SpecWorker::new(1024);
        let img = w.render_tile(&store, None, &p, level, 0);
        let col = (rate as usize * 30 / p.hop(level)).min(TILE_COLS - 1);
        let bright = (0..TILE_ROWS).map(|r| img[r * TILE_COLS + col][0]).max().unwrap();
        assert!(bright > 100, "event invisible at coarse level ({bright})");
        let quiet = (0..TILE_ROWS).map(|r| img[r * TILE_COLS + 5][0]).max().unwrap();
        assert!(quiet < 10);
    }

    #[test]
    fn level_selection_matches_zoom() {
        let p = params(true);
        assert_eq!(level_for(&p, 10.0), 0);
        assert_eq!(level_for(&p, 256.0), 0);
        assert_eq!(level_for(&p, 512.0), 1);
        assert_eq!(level_for(&p, 5000.0), 4);
    }

    #[test]
    fn luts_are_monotonic_in_brightness() {
        for map in [Colormap::Magma, Colormap::Viridis, Colormap::Gray] {
            let lut = colormap_lut(map);
            let lum = |c: [u8; 4]| 0.3 * f32::from(c[0]) + 0.59 * f32::from(c[1]) + 0.11 * f32::from(c[2]);
            assert!(lum(lut[255]) > lum(lut[128]) && lum(lut[128]) > lum(lut[0]), "{map:?}");
        }
    }
}
