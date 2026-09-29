//! Block renderer: tracks → gain → rate conversion → device mix.

use std::sync::Arc;

use oddech_core::gain::GainEnvelope;
use oddech_core::render::render_source_block;
use oddech_core::resample::SincInterpolator;
use oddech_core::store::TrackStore;

/// Per-track mix settings, swapped atomically by the UI.
#[derive(Debug, Clone)]
pub struct TrackMix {
    /// Result of mute/solo resolution.
    pub audible: bool,
    /// Breath attenuation; `None` if the track is not being fixed.
    pub envelope: Option<Arc<GainEnvelope>>,
}

#[derive(Debug, Clone, Default)]
pub struct MixSettings {
    pub tracks: Vec<TrackMix>,
    /// `false` = "Przed" (original), `true` = "Po" (with attenuation).
    pub apply_attenuation: bool,
}

/// What to play.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayRequest {
    /// Start position in seconds.
    pub start: f64,
    /// Stop here (seconds); `None` = end of the longest track.
    pub end: Option<f64>,
    /// When set, playback wraps from `end` back to `loop_start`.
    pub loop_start: Option<f64>,
}

impl PlayRequest {
    pub fn from(start: f64) -> Self {
        Self { start, end: None, loop_start: None }
    }
}

struct TrackSource {
    store: Arc<TrackStore>,
    /// `None` when the track rate equals the device rate.
    interp: Option<SincInterpolator>,
}

/// Renders the timeline into interleaved device frames.
pub struct Mixer {
    sources: Vec<TrackSource>,
    device_rate: u32,
    device_channels: usize,
    // Scratch buffers, reused to keep the hot path allocation-free.
    src: Vec<f32>,
    res: Vec<f32>,
    gain_scratch: Vec<f32>,
    stereo: Vec<[f32; 2]>,
}

impl Mixer {
    pub fn new(stores: Vec<Arc<TrackStore>>, device_rate: u32, device_channels: usize) -> Self {
        let sources = stores
            .into_iter()
            .map(|store| TrackSource {
                interp: (store.rate() != device_rate)
                    .then(|| SincInterpolator::new(store.rate(), device_rate)),
                store,
            })
            .collect();
        Self {
            sources,
            device_rate,
            device_channels: device_channels.max(1),
            src: Vec::new(),
            res: Vec::new(),
            gain_scratch: Vec::new(),
            stereo: Vec::new(),
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub fn device_channels(&self) -> usize {
        self.device_channels
    }

    /// Timeline length in seconds (longest track).
    pub fn duration(&self) -> f64 {
        self.sources.iter().map(|s| s.store.duration_secs()).fold(0.0, f64::max)
    }

    /// Renders `frames` device frames whose first frame is device-rate
    /// sample index `first_frame` (i.e. time `first_frame / device_rate`).
    /// Writes `frames × device_channels` interleaved samples into `out`.
    pub fn render(&mut self, mix: &MixSettings, first_frame: i64, frames: usize, out: &mut Vec<f32>) {
        self.stereo.clear();
        self.stereo.resize(frames, [0.0; 2]);
        let t0 = first_frame as f64 / f64::from(self.device_rate);

        for (i, source) in self.sources.iter().enumerate() {
            let Some(tm) = mix.tracks.get(i) else { continue };
            if !tm.audible {
                continue;
            }
            let env = if mix.apply_attenuation { tm.envelope.as_deref() } else { None };
            let n_ch = source.store.n_channels();
            for ch in 0..n_ch {
                let buf: &[f32] = match &source.interp {
                    None => {
                        // Same rate: timeline sample == source sample.
                        self.res.resize(frames, 0.0);
                        render_source_block(&source.store, ch, env, first_frame, &mut self.res, &mut self.gain_scratch);
                        &self.res
                    }
                    Some(interp) => {
                        let pos = t0 * f64::from(source.store.rate());
                        let (s, e) = interp.source_window(pos, frames);
                        self.src.resize((e - s) as usize, 0.0);
                        render_source_block(&source.store, ch, env, s, &mut self.src, &mut self.gain_scratch);
                        self.res.resize(frames, 0.0);
                        interp.process(&self.src, s, pos, &mut self.res);
                        &self.res
                    }
                };
                // Fold the track's channels to stereo: mono → centre,
                // stereo → L/R, more → even channels left, odd right.
                let (gl, gr) = match n_ch {
                    1 => (1.0, 1.0),
                    2 => if ch == 0 { (1.0, 0.0) } else { (0.0, 1.0) },
                    n => {
                        let per_side = n.div_ceil(2) as f32;
                        if ch % 2 == 0 { (1.0 / per_side, 0.0) } else { (0.0, 1.0 / per_side) }
                    }
                };
                for (f, x) in self.stereo.iter_mut().zip(buf) {
                    f[0] += x * gl;
                    f[1] += x * gr;
                }
            }
        }

        out.clear();
        out.reserve(frames * self.device_channels);
        for [l, r] in &self.stereo {
            match self.device_channels {
                1 => out.push(0.5 * (l + r)),
                n => {
                    out.push(*l);
                    out.push(*r);
                    out.extend(std::iter::repeat_n(0.0, n - 2));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oddech_core::gain::DuckParams;
    use oddech_core::{Region, RegionId, RegionKind, RegionSource, TrackId};

    fn store(dir: &std::path::Path, rate: u32, data: Vec<Vec<f32>>) -> Arc<TrackStore> {
        Arc::new(TrackStore::from_vecs(dir, rate, &data).unwrap())
    }

    #[test]
    fn mono_track_goes_to_both_channels_and_mute_works() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), 48_000, vec![vec![0.5; 1000]]);
        let mut m = Mixer::new(vec![s], 48_000, 2);
        let mut mix = MixSettings {
            tracks: vec![TrackMix { audible: true, envelope: None }],
            apply_attenuation: true,
        };
        let mut out = Vec::new();
        m.render(&mix, 10, 4, &mut out);
        assert_eq!(out, vec![0.5; 8]);
        mix.tracks[0].audible = false;
        m.render(&mix, 10, 4, &mut out);
        assert_eq!(out, vec![0.0; 8]);
        // Past the end: silence.
        mix.tracks[0].audible = true;
        m.render(&mix, 999, 3, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn attenuation_follows_ab_switch() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), 1000, vec![vec![1.0; 3000]]);
        let region = Region {
            id: RegionId(1),
            track: TrackId(0),
            start: 1.0,
            end: 2.0,
            kind: RegionKind::Breath,
            source: RegionSource::User,
            confidence: None,
            gain_db: Some(-20.0),
            fade_ms: Some(0.0),
            silence_db: Vec::new(),
        };
        let env = GainEnvelope::build(&[region], &DuckParams::default(), &oddech_core::region::Gate { threshold: 0.5, discrimination: Default::default() }, 1000);
        let mut m = Mixer::new(vec![s], 1000, 1);
        let mut mix = MixSettings {
            tracks: vec![TrackMix { audible: true, envelope: Some(Arc::new(env)) }],
            apply_attenuation: true,
        };
        let mut out = Vec::new();
        m.render(&mix, 1500, 1, &mut out);
        assert!((out[0] - 0.1).abs() < 1e-6);
        mix.apply_attenuation = false;
        m.render(&mix, 1500, 1, &mut out);
        assert_eq!(out[0], 1.0);
    }

    #[test]
    fn resampled_track_keeps_its_timing() {
        // A click at exactly 1.0 s in a 44.1 kHz track must land at 1.0 s
        // (sample 48000) in the 48 kHz output.
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![0.0f32; 88_200];
        data[44_100] = 1.0;
        let s = store(dir.path(), 44_100, vec![data]);
        let mut m = Mixer::new(vec![s], 48_000, 1);
        let mix = MixSettings {
            tracks: vec![TrackMix { audible: true, envelope: None }],
            apply_attenuation: false,
        };
        let mut out = Vec::new();
        m.render(&mix, 47_900, 200, &mut out);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert_eq!(47_900 + peak as i64, 48_000);
    }
}
